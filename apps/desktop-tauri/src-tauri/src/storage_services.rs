use codexbar::core::TokenAccountRepository;
use codexbar::settings::{
    ApiKeyRepository, ManualCookieRepository, Settings, SettingsMutationError, SettingsRepository,
};
use codexbar::storage::{
    LoadState, LoadStateKind, StoragePaths, StoreError, StoreFailureKind, StoreKind,
    StoreOperation, UserScopeId,
};
use serde::Serialize;
use std::sync::{Arc, Mutex};
use tauri::Manager;

use crate::kimi_accounts::KimiAccountRepository;
use crate::state::AppState;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreIssue {
    pub store: StoreKind,
    pub state: LoadStateKind,
    pub operation: Option<StoreOperation>,
    pub failure: Option<StoreFailureKind>,
    pub found_version: Option<u32>,
    pub supported_version: Option<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StartupStorageState {
    Ready { settings: Box<Settings> },
    RecoveryRequired { issues: Vec<StoreIssue> },
}

impl StartupStorageState {
    #[cfg(test)]
    pub const fn allows_runtime_services(&self) -> bool {
        matches!(self, Self::Ready { .. })
    }
}

fn startup_failure_message(issues: &[StoreIssue]) -> String {
    let summary = issues
        .iter()
        .map(|issue| format!("- {}: {:?}", issue.store, issue.state))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "CodexBar cannot safely read its saved settings.\n\n{summary}\n\nThe application stopped before starting the tray, background refresh, or any settings write. Keep the existing files unchanged and use the recovery tools in a verified CodexBar build."
    )
}

pub fn report_fatal_startup_error(issues: Vec<StoreIssue>) -> ! {
    for issue in &issues {
        tracing::error!(
            store = %issue.store,
            state = ?issue.state,
            operation = ?issue.operation,
            failure = ?issue.failure,
            "startup aborted because a settings store requires recovery"
        );
    }
    show_startup_error(&startup_failure_message(&issues));
    std::process::exit(1)
}

pub fn report_fatal_discovery_error(error: StoreError) -> ! {
    report_fatal_startup_error(vec![issue_from_error(error)])
}

#[cfg(target_os = "windows")]
fn show_startup_error(message: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MessageBoxW,
    };

    let title = "CodexBar storage recovery required"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let message = message
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            message.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR | MB_SETFOREGROUND,
        );
    }
}

#[cfg(not(target_os = "windows"))]
fn show_startup_error(message: &str) {
    eprintln!("{message}");
}

#[derive(Clone)]
pub struct StorageServices {
    pub settings: SettingsRepository,
    pub api_keys: ApiKeyRepository,
    pub manual_cookies: ManualCookieRepository,
    pub token_accounts: TokenAccountRepository,
    pub kimi_accounts: KimiAccountRepository,
    settings_commit_lock: Arc<Mutex<()>>,
    settings_effect_lock: Arc<Mutex<()>>,
}

impl std::fmt::Debug for StorageServices {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StorageServices")
            .field("settings", &self.settings)
            .field("api_keys", &self.api_keys)
            .field("manual_cookies", &self.manual_cookies)
            .field("token_accounts", &self.token_accounts)
            .field("kimi_accounts", &self.kimi_accounts)
            .finish_non_exhaustive()
    }
}

impl StorageServices {
    pub fn discover() -> Result<Self, StoreError> {
        Ok(Self::new(
            StoragePaths::discover()?,
            UserScopeId::current()?,
        ))
    }

    pub fn new(paths: StoragePaths, user_scope: UserScopeId) -> Self {
        Self {
            settings: SettingsRepository::new(paths.clone(), user_scope.clone()),
            api_keys: ApiKeyRepository::new(paths.clone(), user_scope.clone()),
            manual_cookies: ManualCookieRepository::new(paths.clone(), user_scope.clone()),
            token_accounts: TokenAccountRepository::new(paths.clone(), user_scope.clone()),
            kimi_accounts: KimiAccountRepository::new(paths, user_scope),
            settings_commit_lock: Arc::new(Mutex::new(())),
            settings_effect_lock: Arc::new(Mutex::new(())),
        }
    }

