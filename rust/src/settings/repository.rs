use super::{RawSettings, Settings};
use crate::secure_file::{
    PlatformSecretProtector, SecretProtector, decode_string_bytes_with, default_protection,
    protected_file_bytes_with, restrict_storage_file,
};
use crate::storage::{
    CrossProcessStoreLock, LoadState, MigrationCoordinator, MigrationRequest, Protection,
    RealAtomicFileOps, StoragePaths, StoreError, StoreFailure, StoreFailureKind, StoreKind,
    StoreMutationFailureKind, StoreOperation, UserScopeId, write_bytes_verified_for,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const SETTINGS_STORE_VERSION: u32 = 2;
const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(10);
const SECURE_FILE_FORMAT: &str = "codexbar.secure-file";
static SETTINGS_MUTATION_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsFileV2 {
    version: u32,
    settings: Settings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsMutationError {
    InvalidValue,
}

impl std::fmt::Display for SettingsMutationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidValue => formatter.write_str("the settings value is invalid"),
        }
    }
}

impl std::error::Error for SettingsMutationError {}

#[derive(Clone)]
pub struct SettingsRepository {
    paths: StoragePaths,
    user_scope: UserScopeId,
    protection: Protection,
    protector: Arc<dyn SecretProtector>,
    lock_timeout: Duration,
}

impl std::fmt::Debug for SettingsRepository {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SettingsRepository")
            .field("store", &StoreKind::Settings)
            .field("protection", &self.protection)
            .field("lock_timeout", &self.lock_timeout)
            .finish_non_exhaustive()
    }
}

