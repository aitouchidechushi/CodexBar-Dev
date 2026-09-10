use crate::storage::{StoreError, StoreFailure, StoreFailureKind, StoreKind, StoreOperation};
use serde::Serialize;
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protection {
    Plaintext,
    CurrentUserDpapi,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteReceipt {
    pub sha256: String,
    pub bytes_written: u64,
    pub backup_created: bool,
}

/// Injectable filesystem surface used to prove every transaction failure
/// point without touching a real credential store.
pub trait AtomicFileOps: Send + Sync {
    fn write_new(&self, path: &Path, bytes: &[u8]) -> io::Result<()>;
    fn sync_file(&self, path: &Path) -> io::Result<()>;
    fn copy_new(&self, source: &Path, destination: &Path) -> io::Result<()>;
    fn replace(&self, source: &Path, destination: &Path) -> io::Result<()>;
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;
    fn remove_if_exists(&self, path: &Path) -> io::Result<()>;
    fn sync_parent(&self, path: &Path) -> io::Result<()>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct RealAtomicFileOps;

impl AtomicFileOps for RealAtomicFileOps {
    fn write_new(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
        file.write_all(bytes)
    }

    fn sync_file(&self, path: &Path) -> io::Result<()> {
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)?
            .sync_all()
    }

    fn copy_new(&self, source: &Path, destination: &Path) -> io::Result<()> {
        let mut source = File::open(source)?;
        let mut destination = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination)?;
        io::copy(&mut source, &mut destination)?;
        destination.flush()
    }

    fn replace(&self, source: &Path, destination: &Path) -> io::Result<()> {
        atomic_replace(source, destination)
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        File::open(path)?.read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    fn remove_if_exists(&self, path: &Path) -> io::Result<()> {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(source),
        }
    }

    #[cfg(windows)]
    fn sync_parent(&self, _path: &Path) -> io::Result<()> {
        // MoveFileExW with MOVEFILE_WRITE_THROUGH flushes the move before it
        // returns. Opening a directory for sync through std::fs is not
        // supported on Windows.
        Ok(())
    }

    #[cfg(not(windows))]
    fn sync_parent(&self, path: &Path) -> io::Result<()> {
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "target has no parent"))?;
        File::open(parent)?.sync_all()
    }
}

pub fn backup_path_for(target: &Path) -> PathBuf {
    adjacent_path_with_suffix(target, ".bak")
}

pub fn write_verified<T>(
    target: &Path,
    value: &T,
    protection: Protection,
    ops: &dyn AtomicFileOps,
) -> Result<WriteReceipt, StoreError>
where
    T: Serialize + DeserializeOwned + PartialEq,
{
    write_verified_for(StoreKind::Unspecified, target, value, protection, ops)
}

pub fn write_verified_for<T>(
    store: StoreKind,
    target: &Path,
    value: &T,
    protection: Protection,
    ops: &dyn AtomicFileOps,
) -> Result<WriteReceipt, StoreError>
where
    T: Serialize + DeserializeOwned + PartialEq,
{
    let serialized = serde_json::to_vec(value).map_err(|_| StoreError::Io {
        store,
        operation: StoreOperation::Serialize,
        reason: StoreFailure::new(StoreFailureKind::InvalidData, None),
    })?;
    let protected = match protection {
        Protection::Plaintext => serialized,
        Protection::CurrentUserDpapi => {
            return Err(StoreError::Io {
                store,
                operation: StoreOperation::Protect,
                reason: StoreFailure::new(StoreFailureKind::UnsupportedProtection, None),
            });
        }
    };

    write_bytes_transaction(store, target, &protected, ops, |read_back| {
        let parsed =
            serde_json::from_slice::<T>(read_back).map_err(|_| verification_error(store))?;
        if parsed != *value {
            return Err(verification_error(store));
        }
        Ok(())
    })
}

/// Atomically writes already-protected bytes and verifies the exact bytes read
/// back from the formal target. Secret protection must happen before calling
/// this function so a protection failure cannot create a partial file.
pub fn write_bytes_verified_for(
    store: StoreKind,
    target: &Path,
    protected: &[u8],
    ops: &dyn AtomicFileOps,
) -> Result<WriteReceipt, StoreError> {
    write_bytes_transaction(store, target, protected, ops, |_| Ok(()))
}

pub fn write_bytes_verified(
    target: &Path,
    protected: &[u8],
    ops: &dyn AtomicFileOps,
) -> Result<WriteReceipt, StoreError> {
    write_bytes_verified_for(StoreKind::Unspecified, target, protected, ops)
}

