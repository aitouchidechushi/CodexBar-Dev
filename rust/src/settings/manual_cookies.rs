use super::*;
use crate::secure_file::{
    PlatformSecretProtector, SecretProtector, decode_string_bytes_with, default_protection,
    protected_file_bytes_with, restrict_storage_file,
};
use crate::storage::{
    CrossProcessStoreLock, LoadState, MigrationCoordinator, MigrationRequest, Protection,
    RealAtomicFileOps, StoragePaths, StoreError, StoreFailure, StoreFailureKind, StoreKind,
    StoreOperation, UserScopeId, write_bytes_verified_for,
};
use serde_json::Value;
use std::fs;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const MANUAL_COOKIES_STORE_VERSION: u32 = 2;
const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(10);
const SECURE_FILE_FORMAT: &str = "codexbar.secure-file";
static MANUAL_COOKIES_MUTATION_LOCK: Mutex<()> = Mutex::new(());

/// Manual cookie storage
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ManualCookies {
    /// Provider ID -> cookie header mapping
    pub cookies: HashMap<String, ManualCookieEntry>,
}

/// A single manual cookie entry
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManualCookieEntry {
    pub cookie_header: String,
    pub saved_at: String,
}

impl ManualCookies {
    /// Get cookie for a provider
    pub fn get(&self, provider_id: &str) -> Option<&str> {
        self.cookies
            .get(provider_id)
            .map(|e| e.cookie_header.as_str())
    }

    /// Set cookie for a provider
    pub fn set(&mut self, provider_id: &str, cookie_header: &str) {
        let now = chrono::Utc::now().format("%Y-%m-%d %H:%M").to_string();
        self.cookies.insert(
            provider_id.to_string(),
            ManualCookieEntry {
                cookie_header: cookie_header.to_string(),
                saved_at: now,
            },
        );
    }

    /// Remove cookie for a provider
    pub fn remove(&mut self, provider_id: &str) {
        self.cookies.remove(provider_id);
    }

    /// Get all saved cookies for UI display
    pub fn get_all_for_display(&self) -> Vec<SavedCookieInfo> {
        self.cookies
            .iter()
            .map(|(id, entry)| {
                let provider_name = ProviderId::from_cli_name(id)
                    .map(|p| p.display_name().to_string())
                    .unwrap_or_else(|| id.clone());

                SavedCookieInfo {
                    provider_id: id.clone(),
                    provider: provider_name,
                    saved_at: entry.saved_at.clone(),
                }
            })
            .collect()
    }
}

/// Info about a saved cookie for UI display
#[derive(Debug, Clone, Serialize)]
pub struct SavedCookieInfo {
    pub provider_id: String,
    pub provider: String,
    pub saved_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManualCookiesFileV2 {
    version: u32,
    cookies: ManualCookies,
}

#[derive(Clone)]
pub struct ManualCookieRepository {
    paths: StoragePaths,
    user_scope: UserScopeId,
    protection: Protection,
    protector: Arc<dyn SecretProtector>,
    lock_timeout: Duration,
}

impl std::fmt::Debug for ManualCookieRepository {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ManualCookieRepository")
            .field("store", &StoreKind::ManualCookies)
            .field("protection", &self.protection)
            .field("lock_timeout", &self.lock_timeout)
            .finish_non_exhaustive()
    }
}

impl ManualCookieRepository {
    pub fn discover() -> Result<Self, StoreError> {
        Ok(Self::new(
            StoragePaths::discover()?,
            UserScopeId::current()?,
        ))
    }

    pub fn new(paths: StoragePaths, user_scope: UserScopeId) -> Self {
        Self::with_protection(
            paths,
            user_scope,
            default_protection(),
            Arc::new(PlatformSecretProtector),
        )
    }

    pub fn with_protection(
        paths: StoragePaths,
        user_scope: UserScopeId,
        protection: Protection,
        protector: Arc<dyn SecretProtector>,
    ) -> Self {
        Self {
            paths,
            user_scope,
            protection,
            protector,
            lock_timeout: DEFAULT_LOCK_TIMEOUT,
        }
    }