impl SettingsRepository {
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
        self.paths.settings()
    }

    pub fn load(&self) -> Result<LoadState<Settings>, StoreError> {
        let _process_guard = SETTINGS_MUTATION_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.load_or_migrate_unlocked()
    }

    pub fn replace(&self, replacement: Settings) -> Result<(), StoreError> {
        self.mutate(|settings| {
            *settings = replacement;
            Ok(())
        })
    }

    pub fn mutate<R>(
        &self,
        mutation: impl FnOnce(&mut Settings) -> Result<R, SettingsMutationError>,
    ) -> Result<R, StoreError> {
        let _process_guard = SETTINGS_MUTATION_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.paths.ensure_directories()?;

        ensure_writable_state(self.load_or_migrate_unlocked()?)?;
        let _store_guard = CrossProcessStoreLock::acquire(
            StoreKind::Settings,
            &self.user_scope,
            self.paths.settings(),
            self.lock_timeout,
        )?;
        let mut settings = self
            .load_v2_unlocked()
            .into_writable_for(StoreKind::Settings)?;
        let result = mutation(&mut settings).map_err(map_mutation_error)?;
        validate_settings(&settings)?;

        let protected = self.protected_bytes(&settings)?;
        write_bytes_verified_for(
            StoreKind::Settings,
            self.paths.settings(),
            &protected,
            &RealAtomicFileOps,
        )?;
        restrict_storage_file(self.paths.settings()).map_err(|source| StoreError::Io {
            store: StoreKind::Settings,
            operation: StoreOperation::ApplyPermissions,
            reason: StoreFailure::from_io(&source),
        })?;

        match self.load_v2_unlocked() {
            LoadState::Loaded(read_back) if read_back == settings => Ok(result),
            LoadState::Loaded(_) => Err(invalid_payload_error()),
            state => Err(load_state_error(state)),
        }
    }

    fn load_or_migrate_unlocked(&self) -> Result<LoadState<Settings>, StoreError> {
        self.paths.ensure_directories()?;
        if self.paths.settings().exists() {
            return Ok(self.load_v2_unlocked());
        }

        let source = self.paths.legacy_settings();
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
        let (settings, unknown_fields) = match self.decode_legacy_bytes(&raw) {
            Ok(parsed) => parsed,
            Err(error) => return Ok(load_state_from_error(error)),
        };
        validate_settings(&settings)?;
        if !unknown_fields.is_empty() {
            tracing::warn!(
                store = %StoreKind::Settings,
                unknown_field_count = unknown_fields.len(),
                "legacy settings contain unrecognized fields"
            );
        }

        let request = MigrationRequest {
            store: StoreKind::Settings,
            source_path: source,
            source_version: 1,
            destination_path: self.paths.settings().to_path_buf(),
            destination_bytes: self.protected_bytes(&settings)?,
        };
        let repository = self;
        MigrationCoordinator::new(self.paths.clone(), self.user_scope.clone())
            .migrate_bytes_verified(request, move |bytes| {
                repository.decode_v2_bytes(bytes).map(|_| ())
            })?;

        Ok(self.load_v2_unlocked())
    }

    fn load_v2_unlocked(&self) -> LoadState<Settings> {
        let raw = match fs::read(self.paths.settings()) {
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
            Ok(settings) => LoadState::Loaded(settings),
            Err(error) => load_state_from_error(error),
        }
    }

    fn decode_v2_bytes(&self, raw: &[u8]) -> Result<Settings, StoreError> {
        let payload = self.decode_payload(raw)?;
        let value = serde_json::from_str::<Value>(payload.trim_start_matches('\u{feff}'))
            .map_err(|_| invalid_payload_error())?;
        let object = value.as_object().ok_or_else(invalid_payload_error)?;
        if object.len() != 2 || !object.contains_key("version") || !object.contains_key("settings")
        {
            return Err(invalid_payload_error());
        }
        let found = object
            .get("version")
            .and_then(Value::as_u64)
            .and_then(|version| u32::try_from(version).ok())
            .ok_or_else(invalid_payload_error)?;
        if found != SETTINGS_STORE_VERSION {
            return Err(StoreError::UnsupportedVersion {
                store: StoreKind::Settings,
                found,
                supported: SETTINGS_STORE_VERSION,
            });
        }
        let settings_value = object
            .get("settings")
            .cloned()
            .ok_or_else(invalid_payload_error)?;
        let (settings, unknown_fields) =
            RawSettings::from_value_with_unknown_fields(settings_value)
                .map_err(|_| invalid_payload_error())?;
        if !unknown_fields.is_empty() {
            return Err(invalid_payload_error());
        }
        validate_settings(&settings)?;
        Ok(settings)
    }

    fn decode_legacy_bytes(&self, raw: &[u8]) -> Result<(Settings, Vec<String>), StoreError> {
        let payload = self.decode_payload(raw)?;
        let value = serde_json::from_str::<Value>(payload.trim_start_matches('\u{feff}'))
            .map_err(|_| invalid_payload_error())?;
        RawSettings::from_value_with_unknown_fields(value).map_err(|_| invalid_payload_error())
    }

    fn decode_payload(&self, raw: &[u8]) -> Result<String, StoreError> {
        let secure_envelope = raw_is_secure_envelope(raw);
        let payload = decode_string_bytes_with(raw, self.protector.as_ref()).map_err(|source| {
            if secure_envelope {
                if source.kind() == io::ErrorKind::InvalidData {
                    StoreError::CorruptEnvelope {
                        store: StoreKind::Settings,
                        reason: StoreFailure::from_io(&source),
                    }
                } else {
                    let reason = if source.kind() == io::ErrorKind::Unsupported {
                        StoreFailure::new(StoreFailureKind::UnsupportedProtection, None)
                    } else {
                        StoreFailure::from_io(&source)
                    };
                    StoreError::DecryptFailed {
                        store: StoreKind::Settings,
                        reason,
                    }
                }
            } else {
                invalid_payload_error()
            }
        })?;
        if secure_envelope && payload.as_bytes() == raw {
            return Err(StoreError::CorruptEnvelope {
                store: StoreKind::Settings,
                reason: StoreFailure::new(StoreFailureKind::InvalidData, None),
            });
        }
        Ok(payload)
    }

    fn protected_bytes(&self, settings: &Settings) -> Result<Vec<u8>, StoreError> {
        let file = SettingsFileV2 {
            version: SETTINGS_STORE_VERSION,
            settings: settings.clone(),
        };
        let json = serde_json::to_string(&file).map_err(|_| StoreError::Io {
            store: StoreKind::Settings,
            operation: StoreOperation::Serialize,
            reason: StoreFailure::new(StoreFailureKind::InvalidData, None),
        })?;
        protected_file_bytes_with(&json, self.protection, self.protector.as_ref()).map_err(
            |source| StoreError::Io {
                store: StoreKind::Settings,
                operation: StoreOperation::Protect,
                reason: StoreFailure::from_io(&source),
            },
        )
    }
}