fn write_bytes_transaction(
    store: StoreKind,
    target: &Path,
    protected: &[u8],
    ops: &dyn AtomicFileOps,
    verify_logical_value: impl FnOnce(&[u8]) -> Result<(), StoreError>,
) -> Result<WriteReceipt, StoreError> {
    let transaction_id = uuid::Uuid::new_v4();
    let temporary = adjacent_path_with_suffix(target, &format!(".{transaction_id}.tmp"));
    let backup_temporary = adjacent_path_with_suffix(target, &format!(".{transaction_id}.bak.tmp"));
    let backup = backup_path_for(target);
    let backup_created = target.exists();
    let expected_hash = hash_bytes(protected);

    let result = (|| {
        map_io(
            store,
            StoreOperation::WriteTemporary,
            ops.write_new(&temporary, protected),
        )?;
        map_io(
            store,
            StoreOperation::SyncTemporary,
            ops.sync_file(&temporary),
        )?;

        if backup_created {
            map_io(
                store,
                StoreOperation::Backup,
                ops.copy_new(target, &backup_temporary),
            )?;
            map_io(
                store,
                StoreOperation::Backup,
                ops.sync_file(&backup_temporary),
            )?;
            map_io(
                store,
                StoreOperation::Backup,
                ops.replace(&backup_temporary, &backup),
            )?;
            map_io(store, StoreOperation::SyncParent, ops.sync_parent(target))?;
        }

        map_io(
            store,
            StoreOperation::Replace,
            ops.replace(&temporary, target),
        )?;
        map_io(store, StoreOperation::SyncParent, ops.sync_parent(target))?;

        let read_back = map_io(store, StoreOperation::ReadBack, ops.read(target))?;
        if hash_bytes(&read_back) != expected_hash {
            return Err(verification_error(store));
        }
        verify_logical_value(&read_back)?;

        Ok(WriteReceipt {
            sha256: expected_hash,
            bytes_written: read_back.len() as u64,
            backup_created,
        })
    })();

    // Cleanup is best-effort after the primary result has been decided. These
    // names can never be loaded as the formal store, even if a process is
    // terminated before cleanup completes.
    let _ = ops.remove_if_exists(&temporary);
    let _ = ops.remove_if_exists(&backup_temporary);
    result
}

fn map_io<T>(
    store: StoreKind,
    operation: StoreOperation,
    result: io::Result<T>,
) -> Result<T, StoreError> {
    result.map_err(|source| StoreError::Io {
        store,
        operation,
        reason: StoreFailure::from_io(&source),
    })
}

fn verification_error(store: StoreKind) -> StoreError {
    StoreError::Io {
        store,
        operation: StoreOperation::Verify,
        reason: StoreFailure::new(StoreFailureKind::InvalidData, None),
    }
}

fn hash_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded
}

fn adjacent_path_with_suffix(target: &Path, suffix: &str) -> PathBuf {
    let mut path = target.as_os_str().to_os_string();
    path.push(suffix);
    path.into()
}

#[cfg(not(windows))]
fn atomic_replace(source: &Path, destination: &Path) -> io::Result<()> {
    std::fs::rename(source, destination)
}