    pub fn load_startup(&self) -> StartupStorageState {
        classify_settings_startup(self.settings.load())
    }

    pub fn settings_commit_lock(&self) -> Arc<Mutex<()>> {
        Arc::clone(&self.settings_commit_lock)
    }

    pub fn settings_effect_lock(&self) -> Arc<Mutex<()>> {
        Arc::clone(&self.settings_effect_lock)
    }
}

pub fn settings_snapshot(state: &Mutex<AppState>) -> Settings {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .settings
        .clone()
}

pub fn storage_snapshot(state: &Mutex<AppState>) -> StorageServices {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .storage
        .clone()
}

pub fn storage_snapshot_from_app(app: &tauri::AppHandle) -> StorageServices {
    storage_snapshot(&app.state::<Mutex<AppState>>())
}

pub fn settings_snapshot_from_app(app: &tauri::AppHandle) -> Settings {
    settings_snapshot(&app.state::<Mutex<AppState>>())
}

pub fn settings_effect_lock_from_app(app: &tauri::AppHandle) -> Arc<Mutex<()>> {
    app.state::<Mutex<AppState>>()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .storage
        .settings_effect_lock()
}

pub fn mutate_settings<R>(
    state: &Mutex<AppState>,
    mutation: impl FnOnce(&mut Settings) -> Result<R, SettingsMutationError>,
) -> Result<(R, Settings), StoreError> {
    let (repository, commit_lock) = {
        let state = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        (
            state.storage.settings.clone(),
            state.storage.settings_commit_lock(),
        )
    };
    let _commit_guard = commit_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut committed = None;
    let result = repository.mutate(|settings| {
        let result = mutation(settings)?;
        committed = Some(settings.clone());
        Ok(result)
    })?;
    let committed = committed.ok_or_else(|| StoreError::InvalidPayload {
        store: StoreKind::Settings,
        reason: codexbar::storage::StoreFailure::new(StoreFailureKind::InvalidData, None),
    })?;
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .settings = committed.clone();
    Ok((result, committed))
}

pub fn mutate_settings_from_app<R>(
    app: &tauri::AppHandle,
    mutation: impl FnOnce(&mut Settings) -> Result<R, SettingsMutationError>,
) -> Result<(R, Settings), StoreError> {
    mutate_settings(&app.state::<Mutex<AppState>>(), mutation)
}

pub fn command_store_error(error: StoreError) -> String {
    let issue = issue_from_error(error);
    tracing::error!(
        store = %issue.store,
        state = ?issue.state,
        operation = ?issue.operation,
        failure = ?issue.failure,
        "storage command operation failed"
    );
    format!(
        "{} storage operation failed ({:?})",
        issue.store, issue.state
    )
}

fn classify_settings_startup(
    result: Result<LoadState<Settings>, StoreError>,
) -> StartupStorageState {
    match result {
        Ok(LoadState::Missing) => StartupStorageState::Ready {
            settings: Box::new(Settings::default()),
        },
        Ok(LoadState::Loaded(settings)) => StartupStorageState::Ready {
            settings: Box::new(settings),
        },
        Ok(state) => StartupStorageState::RecoveryRequired {
            issues: vec![issue_from_load_state(StoreKind::Settings, &state)],
        },
        Err(error) => StartupStorageState::RecoveryRequired {
            issues: vec![issue_from_error(error)],
        },
    }
}

fn issue_from_load_state<T>(store: StoreKind, state: &LoadState<T>) -> StoreIssue {
    match state {
        LoadState::Missing | LoadState::Loaded(_) => StoreIssue {
            store,
            state: state.kind(),
            operation: None,
            failure: None,
            found_version: None,
            supported_version: None,
        },
        LoadState::NeedsMigration { .. } => StoreIssue {
            store,
            state: LoadStateKind::NeedsMigration,
            operation: Some(StoreOperation::Migrate),
            failure: None,
            found_version: None,
            supported_version: None,
        },
        LoadState::CorruptEnvelope { reason }
        | LoadState::DecryptFailed { reason }
        | LoadState::InvalidPayload { reason }
        | LoadState::Locked { reason } => StoreIssue {
            store,
            state: state.kind(),
            operation: None,
            failure: Some(reason.kind),
            found_version: None,
            supported_version: None,
        },
        LoadState::UnsupportedVersion { found, supported } => StoreIssue {
            store,
            state: LoadStateKind::UnsupportedVersion,
            operation: Some(StoreOperation::ValidateVersion),
            failure: None,
            found_version: Some(*found),
            supported_version: Some(*supported),
        },
        LoadState::IoError { operation, reason } => StoreIssue {
            store,
            state: LoadStateKind::IoError,
            operation: Some(*operation),
            failure: Some(reason.kind),
            found_version: None,
            supported_version: None,
        },
    }
}

