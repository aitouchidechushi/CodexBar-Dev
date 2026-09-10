use super::HooksConfig;
use crate::secure_file::{
    PlatformSecretProtector, SecretProtector, decode_string_bytes_with, default_protection,
    restrict_storage_file,
};
use crate::storage::{
    CrossProcessStoreLock, LoadState, Protection, RealAtomicFileOps, StoragePaths, StoreError,
    StoreFailure, StoreFailureKind, StoreKind, StoreOperation, UserScopeId,
    write_bytes_verified_for,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const HOOKS_CONFIG_STORE_VERSION: u32 = 2;
const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(10);
const SECURE_FILE_FORMAT: &str = "codexbar.secure-file";
static HOOKS_CONFIG_MUTATION_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PreviousHooksConfigFileV2 {
    version: u32,
    config: HooksConfig,
}

#[derive(Clone)]
pub struct HooksConfigRepository {
    paths: StoragePaths,
    user_scope: UserScopeId,
    previous_v2_protection: Protection,
    protector: Arc<dyn SecretProtector>,
    lock_timeout: Duration,
}

impl std::fmt::Debug for HooksConfigRepository {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HooksConfigRepository")
            .field("store", &StoreKind::Hooks)
            .field("previous_v2_protection", &self.previous_v2_protection)
            .field("lock_timeout", &self.lock_timeout)
            .finish_non_exhaustive()
    }
}

impl HooksConfigRepository {
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
            previous_v2_protection: protection,
            protector,
            lock_timeout: DEFAULT_LOCK_TIMEOUT,
        }
    }

    pub fn path(&self) -> &Path {
        self.paths.hooks()
    }

    pub fn load(&self) -> Result<LoadState<HooksConfig>, StoreError> {
        let _process_guard = HOOKS_CONFIG_MUTATION_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.load_or_restore_previous_v2()
    }

    pub fn mutate<R>(&self, mutation: impl FnOnce(&mut HooksConfig) -> R) -> Result<R, StoreError> {
        let _process_guard = HOOKS_CONFIG_MUTATION_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.paths.ensure_directories()?;
        let _store_guard = CrossProcessStoreLock::acquire(
            StoreKind::Hooks,
            &self.user_scope,
            self.paths.hooks(),
            self.lock_timeout,
        )?;
        let mut config = self
            .load_or_restore_previous_v2_unlocked()?
            .into_writable_for(StoreKind::Hooks)?;
        let result = mutation(&mut config);
        validate_hooks_config(&config)?;
        self.write_public_config_verified(&config)?;

        match self.load_public_unlocked() {
            LoadState::Loaded(read_back) if read_back == config => Ok(result),
            LoadState::Loaded(_) => Err(invalid_payload_error()),
            state => Err(load_state_error(state)),
        }
    }

    fn load_or_restore_previous_v2(&self) -> Result<LoadState<HooksConfig>, StoreError> {
        self.paths.ensure_directories()?;
        if self.paths.hooks().exists() {
            return Ok(self.load_public_unlocked());
        }

        let _store_guard = CrossProcessStoreLock::acquire(
            StoreKind::Hooks,
            &self.user_scope,
            self.paths.hooks(),
            self.lock_timeout,
        )?;
        self.load_or_restore_previous_v2_unlocked()
    }

    fn load_or_restore_previous_v2_unlocked(&self) -> Result<LoadState<HooksConfig>, StoreError> {
        if self.paths.hooks().exists() {
            return Ok(self.load_public_unlocked());
        }

        let source = self.paths.previous_v2_hooks();
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
        let config = match self.decode_previous_v2_bytes(&raw) {
            Ok(config) => config,
            Err(error) => return Ok(load_state_from_error(error)),
        };
        self.write_public_config_verified(&config)?;
        Ok(self.load_public_unlocked())
    }

    fn load_public_unlocked(&self) -> LoadState<HooksConfig> {
        let raw = match fs::read(self.paths.hooks()) {
            Ok(raw) => raw,
            Err(source) if source.kind() == io::ErrorKind::NotFound => return LoadState::Missing,
            Err(source) => {
                return LoadState::IoError {
                    operation: StoreOperation::Read,
                    reason: StoreFailure::from_io(&source),
                };
            }
        };
        match self.decode_public_bytes(&raw) {
            Ok(config) => LoadState::Loaded(config),
            Err(error) => load_state_from_error(error),
        }
    }

    fn decode_public_bytes(&self, raw: &[u8]) -> Result<HooksConfig, StoreError> {
        let payload = std::str::from_utf8(raw).map_err(|_| invalid_payload_error())?;
        let config = serde_json::from_str(payload.trim_start_matches('\u{feff}'))
            .map_err(|_| invalid_payload_error())?;
        validate_hooks_config(&config)?;
        Ok(config)
    }

    fn decode_previous_v2_bytes(&self, raw: &[u8]) -> Result<HooksConfig, StoreError> {
        let payload = self.decode_previous_v2_payload(raw)?;
        let file: PreviousHooksConfigFileV2 =
            serde_json::from_str(payload.trim_start_matches('\u{feff}'))
                .map_err(|_| invalid_payload_error())?;
        if file.version != HOOKS_CONFIG_STORE_VERSION {
            return Err(StoreError::UnsupportedVersion {
                store: StoreKind::Hooks,
                found: file.version,
                supported: HOOKS_CONFIG_STORE_VERSION,
            });
        }
        validate_hooks_config(&file.config)?;
        Ok(file.config)
    }

    fn decode_previous_v2_payload(&self, raw: &[u8]) -> Result<String, StoreError> {
        let secure_envelope = raw_is_secure_envelope(raw);
        let payload = decode_string_bytes_with(raw, self.protector.as_ref()).map_err(|source| {
            if secure_envelope {
                if source.kind() == io::ErrorKind::InvalidData {
                    StoreError::CorruptEnvelope {
                        store: StoreKind::Hooks,
                        reason: StoreFailure::from_io(&source),
                    }
                } else {
                    let reason = if source.kind() == io::ErrorKind::Unsupported {
                        StoreFailure::new(StoreFailureKind::UnsupportedProtection, None)
                    } else {
                        StoreFailure::from_io(&source)
                    };
                    StoreError::DecryptFailed {
                        store: StoreKind::Hooks,
                        reason,
                    }
                }
            } else {
                invalid_payload_error()
            }
        })?;
        if secure_envelope && payload.as_bytes() == raw {
            return Err(StoreError::CorruptEnvelope {
                store: StoreKind::Hooks,
                reason: StoreFailure::new(StoreFailureKind::InvalidData, None),
            });
        }
        Ok(payload)
    }

    fn write_public_config_verified(&self, config: &HooksConfig) -> Result<(), StoreError> {
        let json = serde_json::to_vec_pretty(config).map_err(|_| StoreError::Io {
            store: StoreKind::Hooks,
            operation: StoreOperation::Serialize,
            reason: StoreFailure::new(StoreFailureKind::InvalidData, None),
        })?;
        write_bytes_verified_for(
            StoreKind::Hooks,
            self.paths.hooks(),
            &json,
            &RealAtomicFileOps,
        )?;
        restrict_storage_file(self.paths.hooks()).map_err(|source| StoreError::Io {
            store: StoreKind::Hooks,
            operation: StoreOperation::ApplyPermissions,
            reason: StoreFailure::from_io(&source),
        })
    }
}

