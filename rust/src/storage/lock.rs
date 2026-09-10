use crate::storage::{StoreError, StoreFailure, StoreFailureKind, StoreKind, StoreOperation};
use sha2::{Digest, Sha256};
use std::fmt;
use std::marker::PhantomData;
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

/// A one-way hash of a stable operating-system user identifier.
///
/// The raw SID (or platform equivalent) is never retained, formatted, or used
/// directly in the named lock.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct UserScopeId([u8; 32]);

impl UserScopeId {
    pub fn current() -> Result<Self, StoreError> {
        platform_user_identifier()
            .map(|identifier| Self::from_stable_identifier(&identifier))
            .map_err(|source| StoreError::Io {
                store: StoreKind::Unspecified,
                operation: StoreOperation::Lock,
                reason: StoreFailure::from_io(&source),
            })
    }

    pub fn from_stable_identifier(identifier: &[u8]) -> Self {
        let digest = Sha256::digest(identifier);
        let mut value = [0_u8; 32];
        value.copy_from_slice(&digest);
        Self(value)
    }

    fn encoded(&self) -> String {
        let mut encoded = String::with_capacity(self.0.len() * 2);
        for byte in self.0 {
            use std::fmt::Write as _;
            write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
        }
        encoded
    }
}

impl fmt::Debug for UserScopeId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("UserScopeId")
            .field(&self.encoded())
            .finish()
    }
}

/// An OS-backed lock shared by every CodexBar process for one user and store.
///
/// The guard must be dropped on the thread that acquired it. The `Rc` marker
/// enforces that constraint because Windows mutex ownership is thread-bound.
pub struct CrossProcessStoreLock {
    store: StoreKind,
    #[cfg(windows)]
    handle: windows::Win32::Foundation::HANDLE,
    #[cfg(windows)]
    abandoned: bool,
    #[cfg(not(windows))]
    file: std::fs::File,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

impl CrossProcessStoreLock {
    pub fn acquire(
        store: StoreKind,
        user_scope: &UserScopeId,
        target: &Path,
        timeout: Duration,
    ) -> Result<Self, StoreError> {
        platform_acquire(store, user_scope, target, timeout)
    }

    #[cfg(windows)]
    pub const fn was_abandoned(&self) -> bool {
        self.abandoned
    }

    #[cfg(not(windows))]
    pub const fn was_abandoned(&self) -> bool {
        false
    }
}

impl fmt::Debug for CrossProcessStoreLock {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CrossProcessStoreLock")
            .field("store", &self.store)
            .field("was_abandoned", &self.was_abandoned())
            .finish_non_exhaustive()
    }
}

#[cfg(windows)]
fn windows_mutex_name(store: StoreKind, user_scope: &UserScopeId) -> String {
    format!(
        "Local\\CodexBar.StorageV2.{}.{}",
        user_scope.encoded(),
        store.as_str()
    )
}

#[cfg(windows)]
fn platform_acquire(
    store: StoreKind,
    user_scope: &UserScopeId,
    _target: &Path,
    timeout: Duration,
) -> Result<CrossProcessStoreLock, StoreError> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Foundation::{
        CloseHandle, WAIT_ABANDONED, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
    };
    use windows::Win32::System::Threading::{CreateMutexW, WaitForSingleObject};
    use windows::core::PCWSTR;

    let name = std::ffi::OsStr::new(&windows_mutex_name(store, user_scope))
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let handle = unsafe { CreateMutexW(None, false, PCWSTR(name.as_ptr())) }.map_err(|_| {
        StoreError::Io {
            store,
            operation: StoreOperation::Lock,
            reason: StoreFailure::from_io(&std::io::Error::last_os_error()),
        }
    })?;
    let milliseconds = timeout.as_millis().min(u128::from(u32::MAX - 1)) as u32;
    let wait_result = unsafe { WaitForSingleObject(handle, milliseconds) };

    if wait_result == WAIT_OBJECT_0 || wait_result == WAIT_ABANDONED {
        return Ok(CrossProcessStoreLock {
            store,
            handle,
            abandoned: wait_result == WAIT_ABANDONED,
            _not_send_or_sync: PhantomData,
        });
    }

    let wait_error = if wait_result == WAIT_TIMEOUT {
        StoreError::LockTimeout {
            store,
            reason: StoreFailure::new(StoreFailureKind::TimedOut, None),
        }
    } else if wait_result == WAIT_FAILED {
        StoreError::Io {
            store,
            operation: StoreOperation::Lock,
            reason: StoreFailure::from_io(&std::io::Error::last_os_error()),
        }
    } else {
        StoreError::Io {
            store,
            operation: StoreOperation::Lock,
            reason: StoreFailure::new(StoreFailureKind::Other, None),
        }
    };
    unsafe {
        let _ = CloseHandle(handle);
    }
    Err(wait_error)
}