fn issue_from_error(error: StoreError) -> StoreIssue {
    match error {
        StoreError::MigrationRequired {
            store,
            source_version,
        } => StoreIssue {
            store,
            state: LoadStateKind::NeedsMigration,
            operation: Some(StoreOperation::Migrate),
            failure: None,
            found_version: Some(source_version),
            supported_version: None,
        },
        StoreError::CorruptEnvelope { store, reason } => StoreIssue {
            store,
            state: LoadStateKind::CorruptEnvelope,
            operation: None,
            failure: Some(reason.kind),
            found_version: None,
            supported_version: None,
        },
        StoreError::DecryptFailed { store, reason } => StoreIssue {
            store,
            state: LoadStateKind::DecryptFailed,
            operation: None,
            failure: Some(reason.kind),
            found_version: None,
            supported_version: None,
        },
        StoreError::InvalidPayload { store, reason } => StoreIssue {
            store,
            state: LoadStateKind::InvalidPayload,
            operation: None,
            failure: Some(reason.kind),
            found_version: None,
            supported_version: None,
        },
        StoreError::UnsupportedVersion {
            store,
            found,
            supported,
        } => StoreIssue {
            store,
            state: LoadStateKind::UnsupportedVersion,
            operation: Some(StoreOperation::ValidateVersion),
            failure: None,
            found_version: Some(found),
            supported_version: Some(supported),
        },
        StoreError::Locked { store, reason } | StoreError::LockTimeout { store, reason } => {
            StoreIssue {
                store,
                state: LoadStateKind::Locked,
                operation: Some(StoreOperation::Lock),
                failure: Some(reason.kind),
                found_version: None,
                supported_version: None,
            }
        }
        StoreError::MutationRejected { store, .. } => StoreIssue {
            store,
            state: LoadStateKind::InvalidPayload,
            operation: None,
            failure: Some(StoreFailureKind::InvalidData),
            found_version: None,
            supported_version: None,
        },
        StoreError::Io {
            store,
            operation,
            reason,
        } => StoreIssue {
            store,
            state: LoadStateKind::IoError,
            operation: Some(operation),
            failure: Some(reason.kind),
            found_version: None,
            supported_version: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codexbar::secure_file::{PlatformSecretProtector, SecretProtector};
    use codexbar::settings::{ApiKeyRepository, Settings, SettingsRepository};
    use codexbar::storage::{
        LoadState, Protection, StoragePaths, StoreFailure, StoreFailureKind, StoreOperation,
        UserScopeId,
    };
    use std::io;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

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

    fn failure(kind: StoreFailureKind) -> StoreFailure {
        StoreFailure::new(kind, Some(5))
    }

    fn fixture_state() -> (std::path::PathBuf, Arc<Mutex<AppState>>) {
        let root = std::env::temp_dir().join(format!(
            "codexbar-settings-state-test-{}",
            uuid::Uuid::new_v4()
        ));
        let paths = StoragePaths::from_roots(root.join("roaming"), root.join("local"));
        let user_scope = UserScopeId::from_stable_identifier(b"settings-state-test-user");
        let storage = StorageServices {
            settings: SettingsRepository::with_protection(
                paths.clone(),
                user_scope.clone(),
                Protection::Plaintext,
                Arc::new(PlatformSecretProtector),
            ),
            api_keys: ApiKeyRepository::with_protection(
                paths.clone(),
                user_scope.clone(),
                Protection::Plaintext,
                Arc::new(PlatformSecretProtector),
            ),
            manual_cookies: ManualCookieRepository::with_protection(
                paths.clone(),
                user_scope.clone(),
                Protection::Plaintext,
                Arc::new(PlatformSecretProtector),
            ),
            token_accounts: TokenAccountRepository::with_protection(
                paths.clone(),
                user_scope.clone(),
                Protection::Plaintext,
                Arc::new(PlatformSecretProtector),
            ),
            kimi_accounts: KimiAccountRepository::with_protection(
                paths,
                user_scope,
                Protection::Plaintext,
                Arc::new(PlatformSecretProtector),
            ),
            settings_commit_lock: Arc::new(Mutex::new(())),
            settings_effect_lock: Arc::new(Mutex::new(())),
        };
        let mut state = AppState::new();
        state.storage = storage;
        state.settings = Settings::default();
        (root, Arc::new(Mutex::new(state)))
    }

    fn remove_fixture(root: &std::path::Path) {
        if root.exists() {
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn missing_settings_start_with_defaults_without_writing_a_store() {
        let startup = classify_settings_startup(Ok(LoadState::Missing));

        match startup {
            StartupStorageState::Ready { settings } => {
                assert_eq!(*settings, Settings::default());
            }
            StartupStorageState::RecoveryRequired { .. } => {
                panic!("missing settings must use in-memory defaults")
            }
        }
    }

    #[test]
    fn every_unreadable_settings_state_blocks_runtime_services() {
        let states = [
            LoadState::<Settings>::CorruptEnvelope {
                reason: failure(StoreFailureKind::InvalidData),
            },
            LoadState::DecryptFailed {
                reason: failure(StoreFailureKind::AuthenticationFailed),
            },
            LoadState::InvalidPayload {
                reason: failure(StoreFailureKind::InvalidData),
            },
            LoadState::UnsupportedVersion {
                found: 3,
                supported: 2,
            },
            LoadState::Locked {
                reason: failure(StoreFailureKind::TimedOut),
            },
            LoadState::IoError {
                operation: StoreOperation::Read,
                reason: failure(StoreFailureKind::PermissionDenied),
            },
        ];

        for state in states {
            let expected = state.kind();
            let startup = classify_settings_startup(Ok(state));
            assert!(!startup.allows_runtime_services(), "{expected:?}");
            match startup {
                StartupStorageState::RecoveryRequired { issues } => {
                    assert_eq!(issues.len(), 1, "{expected:?}");
                    assert_eq!(issues[0].state, expected, "{expected:?}");
                }
                StartupStorageState::Ready { .. } => {
                    panic!("{expected:?} must require recovery")
                }
            }
        }
    }

    #[test]
    fn loaded_settings_are_the_only_persisted_snapshot_allowed_to_start() {
        let expected = Settings {
            refresh_interval_secs: 17,
            start_minimized: true,
            ..Settings::default()
        };

        let startup = classify_settings_startup(Ok(LoadState::Loaded(expected.clone())));

        assert!(startup.allows_runtime_services());
        match startup {
            StartupStorageState::Ready { settings } => assert_eq!(*settings, expected),
            StartupStorageState::RecoveryRequired { .. } => panic!("loaded settings are valid"),
        }
    }

    #[test]
    fn startup_issues_are_path_free_and_secret_free() {
        let secret = "fixture-secret-path-C:/Users/sensitive";
        let startup = classify_settings_startup(Ok(LoadState::<Settings>::InvalidPayload {
            reason: StoreFailure::new(StoreFailureKind::InvalidData, None),
        }));
        let rendered = format!("{startup:?}");

        assert!(!rendered.contains(secret));
        assert!(!rendered.contains("C:/Users"));
        assert!(rendered.contains("Settings"));
        assert!(rendered.contains("InvalidPayload"));

        let StartupStorageState::RecoveryRequired { issues } = startup else {
            unreachable!()
        };
        let message = startup_failure_message(&issues);
        assert!(!message.contains(secret));
        assert!(!message.contains("C:/Users"));
        assert!(message.contains("settings"));
        assert!(message.contains("InvalidPayload"));
    }

    #[test]
    fn command_storage_errors_expose_only_redacted_categories() {
        let rendered = command_store_error(StoreError::Io {
            store: StoreKind::Settings,
            operation: StoreOperation::Replace,
            reason: StoreFailure::new(StoreFailureKind::PermissionDenied, Some(5)),
        });

        assert_eq!(rendered, "settings storage operation failed (IoError)");
        assert!(!rendered.contains("C:\\Users"));
        assert!(!rendered.contains("secret"));
        assert!(!rendered.contains("os-code"));
    }

    #[test]
    fn settings_disk_io_never_holds_the_app_state_mutex() {
        let (root, state) = fixture_state();
        let observed = Arc::clone(&state);

        mutate_settings(&state, |settings| {
            assert!(
                observed.try_lock().is_ok(),
                "AppState must be unlocked before repository mutation"
            );
            settings.refresh_interval_secs = 17;
            Ok(())
        })
        .unwrap();

        assert_eq!(settings_snapshot(&state).refresh_interval_secs, 17);
        drop(state);
        remove_fixture(&root);
    }

    #[test]
    fn concurrent_settings_transactions_preserve_disk_and_memory_order() {
        let (root, state) = fixture_state();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let first_state = Arc::clone(&state);
        let first_barrier = Arc::clone(&barrier);
        let first = std::thread::spawn(move || {
            first_barrier.wait();
            mutate_settings(&first_state, |settings| {
                settings.refresh_interval_secs = 17;
                Ok(())
            })
            .unwrap();
        });
        let second_state = Arc::clone(&state);
        let second_barrier = Arc::clone(&barrier);
        let second = std::thread::spawn(move || {
            second_barrier.wait();
            mutate_settings(&second_state, |settings| {
                settings.show_notifications = false;
                Ok(())
            })
            .unwrap();
        });

        first.join().unwrap();
        second.join().unwrap();
        let memory = settings_snapshot(&state);
        let repository = state.lock().unwrap().storage.settings.clone();
        let LoadState::Loaded(disk) = repository.load().unwrap() else {
            panic!("settings repository must be loaded")
        };
        assert_eq!(memory, disk);
        assert_eq!(disk.refresh_interval_secs, 17);
        assert!(!disk.show_notifications);
        drop(state);
        remove_fixture(&root);
    }

    #[test]
    fn rejected_settings_transaction_changes_neither_disk_nor_memory() {
        let (root, state) = fixture_state();
        mutate_settings(&state, |settings| {
            settings.refresh_interval_secs = 17;
            Ok(())
        })
        .unwrap();
        let repository = state.lock().unwrap().storage.settings.clone();
        let before_bytes = std::fs::read(repository.path()).unwrap();
        let before_memory = settings_snapshot(&state);

        assert!(
            mutate_settings(&state, |settings| {
                settings.refresh_interval_secs = 99;
                Err::<(), _>(codexbar::settings::SettingsMutationError::InvalidValue)
            })
            .is_err()
        );

        assert_eq!(settings_snapshot(&state), before_memory);
        assert_eq!(std::fs::read(repository.path()).unwrap(), before_bytes);
        drop(state);
        remove_fixture(&root);
    }

    #[test]
    fn failed_settings_transaction_keeps_memory_and_event_count_unchanged() {
        let (root, state) = fixture_state();
        let paths = StoragePaths::from_roots(root.join("roaming"), root.join("local"));
        let repository = SettingsRepository::with_protection(
            paths,
            UserScopeId::from_stable_identifier(b"settings-state-test-user"),
            Protection::CurrentUserDpapi,
            Arc::new(FailingProtectProtector),
        );
        state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .storage
            .settings = repository;
        let before = settings_snapshot(&state);
        let event_count = AtomicUsize::new(0);

        let result = mutate_settings(&state, |settings| {
            settings.refresh_interval_secs = 17;
            Ok(())
        });
        if result.is_ok() {
            event_count.fetch_add(1, Ordering::SeqCst);
        }

        assert!(result.is_err());
        assert_eq!(settings_snapshot(&state), before);
        assert_eq!(event_count.load(Ordering::SeqCst), 0);
        drop(state);
        remove_fixture(&root);
    }
}