    pub fn path(&self) -> &Path {
        self.paths.manual_cookies()
    }

    pub fn load(&self) -> Result<LoadState<ManualCookies>, StoreError> {
        let _process_guard = MANUAL_COOKIES_MUTATION_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.load_or_migrate_unlocked()
    }

    pub fn mutate<R>(
        &self,
        mutation: impl FnOnce(&mut ManualCookies) -> R,
    ) -> Result<R, StoreError> {
        let _process_guard = MANUAL_COOKIES_MUTATION_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.paths.ensure_directories()?;
        self.load_or_migrate_unlocked()?
            .into_writable_for(StoreKind::ManualCookies)?;

        let _store_guard = CrossProcessStoreLock::acquire(
            StoreKind::ManualCookies,
            &self.user_scope,
            self.paths.manual_cookies(),
            self.lock_timeout,
        )?;
        let mut cookies = self
            .load_v2_unlocked()
            .into_writable_for(StoreKind::ManualCookies)?;
        let result = mutation(&mut cookies);
        validate_manual_cookies(&cookies)?;
        self.write_verified(&cookies)?;

        match self.load_v2_unlocked() {
            LoadState::Loaded(read_back) if read_back == cookies => Ok(result),
            LoadState::Loaded(_) => Err(manual_cookie_invalid_payload()),
            state => Err(manual_cookie_load_state_error(state)),
        }
    }

    fn load_or_migrate_unlocked(&self) -> Result<LoadState<ManualCookies>, StoreError> {
        self.paths.ensure_directories()?;
        if self.paths.manual_cookies().exists() {
            return Ok(self.load_v2_unlocked());
        }

        let source = self.paths.legacy_manual_cookies();
        if !source.exists() {
            return Ok(LoadState::Missing);
        }
        let raw = match fs::read(&source) {
            Ok(raw) => raw,
            Err(source) => {
                return Ok(LoadState::IoError {
                    operation: StoreOperation::Read,
                    reason: StoreFailure::from_io(&source),
                });
            }
        };
        let cookies = match self.decode_legacy_bytes(&raw) {
            Ok(cookies) => cookies,
            Err(error) => return Ok(manual_cookie_load_state_from_error(error)),
        };
        validate_manual_cookies(&cookies)?;

        let request = MigrationRequest {
            store: StoreKind::ManualCookies,
            source_path: source,
            source_version: 1,
            destination_path: self.paths.manual_cookies().to_path_buf(),
            destination_bytes: self.protected_bytes(&cookies)?,
        };
        let repository = self;
        MigrationCoordinator::new(self.paths.clone(), self.user_scope.clone())
            .migrate_bytes_verified(request, move |bytes| {
                repository.decode_v2_bytes(bytes).map(|_| ())
            })?;

        Ok(self.load_v2_unlocked())
    }

    fn load_v2_unlocked(&self) -> LoadState<ManualCookies> {
        let raw = match fs::read(self.paths.manual_cookies()) {
            Ok(raw) => raw,
            Err(source) if source.kind() == io::ErrorKind::NotFound => return LoadState::Missing,
            Err(source) => {
                return LoadState::IoError {
                    operation: StoreOperation::Read,
                    reason: StoreFailure::from_io(&source),
                };
            }
        };
        match self.decode_v2_bytes(&raw) {
            Ok(cookies) => LoadState::Loaded(cookies),
            Err(error) => manual_cookie_load_state_from_error(error),
        }
    }