fn validate_settings(settings: &Settings) -> Result<(), StoreError> {
    let value = serde_json::to_value(settings).map_err(|_| invalid_payload_error())?;
    let (normalized, unknown_fields) =
        RawSettings::from_value_with_unknown_fields(value).map_err(|_| invalid_payload_error())?;
    if normalized != *settings || !unknown_fields.is_empty() {
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

fn ensure_writable_state(state: LoadState<Settings>) -> Result<(), StoreError> {
    state.into_writable_for(StoreKind::Settings).map(|_| ())
}

fn load_state_from_error(error: StoreError) -> LoadState<Settings> {
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

fn load_state_error(state: LoadState<Settings>) -> StoreError {
    match state {
        LoadState::Missing => StoreError::Io {
            store: StoreKind::Settings,
            operation: StoreOperation::ReadBack,
            reason: StoreFailure::new(StoreFailureKind::NotFound, None),
        },
        LoadState::Loaded(_) => invalid_payload_error(),
        LoadState::NeedsMigration { source_version, .. } => StoreError::MigrationRequired {
            store: StoreKind::Settings,
            source_version,
        },
        LoadState::CorruptEnvelope { reason } => StoreError::CorruptEnvelope {
            store: StoreKind::Settings,
            reason,
        },
        LoadState::DecryptFailed { reason } => StoreError::DecryptFailed {
            store: StoreKind::Settings,
            reason,
        },
        LoadState::InvalidPayload { reason } => StoreError::InvalidPayload {
            store: StoreKind::Settings,
            reason,
        },
        LoadState::UnsupportedVersion { found, supported } => StoreError::UnsupportedVersion {
            store: StoreKind::Settings,
            found,
            supported,
        },
        LoadState::Locked { reason } => StoreError::Locked {
            store: StoreKind::Settings,
            reason,
        },
        LoadState::IoError { operation, reason } => StoreError::Io {
            store: StoreKind::Settings,
            operation,
            reason,
        },
    }
}

fn map_mutation_error(_error: SettingsMutationError) -> StoreError {
    StoreError::MutationRejected {
        store: StoreKind::Settings,
        reason: StoreMutationFailureKind::InvalidValue,
        limit: None,
    }
}

fn invalid_payload_error() -> StoreError {
    StoreError::InvalidPayload {
        store: StoreKind::Settings,
        reason: StoreFailure::new(StoreFailureKind::InvalidData, None),
    }
}

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::secure_file::{PlatformSecretProtector, SecretProtector, protected_file_bytes_with};
    use crate::storage::{LoadState, LoadStateKind, Protection, StoragePaths, UserScopeId};
    use serde_json::json;
    use std::fs;
    use std::io::{self, Write};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    fn fixture_paths(root: &std::path::Path) -> StoragePaths {
        StoragePaths::from_roots(root.join("roaming"), root.join("local"))
    }

    fn repository(paths: StoragePaths) -> SettingsRepository {
        SettingsRepository::with_protection(
            paths,
            UserScopeId::from_stable_identifier(b"fixture-settings-user"),
            Protection::Plaintext,
            Arc::new(PlatformSecretProtector),
        )
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

    struct FailingDecryptProtector;

    impl SecretProtector for FailingDecryptProtector {
        fn protect_current_user(&self, plain: &[u8]) -> io::Result<Vec<u8>> {
            Ok(plain.to_vec())
        }

        fn unprotect_current_user(&self, _encrypted: &[u8]) -> io::Result<Vec<u8>> {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "fixture decrypt failure",
            ))
        }
    }

    struct FailingProtectProtector;

    impl SecretProtector for FailingProtectProtector {
        fn protect_current_user(&self, _plain: &[u8]) -> io::Result<Vec<u8>> {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "fixture protection failure",
            ))
        }

        fn unprotect_current_user(&self, encrypted: &[u8]) -> io::Result<Vec<u8>> {
            Ok(encrypted.to_vec())
        }
    }

    fn loaded(repository: &SettingsRepository) -> Settings {
        match repository.load().unwrap() {
            LoadState::Loaded(settings) => settings,
            state => panic!("expected loaded settings, got {:?}", state.kind()),
        }
    }

    #[test]
    fn missing_settings_are_distinct_and_do_not_create_a_store() {
        let directory = tempfile::tempdir().unwrap();
        let paths = fixture_paths(directory.path());
        let repository = repository(paths.clone());

        assert_eq!(repository.load().unwrap().kind(), LoadStateKind::Missing);
        assert!(!paths.settings().exists());
    }

    #[test]
    fn malformed_and_future_v2_settings_block_mutation_without_overwriting_bytes() {
        for (scenario, bytes, expected) in [
            (
                "malformed",
                b"not-json-fixture-settings".to_vec(),
                LoadStateKind::InvalidPayload,
            ),
            (
                "future",
                serde_json::to_vec(&json!({
                    "version": SETTINGS_STORE_VERSION + 1,
                    "settings": {}
                }))
                .unwrap(),
                LoadStateKind::UnsupportedVersion,
            ),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let paths = fixture_paths(directory.path());
            paths.ensure_directories().unwrap();
            fs::write(paths.settings(), &bytes).unwrap();
            let repository = repository(paths.clone());
            let mutation_called = AtomicBool::new(false);

            assert_eq!(repository.load().unwrap().kind(), expected, "{scenario}");
            assert!(
                repository
                    .mutate(|settings| {
                        mutation_called.store(true, Ordering::SeqCst);
                        settings.refresh_interval_secs = 17;
                        Ok(())
                    })
                    .is_err(),
                "{scenario}"
            );
            assert!(!mutation_called.load(Ordering::SeqCst), "{scenario}");
            assert_eq!(fs::read(paths.settings()).unwrap(), bytes, "{scenario}");
        }
    }

    #[test]
    fn formal_v2_unknown_fields_are_invalid_and_never_rewritten() {
        let directory = tempfile::tempdir().unwrap();
        let paths = fixture_paths(directory.path());
        paths.ensure_directories().unwrap();
        let bytes = serde_json::to_vec(&json!({
            "version": SETTINGS_STORE_VERSION,
            "settings": { "unknown_future_setting": true }
        }))
        .unwrap();
        fs::write(paths.settings(), &bytes).unwrap();
        let repository = repository(paths.clone());

        assert_eq!(
            repository.load().unwrap().kind(),
            LoadStateKind::InvalidPayload
        );
        assert!(
            repository
                .mutate(|settings| {
                    settings.refresh_interval_secs = 17;
                    Ok(())
                })
                .is_err()
        );
        assert_eq!(fs::read(paths.settings()).unwrap(), bytes);
    }

    #[test]
    fn decrypt_and_protect_failures_never_clear_or_create_settings() {
        let directory = tempfile::tempdir().unwrap();
        let paths = fixture_paths(directory.path());
        paths.ensure_directories().unwrap();
        let protected = protected_file_bytes_with(
            r#"{"version":2,"settings":{}}"#,
            Protection::CurrentUserDpapi,
            &IdentityProtector,
        )
        .unwrap();
        fs::write(paths.settings(), &protected).unwrap();
        let decrypt_failing = SettingsRepository::with_protection(
            paths.clone(),
            UserScopeId::from_stable_identifier(b"fixture-decrypt-failure"),
            Protection::CurrentUserDpapi,
            Arc::new(FailingDecryptProtector),
        );
        assert_eq!(
            decrypt_failing.load().unwrap().kind(),
            LoadStateKind::DecryptFailed
        );
        assert!(
            decrypt_failing
                .mutate(|settings| {
                    settings.refresh_interval_secs = 17;
                    Ok(())
                })
                .is_err()
        );
        assert_eq!(fs::read(paths.settings()).unwrap(), protected);

        let empty_directory = tempfile::tempdir().unwrap();
        let empty_paths = fixture_paths(empty_directory.path());
        let protect_failing = SettingsRepository::with_protection(
            empty_paths.clone(),
            UserScopeId::from_stable_identifier(b"fixture-protect-failure"),
            Protection::CurrentUserDpapi,
            Arc::new(FailingProtectProtector),
        );
        assert!(
            protect_failing
                .mutate(|settings| {
                    settings.refresh_interval_secs = 17;
                    Ok(())
                })
                .is_err()
        );
        assert!(!empty_paths.settings().exists());
    }

    #[test]
    fn concurrent_settings_mutations_preserve_both_fields() {
        let directory = tempfile::tempdir().unwrap();
        let repository = Arc::new(repository(fixture_paths(directory.path())));
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let first_repository = Arc::clone(&repository);
        let first_barrier = Arc::clone(&barrier);
        let first = std::thread::spawn(move || {
            first_barrier.wait();
            first_repository
                .mutate(|settings| {
                    settings.refresh_interval_secs = 17;
                    Ok(())
                })
                .unwrap();
        });
        let second_repository = Arc::clone(&repository);
        let second_barrier = Arc::clone(&barrier);
        let second = std::thread::spawn(move || {
            second_barrier.wait();
            second_repository
                .mutate(|settings| {
                    settings.show_notifications = false;
                    Ok(())
                })
                .unwrap();
        });

        first.join().unwrap();
        second.join().unwrap();
        let settings = loaded(&repository);
        assert_eq!(settings.refresh_interval_secs, 17);
        assert!(!settings.show_notifications);
    }

    #[test]
    fn loading_v2_settings_preserves_start_at_login_without_registry_sync() {
        let directory = tempfile::tempdir().unwrap();
        let paths = fixture_paths(directory.path());
        let repository = repository(paths);

        repository
            .mutate(|settings| {
                settings.start_at_login = true;
                Ok(())
            })
            .unwrap();

        assert!(loaded(&repository).start_at_login);
    }

    #[derive(Clone, Default)]
    struct LogBuffer(Arc<Mutex<Vec<u8>>>);

    struct LogWriter(Arc<Mutex<Vec<u8>>>);

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogBuffer {
        type Writer = LogWriter;

        fn make_writer(&'a self) -> Self::Writer {
            LogWriter(Arc::clone(&self.0))
        }
    }

    impl Write for LogWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn legacy_settings_migrate_with_backup_and_a_redacted_unknown_field_warning() {
        let directory = tempfile::tempdir().unwrap();
        let paths = fixture_paths(directory.path());
        let source = paths.legacy_settings();
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        let source_bytes = serde_json::to_vec(&json!({
            "refresh_interval_secs": 17,
            "start_at_login": true,
            "codex_cookie_source": "manual",
            "future_extension": {
                "secret": "fixture-unknown-setting-secret"
            }
        }))
        .unwrap();
        fs::write(&source, &source_bytes).unwrap();
        let repository = repository(paths.clone());
        let output = LogBuffer::default();
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(output.clone())
            .finish();

        let settings = tracing::subscriber::with_default(subscriber, || loaded(&repository));

        assert_eq!(settings.refresh_interval_secs, 17);
        assert!(settings.start_at_login);
        assert_eq!(
            settings.cookie_source(crate::core::ProviderId::Codex),
            "manual"
        );
        assert_eq!(fs::read(&source).unwrap(), source_bytes);
        let backups = fs::read_dir(paths.migration_backups())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("settings-") && name.ends_with(".backup"))
            })
            .collect::<Vec<_>>();
        assert_eq!(backups.len(), 1);
        assert_eq!(fs::read(&backups[0]).unwrap(), source_bytes);

        let log = String::from_utf8(output.0.lock().unwrap().clone()).unwrap();
        assert!(log.contains("legacy settings contain unrecognized fields"));
        assert!(log.contains("unknown_field_count=1"));
        assert!(!log.contains("future_extension"));
        assert!(!log.contains("fixture-unknown-setting-secret"));
    }
}