#[cfg(windows)]
impl Drop for CrossProcessStoreLock {
    fn drop(&mut self) {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::ReleaseMutex;

        unsafe {
            let _ = ReleaseMutex(self.handle);
            let _ = CloseHandle(self.handle);
        }
    }
}

#[cfg(windows)]
fn platform_user_identifier() -> std::io::Result<Vec<u8>> {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{
        GetLengthSid, GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    struct TokenHandle(HANDLE);

    impl Drop for TokenHandle {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)
            .map_err(|_| std::io::Error::last_os_error())?;
        let token = TokenHandle(token);

        let mut required = 0_u32;
        let _ = GetTokenInformation(token.0, TokenUser, None, 0, &mut required);
        if required == 0 {
            return Err(std::io::Error::last_os_error());
        }

        let mut buffer = vec![0_u8; required as usize];
        GetTokenInformation(
            token.0,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            required,
            &mut required,
        )
        .map_err(|_| std::io::Error::last_os_error())?;

        let token_user = std::ptr::read_unaligned(buffer.as_ptr().cast::<TOKEN_USER>());
        let sid_length = GetLengthSid(token_user.User.Sid);
        if sid_length == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(
            std::slice::from_raw_parts(token_user.User.Sid.0.cast::<u8>(), sid_length as usize)
                .to_vec(),
        )
    }
}