    fn decode_v2_bytes(&self, raw: &[u8]) -> Result<ManualCookies, StoreError> {
        let payload = self.decode_payload(raw)?;
        let value = serde_json::from_str::<Value>(payload.trim_start_matches('\u{feff}'))
            .map_err(|_| manual_cookie_invalid_payload())?;
        let object = value
            .as_object()
            .ok_or_else(manual_cookie_invalid_payload)?;
        if object.len() != 2 || !object.contains_key("version") || !object.contains_key("cookies") {
            return Err(manual_cookie_invalid_payload());
        }
        let found = object
            .get("version")
            .and_then(Value::as_u64)
            .and_then(|version| u32::try_from(version).ok())
            .ok_or_else(manual_cookie_invalid_payload)?;
        if found != MANUAL_COOKIES_STORE_VERSION {
            return Err(StoreError::UnsupportedVersion {
                store: StoreKind::ManualCookies,
                found,
                supported: MANUAL_COOKIES_STORE_VERSION,
            });
        }
        let cookies = serde_json::from_value(
            object
                .get("cookies")
                .cloned()
                .ok_or_else(manual_cookie_invalid_payload)?,
        )
        .map_err(|_| manual_cookie_invalid_payload())?;
        validate_manual_cookies(&cookies)?;
        Ok(cookies)
    }

    fn decode_legacy_bytes(&self, raw: &[u8]) -> Result<ManualCookies, StoreError> {
        let payload = self.decode_payload(raw)?;
        serde_json::from_str(payload.trim_start_matches('\u{feff}'))
            .map_err(|_| manual_cookie_invalid_payload())
    }

    fn decode_payload(&self, raw: &[u8]) -> Result<String, StoreError> {
        let secure_envelope = manual_cookie_raw_is_secure_envelope(raw);
        let payload = decode_string_bytes_with(raw, self.protector.as_ref()).map_err(|source| {
            if secure_envelope {
                if source.kind() == io::ErrorKind::InvalidData {
                    StoreError::CorruptEnvelope {
                        store: StoreKind::ManualCookies,
                        reason: StoreFailure::from_io(&source),
                    }
                } else {
                    let reason = if source.kind() == io::ErrorKind::Unsupported {
                        StoreFailure::new(StoreFailureKind::UnsupportedProtection, None)
                    } else {
                        StoreFailure::from_io(&source)
                    };
                    StoreError::DecryptFailed {
                        store: StoreKind::ManualCookies,
                        reason,
                    }
                }
            } else {
                manual_cookie_invalid_payload()
            }
        })?;
        if secure_envelope && payload.as_bytes() == raw {
            return Err(StoreError::CorruptEnvelope {
                store: StoreKind::ManualCookies,
                reason: StoreFailure::new(StoreFailureKind::InvalidData, None),
            });
        }
        Ok(payload)
    }

    fn protected_bytes(&self, cookies: &ManualCookies) -> Result<Vec<u8>, StoreError> {
        let file = ManualCookiesFileV2 {
            version: MANUAL_COOKIES_STORE_VERSION,
            cookies: cookies.clone(),
        };
        let json = serde_json::to_string(&file).map_err(|_| StoreError::Io {
            store: StoreKind::ManualCookies,
            operation: StoreOperation::Serialize,
            reason: StoreFailure::new(StoreFailureKind::InvalidData, None),
        })?;
        protected_file_bytes_with(&json, self.protection, self.protector.as_ref()).map_err(
            |source| StoreError::Io {
                store: StoreKind::ManualCookies,
                operation: StoreOperation::Protect,
                reason: StoreFailure::from_io(&source),
            },
        )
    }

    fn write_verified(&self, cookies: &ManualCookies) -> Result<(), StoreError> {
        let protected = self.protected_bytes(cookies)?;
        write_bytes_verified_for(
            StoreKind::ManualCookies,
            self.paths.manual_cookies(),
            &protected,
            &RealAtomicFileOps,
        )?;
        restrict_storage_file(self.paths.manual_cookies()).map_err(|source| StoreError::Io {
            store: StoreKind::ManualCookies,
            operation: StoreOperation::ApplyPermissions,
            reason: StoreFailure::from_io(&source),
        })
    }
}