fn validate_hooks_config(config: &HooksConfig) -> Result<(), StoreError> {
    if config.events.len() > HooksConfig::MAX_RULES {
        return Err(invalid_payload_error());
    }
    let value = serde_json::to_value(config).map_err(|_| invalid_payload_error())?;
    let normalized: HooksConfig =
        serde_json::from_value(value).map_err(|_| invalid_payload_error())?;
    if normalized != *config {
        return Err(invalid_payload_error());
    }
    Ok(())
}

fn raw_is_secure_envelope(raw: &[u8]) -> bool {
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

fn load_state_from_error(error: StoreError) -> LoadState<HooksConfig> {
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

fn load_state_error(state: LoadState<HooksConfig>) -> StoreError {
    match state {
        LoadState::Missing => StoreError::Io {
            store: StoreKind::Hooks,
            operation: StoreOperation::ReadBack,
            reason: StoreFailure::new(StoreFailureKind::NotFound, None),
        },
        LoadState::Loaded(_) => invalid_payload_error(),
        LoadState::NeedsMigration { source_version, .. } => StoreError::MigrationRequired {
            store: StoreKind::Hooks,
            source_version,
        },
        LoadState::CorruptEnvelope { reason } => StoreError::CorruptEnvelope {
            store: StoreKind::Hooks,
            reason,
        },
        LoadState::DecryptFailed { reason } => StoreError::DecryptFailed {
            store: StoreKind::Hooks,
            reason,
        },
        LoadState::InvalidPayload { reason } => StoreError::InvalidPayload {
            store: StoreKind::Hooks,
            reason,
        },
        LoadState::UnsupportedVersion { found, supported } => StoreError::UnsupportedVersion {
            store: StoreKind::Hooks,
            found,
            supported,
        },
        LoadState::Locked { reason } => StoreError::Locked {
            store: StoreKind::Hooks,
            reason,
        },
        LoadState::IoError { operation, reason } => StoreError::Io {
            store: StoreKind::Hooks,
            operation,
            reason,
        },
    }
}

fn invalid_payload_error() -> StoreError {
    StoreError::InvalidPayload {
        store: StoreKind::Hooks,
        reason: StoreFailure::new(StoreFailureKind::InvalidData, None),
    }
}

#[cfg(test)]
mod tests {
    use super::super::{HookEventType, HookRule};
    use super::*;
    use crate::secure_file::{SecretProtector, protected_file_bytes_with};
    use crate::storage::{LoadState, Protection, StoragePaths, UserScopeId};
    use std::fs;
    use std::io;
    use std::path::PathBuf;
    use std::sync::{Arc, Barrier};

    fn configured_hooks(label: &str) -> HooksConfig {
        #[cfg(windows)]
        let executable = PathBuf::from(r"C:\\Windows\\System32\\cmd.exe");
        #[cfg(not(windows))]
        let executable = PathBuf::from("/usr/bin/true");

        HooksConfig {
            enabled: true,
            events: vec![HookRule {
                enabled: true,
                event: Some(HookEventType::QuotaLow),
                events: vec![],
                provider: Some(label.to_string()),
                threshold: Some(0.8),
                executable,
                arguments: vec![label.to_string()],
                timeout_secs: 10,
            }],
        }
    }

    fn fixture() -> (PathBuf, StoragePaths, HooksConfigRepository) {
        let root = std::env::temp_dir().join(format!("codexbar-hooks-{}", uuid::Uuid::new_v4()));
        let paths = StoragePaths::from_roots(root.join("roaming"), root.join("local"));
        let repository = HooksConfigRepository::with_protection(
            paths.clone(),
            UserScopeId::from_stable_identifier(b"fixture-hooks-user"),
            Protection::Plaintext,
            Arc::new(IdentityProtector),
        );
        (root, paths, repository)
    }

    fn cleanup(root: &std::path::Path) {
        let _ = fs::remove_dir_all(root);
    }

    struct IdentityProtector;

    impl SecretProtector for IdentityProtector {
        fn protect_current_user(&self, plain: &[u8]) -> io::Result<Vec<u8>> {
            Ok(plain.to_vec())
        }

        fn unprotect_current_user(&self, encrypted: &[u8]) -> io::Result<Vec<u8>> {
            Ok(encrypted.to_vec())
        }
    }

    #[test]
    fn missing_hook_config_is_distinct_and_does_not_create_a_file() {
        let (root, paths, repository) = fixture();

        assert_eq!(
            repository.load().unwrap().kind(),
            crate::storage::LoadStateKind::Missing
        );
        assert!(!paths.hooks().exists());
        cleanup(&root);
    }

    #[test]
    fn public_hook_config_remains_authoritative_after_user_edits() {
        let (root, paths, repository) = fixture();
        let initial = configured_hooks("claude");
        let edited = configured_hooks("codex");
        fs::create_dir_all(paths.hooks().parent().unwrap()).unwrap();
        fs::write(paths.hooks(), serde_json::to_vec(&initial).unwrap()).unwrap();

        assert_eq!(
            repository.load().unwrap(),
            LoadState::Loaded(initial)
        );

        fs::write(paths.hooks(), serde_json::to_vec(&edited).unwrap()).unwrap();
        assert_eq!(repository.load().unwrap(), LoadState::Loaded(edited));
        cleanup(&root);
    }

    #[test]
    fn previous_encrypted_v2_config_is_restored_to_the_public_path_without_a_shared_journal() {
        let (root, paths, repository) = fixture();
        let expected = configured_hooks("claude");
        let previous = PreviousHooksConfigFileV2 {
            version: HOOKS_CONFIG_STORE_VERSION,
            config: expected.clone(),
        };
        let previous_json = serde_json::to_string(&previous).unwrap();
        let previous_bytes = protected_file_bytes_with(
            &previous_json,
            Protection::CurrentUserDpapi,
            &IdentityProtector,
        )
        .unwrap();
        let previous_path = paths.previous_v2_hooks();
        fs::create_dir_all(previous_path.parent().unwrap()).unwrap();
        fs::write(&previous_path, previous_bytes).unwrap();

        assert_eq!(repository.load().unwrap(), LoadState::Loaded(expected.clone()));
        assert!(previous_path.exists());
        assert!(!paths.migration_journal().exists());
        assert_eq!(
            serde_json::from_slice::<HooksConfig>(&fs::read(paths.hooks()).unwrap()).unwrap(),
            expected
        );
        cleanup(&root);
    }

    #[test]
    fn public_config_wins_over_a_previous_encrypted_v2_copy() {
        let (root, paths, repository) = fixture();
        let public = configured_hooks("public");
        let previous = PreviousHooksConfigFileV2 {
            version: HOOKS_CONFIG_STORE_VERSION,
            config: configured_hooks("previous"),
        };
        fs::create_dir_all(paths.hooks().parent().unwrap()).unwrap();
        fs::write(paths.hooks(), serde_json::to_vec(&public).unwrap()).unwrap();
        let previous_path = paths.previous_v2_hooks();
        fs::create_dir_all(previous_path.parent().unwrap()).unwrap();
        fs::write(&previous_path, serde_json::to_vec(&previous).unwrap()).unwrap();

        assert_eq!(repository.load().unwrap(), LoadState::Loaded(public));
        cleanup(&root);
    }

    #[test]
    fn corrupt_hook_config_is_not_default_and_cannot_be_overwritten() {
        let (root, paths, repository) = fixture();
        fs::create_dir_all(paths.hooks().parent().unwrap()).unwrap();
        let original = b"not-valid-hooks-json";
        fs::write(paths.hooks(), original).unwrap();

        assert!(matches!(
            repository.load().unwrap(),
            LoadState::InvalidPayload { .. }
        ));
        assert!(repository.mutate(|config| config.enabled = true).is_err());
        assert_eq!(fs::read(paths.hooks()).unwrap(), original);
        cleanup(&root);
    }

    #[test]
    fn future_previous_v2_config_cannot_be_overwritten_by_this_build() {
        let (root, paths, repository) = fixture();
        let previous_path = paths.previous_v2_hooks();
        fs::create_dir_all(previous_path.parent().unwrap()).unwrap();
        let future = br#"{"version":99,"config":{"enabled":true,"events":[]}}"#;
        fs::write(&previous_path, future).unwrap();

        assert!(matches!(
            repository.load().unwrap(),
            LoadState::UnsupportedVersion {
                found: 99,
                supported: HOOKS_CONFIG_STORE_VERSION
            }
        ));
        assert!(repository.mutate(|config| config.enabled = false).is_err());
        assert_eq!(fs::read(&previous_path).unwrap(), future);
        assert!(!paths.hooks().exists());
        cleanup(&root);
    }

    #[test]
    fn unknown_public_hook_fields_block_mutation_without_losing_the_original_bytes() {
        let (root, paths, repository) = fixture();
        let original = br#"{"enabled":true,"events":[],"futureRuleFormat":true}"#;
        fs::create_dir_all(paths.hooks().parent().unwrap()).unwrap();
        fs::write(paths.hooks(), original).unwrap();

        assert!(matches!(
            repository.load().unwrap(),
            LoadState::InvalidPayload { .. }
        ));
        assert!(repository.mutate(|config| config.enabled = false).is_err());
        assert_eq!(fs::read(paths.hooks()).unwrap(), original);
        cleanup(&root);
    }

    #[test]
    fn application_mutations_keep_hook_rules_as_readable_public_json() {
        let (root, paths, repository) = fixture();
        let expected = configured_hooks("claude");
        repository
            .mutate(|config| {
                *config = expected.clone();
            })
            .unwrap();

        let raw = fs::read(paths.hooks()).unwrap();
        assert_eq!(serde_json::from_slice::<HooksConfig>(&raw).unwrap(), expected);
        assert_ne!(
            serde_json::from_slice::<Value>(&raw)
                .unwrap()
                .get("format")
                .and_then(Value::as_str),
            Some(SECURE_FILE_FORMAT)
        );
        cleanup(&root);
    }

    #[test]
    fn concurrent_hook_mutations_preserve_both_rules() {
        let (root, _paths, repository) = fixture();
        let first = repository.clone();
        let second = repository.clone();
        let barrier = Arc::new(Barrier::new(2));
        let first_barrier = Arc::clone(&barrier);
        let second_barrier = Arc::clone(&barrier);

        let first_handle = std::thread::spawn(move || {
            first_barrier.wait();
            first
                .mutate(|config| {
                    config.enabled = true;
                    let mut replacement = configured_hooks("claude");
                    config.events.push(replacement.events.remove(0));
                })
                .unwrap();
        });
        let second_handle = std::thread::spawn(move || {
            second_barrier.wait();
            second
                .mutate(|config| {
                    config.enabled = true;
                    let mut replacement = configured_hooks("codex");
                    config.events.push(replacement.events.remove(0));
                })
                .unwrap();
        });
        first_handle.join().unwrap();
        second_handle.join().unwrap();

        let loaded = repository
            .load()
            .unwrap()
            .into_writable_for(crate::storage::StoreKind::Hooks)
            .unwrap();
        assert_eq!(loaded.events.len(), 2);
        assert!(
            loaded
                .events
                .iter()
                .any(|rule| rule.provider.as_deref() == Some("claude"))
        );
        assert!(
            loaded
                .events
                .iter()
                .any(|rule| rule.provider.as_deref() == Some("codex"))
        );
        cleanup(&root);
    }
}