#[cfg(not(windows))]
fn platform_acquire(
    store: StoreKind,
    _user_scope: &UserScopeId,
    target: &Path,
    timeout: Duration,
) -> Result<CrossProcessStoreLock, StoreError> {
    use fs2::FileExt;
    use std::fs::OpenOptions;
    use std::time::Instant;

    let lock_path = adjacent_path_with_suffix(target, ".lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(&lock_path)
        .map_err(|source| StoreError::Io {
            store,
            operation: StoreOperation::Lock,
            reason: StoreFailure::from_io(&source),
        })?;
    let started = Instant::now();

    loop {
        match file.try_lock_exclusive() {
            Ok(()) => {
                return Ok(CrossProcessStoreLock {
                    store,
                    file,
                    _not_send_or_sync: PhantomData,
                });
            }
            Err(source) if source.kind() == std::io::ErrorKind::WouldBlock => {
                if started.elapsed() >= timeout {
                    return Err(StoreError::LockTimeout {
                        store,
                        reason: StoreFailure::new(StoreFailureKind::TimedOut, None),
                    });
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(source) => {
                return Err(StoreError::Io {
                    store,
                    operation: StoreOperation::Lock,
                    reason: StoreFailure::from_io(&source),
                });
            }
        }
    }
}

#[cfg(not(windows))]
impl Drop for CrossProcessStoreLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

#[cfg(not(windows))]
fn adjacent_path_with_suffix(target: &Path, suffix: &str) -> std::path::PathBuf {
    let mut path = target.as_os_str().to_os_string();
    path.push(suffix);
    path.into()
}

#[cfg(not(windows))]
fn platform_user_identifier() -> std::io::Result<Vec<u8>> {
    let user = std::env::var_os("USER").unwrap_or_default();
    let home = std::env::var_os("HOME").unwrap_or_default();
    if user.is_empty() && home.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "platform user identifier unavailable",
        ));
    }

    let mut identifier = user.as_encoded_bytes().to_vec();
    identifier.push(0);
    identifier.extend_from_slice(home.as_encoded_bytes());
    Ok(identifier)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{StoreError, StoreFailureKind, StoreKind};
    use std::fs;
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    const WORKER_TARGET_ENV: &str = "CODEXBAR_STORAGE_LOCK_WORKER_TARGET";
    const WORKER_SCOPE_ENV: &str = "CODEXBAR_STORAGE_LOCK_WORKER_SCOPE";
    const WORKER_ITERATIONS_ENV: &str = "CODEXBAR_STORAGE_LOCK_WORKER_ITERATIONS";

    fn test_scope(seed: &str) -> UserScopeId {
        UserScopeId::from_stable_identifier(seed.as_bytes())
    }

    #[test]
    fn second_writer_waits_until_the_first_writer_releases() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("api-keys.json");
        let scope = test_scope("waits-for-first-writer");
        let (held_sender, held_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();

        let first_target = target.clone();
        let first_scope = scope.clone();
        let first = thread::spawn(move || {
            let guard = CrossProcessStoreLock::acquire(
                StoreKind::ApiKeys,
                &first_scope,
                &first_target,
                Duration::from_secs(2),
            )
            .unwrap();
            held_sender.send(()).unwrap();
            release_receiver.recv().unwrap();
            drop(guard);
        });

        held_receiver.recv_timeout(Duration::from_secs(2)).unwrap();
        let releaser = thread::spawn(move || {
            thread::sleep(Duration::from_millis(150));
            release_sender.send(()).unwrap();
        });

        let started = Instant::now();
        let second = CrossProcessStoreLock::acquire(
            StoreKind::ApiKeys,
            &scope,
            &target,
            Duration::from_secs(2),
        )
        .unwrap();
        let waited = started.elapsed();

        assert!(
            waited >= Duration::from_millis(100),
            "second writer entered after only {waited:?}"
        );
        drop(second);
        releaser.join().unwrap();
        first.join().unwrap();
    }

    #[test]
    fn timeout_does_not_release_or_delete_another_writer_lock() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("api-keys.json");
        let scope = test_scope("timeout-keeps-owner-lock");
        let (held_sender, held_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();

        let owner_target = target.clone();
        let owner_scope = scope.clone();
        let owner = thread::spawn(move || {
            let guard = CrossProcessStoreLock::acquire(
                StoreKind::ApiKeys,
                &owner_scope,
                &owner_target,
                Duration::from_secs(2),
            )
            .unwrap();
            held_sender.send(()).unwrap();
            release_receiver.recv().unwrap();
            drop(guard);
        });

        held_receiver.recv_timeout(Duration::from_secs(2)).unwrap();
        let first_timeout = CrossProcessStoreLock::acquire(
            StoreKind::ApiKeys,
            &scope,
            &target,
            Duration::from_millis(40),
        )
        .unwrap_err();
        let second_timeout = CrossProcessStoreLock::acquire(
            StoreKind::ApiKeys,
            &scope,
            &target,
            Duration::from_millis(40),
        )
        .unwrap_err();

        for error in [first_timeout, second_timeout] {
            assert!(matches!(
                error,
                StoreError::LockTimeout {
                    store: StoreKind::ApiKeys,
                    reason
                } if reason.kind == StoreFailureKind::TimedOut
            ));
        }

        release_sender.send(()).unwrap();
        owner.join().unwrap();
        CrossProcessStoreLock::acquire(StoreKind::ApiKeys, &scope, &target, Duration::from_secs(1))
            .unwrap();
    }

    #[test]
    fn independent_processes_do_not_lose_counter_updates() {
        if std::env::var_os(WORKER_TARGET_ENV).is_some() {
            return;
        }

        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("counter.txt");
        fs::write(&target, b"0").unwrap();
        let scope_seed = format!("counter-{}", uuid::Uuid::new_v4());
        let process_count = 4_u64;
        let iterations = 12_u64;
        let executable = std::env::current_exe().unwrap();
        let mut children = Vec::new();

        for _ in 0..process_count {
            let child = Command::new(&executable)
                .arg("--exact")
                .arg("storage::lock::tests::cross_process_counter_worker")
                .arg("--nocapture")
                .env(WORKER_TARGET_ENV, &target)
                .env(WORKER_SCOPE_ENV, &scope_seed)
                .env(WORKER_ITERATIONS_ENV, iterations.to_string())
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            children.push(child);
        }

        for child in children {
            let output = child.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "counter worker failed: status={:?}, stdout={}, stderr={}",
                output.status.code(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let actual = fs::read_to_string(&target)
            .unwrap()
            .trim()
            .parse::<u64>()
            .unwrap();
        assert_eq!(actual, process_count * iterations);
    }

    #[test]
    fn cross_process_counter_worker() {
        let Some(target) = std::env::var_os(WORKER_TARGET_ENV) else {
            return;
        };
        let scope_seed = std::env::var(WORKER_SCOPE_ENV).unwrap();
        let iterations = std::env::var(WORKER_ITERATIONS_ENV)
            .unwrap()
            .parse::<u64>()
            .unwrap();
        let target = std::path::PathBuf::from(target);
        let scope = test_scope(&scope_seed);

        for _ in 0..iterations {
            let guard = CrossProcessStoreLock::acquire(
                StoreKind::ApiKeys,
                &scope,
                &target,
                Duration::from_secs(5),
            )
            .unwrap();
            let current = fs::read_to_string(&target)
                .unwrap()
                .trim()
                .parse::<u64>()
                .unwrap();
            thread::sleep(Duration::from_millis(2));
            fs::write(&target, (current + 1).to_string()).unwrap();
            drop(guard);
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_mutex_name_contains_only_the_hashed_user_scope() {
        let raw_scope = "S-1-5-21-fixture-sensitive-sid";
        let scope = test_scope(raw_scope);
        let name = windows_mutex_name(StoreKind::ApiKeys, &scope);

        assert!(name.starts_with("Local\\CodexBar.StorageV2."));
        assert!(name.ends_with(".api-keys"));
        assert!(!name.contains(raw_scope));
    }

    #[cfg(windows)]
    #[test]
    fn current_windows_user_scope_is_stable_without_retaining_the_sid() {
        let first = UserScopeId::current().unwrap();
        let second = UserScopeId::current().unwrap();

        assert_eq!(first, second);
        assert_eq!(first.encoded().len(), 64);
    }
}