fn validate_manual_cookies(cookies: &ManualCookies) -> Result<(), StoreError> {
    let value = serde_json::to_value(cookies).map_err(|_| manual_cookie_invalid_payload())?;
    let round_trip: ManualCookies =
        serde_json::from_value(value).map_err(|_| manual_cookie_invalid_payload())?;
    if round_trip != *cookies {
        return Err(manual_cookie_invalid_payload());
    }
    Ok(())
}

fn manual_cookie_raw_is_secure_envelope(raw: &[u8]) -> bool {
    serde_json::from_slice::<Value>(raw)
        .ok()
        .and_then(|value| {
            value
                .get("format")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .is_some_and(|format| format == SECURE_FILE_FORMAT)
}

fn manual_cookie_load_state_from_error(error: StoreError) -> LoadState<ManualCookies> {
    match error {
        StoreError::CorruptEnvelope { reason, .. } => LoadState::CorruptEnvelope { reason },
        StoreError::DecryptFailed { reason, .. } => LoadState::DecryptFailed { reason },
        StoreError::InvalidPayload { reason, .. } => LoadState::InvalidPayload { reason },
        StoreError::UnsupportedVersion {
            found, supported, ..
        } => LoadState::UnsupportedVersion { found, supported },
        StoreError::LockTimeout { reason, .. } | StoreError::Locked { reason, .. } => {
            LoadState::Locked { reason }
        }
        StoreError::Io {
            operation, reason, ..
        } => LoadState::IoError { operation, reason },
        StoreError::MigrationRequired { .. } | StoreError::MutationRejected { .. } => {
            LoadState::InvalidPayload {
                reason: StoreFailure::new(StoreFailureKind::InvalidData, None),
            }
        }
    }
}

fn manual_cookie_load_state_error(state: LoadState<ManualCookies>) -> StoreError {
    match state {
        LoadState::Missing => StoreError::Io {
            store: StoreKind::ManualCookies,
            operation: StoreOperation::ReadBack,
            reason: StoreFailure::new(StoreFailureKind::NotFound, None),
        },
        LoadState::Loaded(_) => manual_cookie_invalid_payload(),
        LoadState::NeedsMigration { source_version, .. } => StoreError::MigrationRequired {
            store: StoreKind::ManualCookies,
            source_version,
        },
        LoadState::CorruptEnvelope { reason } => StoreError::CorruptEnvelope {
            store: StoreKind::ManualCookies,
            reason,
        },
        LoadState::DecryptFailed { reason } => StoreError::DecryptFailed {
            store: StoreKind::ManualCookies,
            reason,
        },
        LoadState::InvalidPayload { reason } => StoreError::InvalidPayload {
            store: StoreKind::ManualCookies,
            reason,
        },
        LoadState::UnsupportedVersion { found, supported } => StoreError::UnsupportedVersion {
            store: StoreKind::ManualCookies,
            found,
            supported,
        },
        LoadState::Locked { reason } => StoreError::Locked {
            store: StoreKind::ManualCookies,
            reason,
        },
        LoadState::IoError { operation, reason } => StoreError::Io {
            store: StoreKind::ManualCookies,
            operation,
            reason,
        },
    }
}

fn manual_cookie_invalid_payload() -> StoreError {
    StoreError::InvalidPayload {
        store: StoreKind::ManualCookies,
        reason: StoreFailure::new(StoreFailureKind::InvalidData, None),
    }
}

#[cfg(test)]
mod repository_tests {
    use super::*;
    use crate::secure_file::PlatformSecretProtector;
    use crate::storage::{LoadState, Protection, StoragePaths, UserScopeId};
    use std::fs;
    use std::sync::{Arc, Barrier};

    fn fixture() -> (PathBuf, StoragePaths, ManualCookieRepository) {
        let root = std::env::temp_dir().join(format!(
            "codexbar-manual-cookie-repository-test-{}",
            uuid::Uuid::new_v4()
        ));
        let paths = StoragePaths::from_roots(root.join("roaming"), root.join("local"));
        let repository = ManualCookieRepository::with_protection(
            paths.clone(),
            UserScopeId::from_stable_identifier(b"manual-cookie-test-user"),
            Protection::Plaintext,
            Arc::new(PlatformSecretProtector),
        );
        (root, paths, repository)
    }

    fn cleanup(root: &std::path::Path) {
        if root.exists() {
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn legacy_manual_cookies_migrate_without_losing_provider_values() {
        let (root, paths, repository) = fixture();
        let legacy = paths.legacy_manual_cookies();
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        let mut expected = ManualCookies::default();
        expected.set("claude", "sessionKey=fixture-claude");
        expected.set("ollama", "__Secure-session=fixture-ollama");
        fs::write(&legacy, serde_json::to_vec_pretty(&expected).unwrap()).unwrap();

        assert_eq!(
            repository.load().unwrap(),
            LoadState::Loaded(expected.clone())
        );
        assert!(paths.manual_cookies().exists());
        assert!(legacy.exists());

        let mut later_legacy_write = ManualCookies::default();
        later_legacy_write.set("claude", "sessionKey=stale-old-build");
        fs::write(
            &legacy,
            serde_json::to_vec_pretty(&later_legacy_write).unwrap(),
        )
        .unwrap();
        assert_eq!(repository.load().unwrap(), LoadState::Loaded(expected));

        cleanup(&root);
    }

    #[test]
    fn corrupt_manual_cookie_store_is_not_empty_and_cannot_be_overwritten() {
        let (root, paths, repository) = fixture();
        fs::create_dir_all(paths.manual_cookies().parent().unwrap()).unwrap();
        let corrupt = b"{not-valid-json";
        fs::write(paths.manual_cookies(), corrupt).unwrap();

        assert!(matches!(
            repository.load().unwrap(),
            LoadState::InvalidPayload { .. }
        ));
        assert!(
            repository
                .mutate(|cookies| cookies.set("claude", "sessionKey=replacement"))
                .is_err()
        );
        assert_eq!(fs::read(paths.manual_cookies()).unwrap(), corrupt);

        cleanup(&root);
    }

    #[test]
    fn concurrent_manual_cookie_transactions_preserve_both_providers() {
        let (root, _paths, repository) = fixture();
        let barrier = Arc::new(Barrier::new(2));
        let first_repository = repository.clone();
        let first_barrier = Arc::clone(&barrier);
        let first = std::thread::spawn(move || {
            first_barrier.wait();
            first_repository
                .mutate(|cookies| cookies.set("claude", "sessionKey=fixture-claude"))
                .unwrap();
        });
        let second_repository = repository.clone();
        let second_barrier = Arc::clone(&barrier);
        let second = std::thread::spawn(move || {
            second_barrier.wait();
            second_repository
                .mutate(|cookies| cookies.set("cursor", "WorkosCursorSessionToken=fixture-cursor"))
                .unwrap();
        });

        first.join().unwrap();
        second.join().unwrap();
        let LoadState::Loaded(cookies) = repository.load().unwrap() else {
            panic!("concurrent transactions must leave a readable store")
        };
        assert_eq!(cookies.get("claude"), Some("sessionKey=fixture-claude"));
        assert_eq!(
            cookies.get("cursor"),
            Some("WorkosCursorSessionToken=fixture-cursor")
        );

        cleanup(&root);
    }

    #[test]
    fn future_manual_cookie_version_cannot_be_overwritten_by_this_build() {
        let (root, paths, repository) = fixture();
        fs::create_dir_all(paths.manual_cookies().parent().unwrap()).unwrap();
        let future = br#"{"version":99,"cookies":{"cookies":{}}}"#;
        fs::write(paths.manual_cookies(), future).unwrap();

        assert!(matches!(
            repository.load().unwrap(),
            LoadState::UnsupportedVersion {
                found: 99,
                supported: MANUAL_COOKIES_STORE_VERSION
            }
        ));
        assert!(
            repository
                .mutate(|cookies| cookies.set("claude", "sessionKey=replacement"))
                .is_err()
        );
        assert_eq!(fs::read(paths.manual_cookies()).unwrap(), future);

        cleanup(&root);
    }
}