#[cfg(windows)]
fn atomic_replace(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    use windows::core::PCWSTR;

    let source = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    unsafe {
        MoveFileExW(
            PCWSTR(source.as_ptr()),
            PCWSTR(destination.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    }
    .map_err(|_| io::Error::last_os_error())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{StoreFailureKind, StoreKind, StoreOperation};
    use serde::{Deserialize, Serialize};
    use std::fs;
    use std::io;
    use std::path::Path;
    use std::sync::Mutex;

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct Fixture {
        generation: u64,
        marker: String,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum FailurePoint {
        WriteTemporary,
        SyncTemporary,
        Backup,
        Replace,
        ReadBack,
    }

    struct FailingOps {
        real: RealAtomicFileOps,
        target: std::path::PathBuf,
        point: FailurePoint,
        sync_calls: Mutex<usize>,
    }

    impl FailingOps {
        fn new(target: &Path, point: FailurePoint) -> Self {
            Self {
                real: RealAtomicFileOps,
                target: target.to_path_buf(),
                point,
                sync_calls: Mutex::new(0),
            }
        }

        fn injected_error() -> io::Error {
            io::Error::new(io::ErrorKind::PermissionDenied, "injected fixture failure")
        }
    }

    impl AtomicFileOps for FailingOps {
        fn write_new(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
            if self.point == FailurePoint::WriteTemporary {
                return Err(Self::injected_error());
            }
            self.real.write_new(path, bytes)
        }

        fn sync_file(&self, path: &Path) -> io::Result<()> {
            let mut calls = self.sync_calls.lock().unwrap();
            *calls += 1;
            if self.point == FailurePoint::SyncTemporary && *calls == 1 {
                return Err(Self::injected_error());
            }
            self.real.sync_file(path)
        }

        fn copy_new(&self, source: &Path, destination: &Path) -> io::Result<()> {
            if self.point == FailurePoint::Backup {
                return Err(Self::injected_error());
            }
            self.real.copy_new(source, destination)
        }

        fn replace(&self, source: &Path, destination: &Path) -> io::Result<()> {
            if self.point == FailurePoint::Replace && destination == self.target {
                return Err(Self::injected_error());
            }
            self.real.replace(source, destination)
        }

        fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
            if self.point == FailurePoint::ReadBack && path == self.target {
                return Err(Self::injected_error());
            }
            self.real.read(path)
        }

        fn remove_if_exists(&self, path: &Path) -> io::Result<()> {
            self.real.remove_if_exists(path)
        }

        fn sync_parent(&self, path: &Path) -> io::Result<()> {
            self.real.sync_parent(path)
        }
    }

    fn fixture(generation: u64) -> Fixture {
        Fixture {
            generation,
            marker: format!("fixture-{generation}"),
        }
    }

    fn read_fixture(path: &Path) -> Fixture {
        serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
    }

    fn assert_old_value_recoverable(target: &Path, old: &Fixture) {
        let target_value = read_fixture(target);
        let backup = backup_path_for(target);
        let backup_value = backup.exists().then(|| read_fixture(&backup));
        assert!(
            target_value == *old || backup_value.as_ref() == Some(old),
            "neither target nor backup retained the old complete value"
        );

        let temporary_files = fs::read_dir(target.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp"))
            .collect::<Vec<_>>();
        assert!(
            temporary_files.is_empty(),
            "temporary files were not cleaned up"
        );
    }

    #[test]
    fn verified_write_replaces_whole_value_and_keeps_previous_backup() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("api-keys.v2.json");
        let old = fixture(1);
        let new = fixture(2);
        fs::write(&target, serde_json::to_vec(&old).unwrap()).unwrap();

        let receipt = write_verified_for(
            StoreKind::ApiKeys,
            &target,
            &new,
            Protection::Plaintext,
            &RealAtomicFileOps,
        )
        .unwrap();

        assert_eq!(read_fixture(&target), new);
        assert_eq!(read_fixture(&backup_path_for(&target)), old);
        assert!(receipt.backup_created);
        assert_eq!(receipt.bytes_written, fs::metadata(&target).unwrap().len());
        assert_eq!(receipt.sha256.len(), 64);
    }

    #[test]
    fn each_injected_failure_retains_a_complete_old_copy() {
        for point in [
            FailurePoint::WriteTemporary,
            FailurePoint::SyncTemporary,
            FailurePoint::Backup,
            FailurePoint::Replace,
            FailurePoint::ReadBack,
        ] {
            let directory = tempfile::tempdir().unwrap();
            let target = directory.path().join("api-keys.v2.json");
            let old = fixture(10);
            let new = fixture(11);
            fs::write(&target, serde_json::to_vec(&old).unwrap()).unwrap();
            let ops = FailingOps::new(&target, point);

            let error = write_verified_for(
                StoreKind::ApiKeys,
                &target,
                &new,
                Protection::Plaintext,
                &ops,
            )
            .unwrap_err();

            assert!(matches!(
                error,
                crate::storage::StoreError::Io {
                    store: StoreKind::ApiKeys,
                    operation: StoreOperation::WriteTemporary
                        | StoreOperation::SyncTemporary
                        | StoreOperation::Backup
                        | StoreOperation::Replace
                        | StoreOperation::ReadBack,
                    reason
                } if reason.kind == StoreFailureKind::PermissionDenied
            ));
            assert_old_value_recoverable(&target, &old);
        }
    }

    #[test]
    fn unsupported_protection_fails_before_creating_any_file() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("api-keys.v2.json");

        let error = write_verified_for(
            StoreKind::ApiKeys,
            &target,
            &fixture(1),
            Protection::CurrentUserDpapi,
            &RealAtomicFileOps,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            crate::storage::StoreError::Io {
                store: StoreKind::ApiKeys,
                operation: StoreOperation::Protect,
                reason
            } if reason.kind == StoreFailureKind::UnsupportedProtection
        ));
        assert!(!target.exists());
        assert!(!backup_path_for(&target).exists());
    }
}
