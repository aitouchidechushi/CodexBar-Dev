use std::collections::{HashMap, HashSet, hash_map::DefaultHasher};
use std::fs;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
};
use std::time::Duration;

use chrono::Utc;
use codexbar::core::ProviderError;
use codexbar::providers::kimi::{
    KimiAccountIdentity, KimiIdentityFingerprint, KimiMonthlyQuota, fetch_code_api_identity,
    fetch_web_account,
};
use codexbar::secure_file::{
    PlatformSecretProtector, SecretProtector, decode_string_bytes_with, protected_file_bytes_with,
    restrict_storage_file,
};
use codexbar::storage::{
    CrossProcessStoreLock, LoadState, MigrationCoordinator, MigrationRequest, Protection,
    RealAtomicFileOps, StoragePaths, StoreError, StoreFailure, StoreFailureKind, StoreKind,
    StoreOperation, UserScopeId, write_bytes_verified_for,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{
    AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, Url, WebviewUrl,
    webview::{NewWindowResponse, WebviewBuilder},
};
use uuid::Uuid;

fn load_api_keys_for_matching() -> Result<codexbar::settings::ApiKeys, String> {
    codexbar::settings::ApiKeys::load()
        .and_then(|state| state.into_writable_for(codexbar::storage::StoreKind::ApiKeys))
        .map_err(|error| error.to_string())
}

#[cfg(windows)]
use webview2_com::{
    ExecuteScriptCompletedHandler, GetCookiesCompletedHandler,
    Microsoft::Web::WebView2::Win32::ICoreWebView2_2, take_pwstr,
};
#[cfg(windows)]
use windows_core::{HSTRING, Interface, PCWSTR, PWSTR};

const LEGACY_KIMI_ACCOUNT_STORE_VERSION: u32 = 1;
const PREVIOUS_KIMI_ACCOUNT_STORE_VERSION: u32 = 2;
const KIMI_ACCOUNT_STORE_VERSION: u32 = 3;
const KIMI_ACCOUNT_LOCK_TIMEOUT: Duration = Duration::from_secs(10);
const SECURE_FILE_FORMAT: &str = "codexbar.secure-file";
const KIMI_ACCOUNT_EVENT: &str = "kimi-accounts-updated";
const KIMI_LOGIN_COMPLETION_EVENT: &str = "kimi-account-login-completed";
pub(crate) const KIMI_LOGIN_URL: &str = "https://www.kimi.com/";
const KIMI_WEBVIEW_PREFIX: &str = "kimi-account-";
const LOGIN_FOREGROUND_BUDGET: std::time::Duration = std::time::Duration::from_millis(4_500);
const LOCAL_STORAGE_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
const LOGIN_CANCEL_COOPERATIVE_BUDGET: std::time::Duration = std::time::Duration::from_millis(400);
const LOGIN_CANCEL_ABORT_BUDGET: std::time::Duration = std::time::Duration::from_millis(100);
const BACKGROUND_WEBVIEW_SHUTDOWN_BUDGET: std::time::Duration =
    std::time::Duration::from_millis(500);
const BROWSER_SECRET_CLEANUP_RETRY_DELAYS: [Duration; 4] = [
    Duration::from_millis(250),
    Duration::from_millis(500),
    Duration::from_secs(1),
    Duration::from_secs(2),
];

fn kimi_settings(app: &AppHandle) -> codexbar::settings::Settings {
    crate::storage_services::settings_snapshot_from_app(app)
}

fn kimi_monthly_quota_enabled(app: &AppHandle) -> bool {
    kimi_settings(app).kimi_monthly_quota_enabled
}

fn kimi_account_repository(app: &AppHandle) -> KimiAccountRepository {
    crate::storage_services::storage_snapshot_from_app(app).kimi_accounts
}

fn load_kimi_account_store(app: &AppHandle) -> Result<KimiAccountStore, String> {
    load_kimi_account_store_with(
        &kimi_account_repository(app),
        cache(),
        |snapshots, failed| {
            emit_kimi_state(app, snapshots, failed);
        },
    )
    .map_err(crate::storage_services::command_store_error)?
}

fn load_kimi_account_store_with(
    repository: &KimiAccountRepository,
    account_cache: &Mutex<KimiAccountCache>,
    emit: impl FnOnce(Vec<KimiAccountSnapshot>, bool),
) -> Result<Result<KimiAccountStore, String>, StoreError> {
    observe_kimi_storage_with(
        account_cache,
        false,
        || {
            repository
                .load()
                .and_then(|state| state.into_writable_for(StoreKind::KimiAccounts))
                .map(|store| {
                    let mut guard = account_cache
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    if guard.storage_failed {
                        // Recovery must publish the validated account membership,
                        // not the empty/older cache from before this read. Keep
                        // last-good quota, but never promote it to fresh quota.
                        guard.snapshots = store
                            .accounts
                            .iter()
                            .map(|record| {
                                guard
                                    .snapshots
                                    .iter()
                                    .find(|snapshot| snapshot.account_id == record.account_id)
                                    .cloned()
                                    .unwrap_or_else(|| {
                                        snapshot_from_stored_quota(record)
                                            .unwrap_or_else(|| empty_snapshot(record, "loading"))
                                    })
                            })
                            .collect();
                        guard.mark_quota_stale();
                    }
                    Ok::<_, String>(store)
                })
        },
        emit,
    )
}

fn mutate_kimi_account_store<R>(
    app: &AppHandle,
    mutation: impl FnOnce(&mut KimiAccountStore) -> R,
) -> Result<R, String> {
    observe_kimi_storage(app, true, || {
        kimi_account_repository(app)
            .mutate(mutation)
            .map(Ok::<_, String>)
    })
    .map_err(crate::storage_services::command_store_error)?
}

fn try_mutate_kimi_account_store<R>(
    app: &AppHandle,
    mutation: impl FnOnce(&mut KimiAccountStore) -> Result<R, String>,
) -> Result<R, String> {
    observe_kimi_storage(app, true, || {
        kimi_account_repository(app).try_mutate(mutation)
    })
    .map_err(crate::storage_services::command_store_error)?
}

static ACCOUNT_CACHE: OnceLock<Mutex<KimiAccountCache>> = OnceLock::new();
static ACCOUNT_REFRESH_GENERATION: AtomicU64 = AtomicU64::new(0);
static LOGIN_GENERATION: AtomicU64 = AtomicU64::new(0);
static CODE_IDENTITY_CACHE: OnceLock<Mutex<HashMap<Uuid, CachedCodeIdentity>>> = OnceLock::new();
static KIMI_ACCOUNT_REPOSITORY_LOCK: Mutex<()> = Mutex::new(());
static ACCOUNT_COMMIT_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static LOGIN_STATE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static ACTIVE_LOGIN: OnceLock<Mutex<Option<ActiveLogin>>> = OnceLock::new();
static VISIBLE_LOGIN_SESSION: OnceLock<Mutex<Option<String>>> = OnceLock::new();
static LOGIN_TASKS: OnceLock<Mutex<HashMap<String, LoginTaskEntry>>> = OnceLock::new();
static BACKGROUND_WEBVIEWS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
static BACKGROUND_SESSION_LEASES: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> =
    OnceLock::new();
static BROWSER_SECRET_CLEANUP_RETRY_TASKS: OnceLock<Mutex<HashSet<Uuid>>> = OnceLock::new();

mod browser;
mod session_migration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum KimiBrowserKind {
    Edge,
    Chrome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KimiBrowserSourceRecord {
    pub source_id: Uuid,
    pub browser: KimiBrowserKind,
    pub profile_name: String,
    pub profile_path: PathBuf,
}

impl KimiBrowserSourceRecord {
    fn safe_label(&self) -> String {
        format!(
            "{} · {}",
            match self.browser {
                KimiBrowserKind::Edge => "Microsoft Edge",
                KimiBrowserKind::Chrome => "Google Chrome",
            },
            self.profile_name
        )
    }
}

#[cfg(windows)]
type CookieReadResult = Result<Option<String>, String>;
#[cfg(windows)]
type CookieReceiver = tokio::sync::oneshot::Receiver<CookieReadResult>;
#[cfg(windows)]
type ScriptTokenReadResult = Result<Option<String>, String>;
#[cfg(windows)]
type ScriptTokenReceiver = tokio::sync::oneshot::Receiver<ScriptTokenReadResult>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KimiAccountRecord {
    pub account_id: Uuid,
    #[serde(default, alias = "session_id", skip_serializing_if = "Option::is_none")]
    pub webview_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub browser_sources: Vec<KimiBrowserSourceRecord>,
    pub identity: KimiIdentityFingerprint,
    pub display_name: String,
    #[serde(default)]
    pub last_quota: Option<KimiStoredQuota>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct KimiStoredQuota {
    pub used_percent: f64,
    pub resets_at: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KimiAccountStore {
    version: u32,
    pub accounts: Vec<KimiAccountRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suppressed_browser_profiles: Vec<String>,
    #[serde(default)]
    pub pending_session_cleanup: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_browser_secret_cleanup: Vec<Uuid>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_browser_secret_provisioning: Vec<Uuid>,
}

/// The previously released V2 schema. Keep this separate from the current
/// type so a V2 reader encounters the version mismatch before it can reject
/// the V3-only provisioning field as an invalid payload.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct KimiAccountStoreV2 {
    version: u32,
    accounts: Vec<KimiAccountRecord>,
    #[serde(default)]
    suppressed_browser_profiles: Vec<String>,
    #[serde(default)]
    pending_session_cleanup: Vec<String>,
    #[serde(default)]
    pending_browser_secret_cleanup: Vec<Uuid>,
}

impl KimiAccountStoreV2 {
    fn into_current(self) -> KimiAccountStore {
        KimiAccountStore {
            version: KIMI_ACCOUNT_STORE_VERSION,
            accounts: self.accounts,
            suppressed_browser_profiles: self.suppressed_browser_profiles,
            pending_session_cleanup: self.pending_session_cleanup,
            pending_browser_secret_cleanup: self.pending_browser_secret_cleanup,
            pending_browser_secret_provisioning: Vec::new(),
        }
    }
}

enum DecodedKimiAccountStore {
    Current(KimiAccountStore),
    Previous(KimiAccountStore),
}

impl Default for KimiAccountStore {
    fn default() -> Self {
        Self {
            version: KIMI_ACCOUNT_STORE_VERSION,
            accounts: Vec::new(),
            suppressed_browser_profiles: Vec::new(),
            pending_session_cleanup: Vec::new(),
            pending_browser_secret_cleanup: Vec::new(),
            pending_browser_secret_provisioning: Vec::new(),
        }
    }
}

#[derive(Clone)]
pub struct KimiAccountRepository {
    paths: StoragePaths,
    user_scope: UserScopeId,
    protection: Protection,
    protector: Arc<dyn SecretProtector>,
    lock_timeout: Duration,
}

impl std::fmt::Debug for KimiAccountRepository {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("KimiAccountRepository")
            .field("store", &StoreKind::KimiAccounts)
            .field("protection", &self.protection)
            .field("lock_timeout", &self.lock_timeout)
            .finish_non_exhaustive()
    }
}

impl KimiAccountRepository {
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
            default_kimi_account_protection(),
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
            lock_timeout: KIMI_ACCOUNT_LOCK_TIMEOUT,
        }
    }

    pub fn path(&self) -> &Path {
        self.paths.kimi_accounts()
    }

    pub fn session_root(&self) -> Result<PathBuf, StoreError> {
        self.path()
            .parent()
            .map(|parent| parent.join("kimi-account-sessions"))
            .ok_or_else(kimi_account_invalid_payload)
    }

    pub fn load(&self) -> Result<LoadState<KimiAccountStore>, StoreError> {
        let _process_guard = KIMI_ACCOUNT_REPOSITORY_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.load_or_migrate_unlocked()
    }

    pub fn mutate<R>(
        &self,
        mutation: impl FnOnce(&mut KimiAccountStore) -> R,
    ) -> Result<R, StoreError> {
        match self.try_mutate(|store| Ok::<R, std::convert::Infallible>(mutation(store)))? {
            Ok(result) => Ok(result),
            Err(never) => match never {},
        }
    }

    pub fn try_mutate<R, E>(
        &self,
        mutation: impl FnOnce(&mut KimiAccountStore) -> Result<R, E>,
    ) -> Result<Result<R, E>, StoreError> {
        let _process_guard = KIMI_ACCOUNT_REPOSITORY_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.paths.ensure_directories()?;
        self.load_or_migrate_unlocked()?
            .into_writable_for(StoreKind::KimiAccounts)?;

        let _store_guard = CrossProcessStoreLock::acquire(
            StoreKind::KimiAccounts,
            &self.user_scope,
            self.paths.kimi_accounts(),
            self.lock_timeout,
        )?;
        let mut store = self
            .load_current_unlocked()
            .into_writable_for(StoreKind::KimiAccounts)?;
        let result = match mutation(&mut store) {
            Ok(result) => result,
            Err(error) => return Ok(Err(error)),
        };
        validate_kimi_account_store(&store, KIMI_ACCOUNT_STORE_VERSION)?;
        self.write_verified(&store)?;

        match self.load_current_unlocked() {
            LoadState::Loaded(read_back) if read_back == store => Ok(Ok(result)),
            LoadState::Loaded(_) => Err(kimi_account_invalid_payload()),
            state => Err(kimi_account_load_state_error(state)),
        }
    }

    fn load_or_migrate_unlocked(&self) -> Result<LoadState<KimiAccountStore>, StoreError> {
        self.paths.ensure_directories()?;
        if self.paths.kimi_accounts().exists() {
            return Ok(self.load_current_or_migrate_previous_unlocked());
        }

        let source = self.paths.legacy_kimi_accounts();
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
        let migrated = match self.decode_legacy_bytes(&raw) {
            Ok(store) => store,
            Err(error) => return Ok(kimi_account_load_state_from_error(error)),
        };

        let session_root = self.session_root()?;
        let _sessions_guard = CrossProcessStoreLock::acquire(
            StoreKind::KimiAccounts,
            &self.user_scope,
            &session_root,
            self.lock_timeout,
        )?;
        if self.paths.kimi_accounts().exists() {
            return Ok(self.load_current_or_migrate_previous_unlocked());
        }
        let legacy_sessions = source
            .parent()
            .ok_or_else(kimi_account_invalid_payload)?
            .join("kimi-account-sessions");
        for session_id in migrated
            .accounts
            .iter()
            .filter_map(|account| account.webview_session_id.as_deref())
        {
            Uuid::parse_str(session_id).map_err(|_| kimi_account_invalid_payload())?;
            session_migration::copy_session_verified(
                &legacy_sessions.join(session_id),
                &session_root.join(session_id),
            )
            .map_err(|error| StoreError::Io {
                store: StoreKind::KimiAccounts,
                operation: StoreOperation::Read,
                reason: StoreFailure::from_io(&error),
            })?;
        }

        let request = MigrationRequest {
            store: StoreKind::KimiAccounts,
            source_path: source,
            source_version: LEGACY_KIMI_ACCOUNT_STORE_VERSION,
            destination_path: self.paths.kimi_accounts().to_path_buf(),
            destination_bytes: self.protected_bytes(&migrated)?,
        };
        let repository = self;
        MigrationCoordinator::new(self.paths.clone(), self.user_scope.clone())
            .migrate_bytes_verified(request, move |bytes| {
                repository.decode_current_bytes(bytes).map(|_| ())
            })?;

        Ok(self.load_current_unlocked())
    }

    fn load_current_or_migrate_previous_unlocked(&self) -> LoadState<KimiAccountStore> {
        let raw = match fs::read(self.paths.kimi_accounts()) {
            Ok(raw) => raw,
            Err(source) if source.kind() == io::ErrorKind::NotFound => return LoadState::Missing,
            Err(source) => {
                return LoadState::IoError {
                    operation: StoreOperation::Read,
                    reason: StoreFailure::from_io(&source),
                };
            }
        };
        match self.decode_current_or_previous_bytes(&raw) {
            Ok(DecodedKimiAccountStore::Current(store)) => LoadState::Loaded(store),
            Ok(DecodedKimiAccountStore::Previous(_)) => {
                match self.migrate_previous_store_in_place_unlocked() {
                    Ok(state) => state,
                    Err(error) => kimi_account_load_state_from_error(error),
                }
            }
            Err(error) => kimi_account_load_state_from_error(error),
        }
    }

    fn load_current_unlocked(&self) -> LoadState<KimiAccountStore> {
        let raw = match fs::read(self.paths.kimi_accounts()) {
            Ok(raw) => raw,
            Err(source) if source.kind() == io::ErrorKind::NotFound => return LoadState::Missing,
            Err(source) => {
                return LoadState::IoError {
                    operation: StoreOperation::Read,
                    reason: StoreFailure::from_io(&source),
                };
            }
        };
        match self.decode_current_bytes(&raw) {
            Ok(store) => LoadState::Loaded(store),
            Err(error) => kimi_account_load_state_from_error(error),
        }
    }

    fn migrate_previous_store_in_place_unlocked(
        &self,
    ) -> Result<LoadState<KimiAccountStore>, StoreError> {
        let _store_guard = CrossProcessStoreLock::acquire(
            StoreKind::KimiAccounts,
            &self.user_scope,
            self.paths.kimi_accounts(),
            self.lock_timeout,
        )?;
        let raw = fs::read(self.paths.kimi_accounts()).map_err(|source| StoreError::Io {
            store: StoreKind::KimiAccounts,
            operation: StoreOperation::Read,
            reason: StoreFailure::from_io(&source),
        })?;
        match self.decode_current_or_previous_bytes(&raw)? {
            DecodedKimiAccountStore::Current(store) => Ok(LoadState::Loaded(store)),
            DecodedKimiAccountStore::Previous(store) => {
                self.write_verified(&store)?;
                match self.load_current_unlocked() {
                    LoadState::Loaded(store) => Ok(LoadState::Loaded(store)),
                    state => Err(kimi_account_load_state_error(state)),
                }
            }
        }
    }

    fn decode_current_or_previous_bytes(
        &self,
        raw: &[u8],
    ) -> Result<DecodedKimiAccountStore, StoreError> {
        let payload = self.decode_payload(raw)?;
        let value = serde_json::from_str::<Value>(payload.trim_start_matches('\u{feff}'))
            .map_err(|_| kimi_account_invalid_payload())?;
        let found = kimi_account_version(&value)?;
        match found {
            KIMI_ACCOUNT_STORE_VERSION => Ok(DecodedKimiAccountStore::Current(
                decode_kimi_account_store_current(value)?,
            )),
            PREVIOUS_KIMI_ACCOUNT_STORE_VERSION => Ok(DecodedKimiAccountStore::Previous(
                decode_kimi_account_store_v2(value)?,
            )),
            _ => Err(StoreError::UnsupportedVersion {
                store: StoreKind::KimiAccounts,
                found,
                supported: KIMI_ACCOUNT_STORE_VERSION,
            }),
        }
    }

    fn decode_current_bytes(&self, raw: &[u8]) -> Result<KimiAccountStore, StoreError> {
        let payload = self.decode_payload(raw)?;
        let value = serde_json::from_str::<Value>(payload.trim_start_matches('\u{feff}'))
            .map_err(|_| kimi_account_invalid_payload())?;
        let found = kimi_account_version(&value)?;
        if found != KIMI_ACCOUNT_STORE_VERSION {
            return Err(StoreError::UnsupportedVersion {
                store: StoreKind::KimiAccounts,
                found,
                supported: KIMI_ACCOUNT_STORE_VERSION,
            });
        }
        decode_kimi_account_store_current(value)
    }

    fn decode_legacy_bytes(&self, raw: &[u8]) -> Result<KimiAccountStore, StoreError> {
        let payload = self.decode_payload(raw)?;
        let value = serde_json::from_str::<Value>(payload.trim_start_matches('\u{feff}'))
            .map_err(|_| kimi_account_invalid_payload())?;
        let found = kimi_account_version(&value)?;
        if found != LEGACY_KIMI_ACCOUNT_STORE_VERSION {
            return Err(StoreError::UnsupportedVersion {
                store: StoreKind::KimiAccounts,
                found,
                supported: LEGACY_KIMI_ACCOUNT_STORE_VERSION,
            });
        }
        let mut store: KimiAccountStore =
            serde_json::from_value(value).map_err(|_| kimi_account_invalid_payload())?;
        validate_kimi_account_store(&store, LEGACY_KIMI_ACCOUNT_STORE_VERSION)?;
        store.version = KIMI_ACCOUNT_STORE_VERSION;
        validate_kimi_account_store(&store, KIMI_ACCOUNT_STORE_VERSION)?;
        Ok(store)
    }

    fn decode_payload(&self, raw: &[u8]) -> Result<String, StoreError> {
        let secure_envelope = kimi_account_raw_is_secure_envelope(raw);
        let payload = decode_string_bytes_with(raw, self.protector.as_ref()).map_err(|source| {
            if secure_envelope {
                if source.kind() == io::ErrorKind::InvalidData {
                    StoreError::CorruptEnvelope {
                        store: StoreKind::KimiAccounts,
                        reason: StoreFailure::from_io(&source),
                    }
                } else {
                    let reason = if source.kind() == io::ErrorKind::Unsupported {
                        StoreFailure::new(StoreFailureKind::UnsupportedProtection, None)
                    } else {
                        StoreFailure::from_io(&source)
                    };
                    StoreError::DecryptFailed {
                        store: StoreKind::KimiAccounts,
                        reason,
                    }
                }
            } else {
                kimi_account_invalid_payload()
            }
        })?;
        if secure_envelope && payload.as_bytes() == raw {
            return Err(StoreError::CorruptEnvelope {
                store: StoreKind::KimiAccounts,
                reason: StoreFailure::new(StoreFailureKind::InvalidData, None),
            });
        }
        Ok(payload)
    }

    fn protected_bytes(&self, store: &KimiAccountStore) -> Result<Vec<u8>, StoreError> {
        validate_kimi_account_store(store, KIMI_ACCOUNT_STORE_VERSION)?;
        let json = serde_json::to_string(store).map_err(|_| StoreError::Io {
            store: StoreKind::KimiAccounts,
            operation: StoreOperation::Serialize,
            reason: StoreFailure::new(StoreFailureKind::InvalidData, None),
        })?;
        protected_file_bytes_with(&json, self.protection, self.protector.as_ref()).map_err(
            |source| StoreError::Io {
                store: StoreKind::KimiAccounts,
                operation: StoreOperation::Protect,
                reason: StoreFailure::from_io(&source),
            },
        )
    }

    fn write_verified(&self, store: &KimiAccountStore) -> Result<(), StoreError> {
        let protected = self.protected_bytes(store)?;
        write_bytes_verified_for(
            StoreKind::KimiAccounts,
            self.paths.kimi_accounts(),
            &protected,
            &RealAtomicFileOps,
        )?;
        restrict_storage_file(self.paths.kimi_accounts()).map_err(|source| StoreError::Io {
            store: StoreKind::KimiAccounts,
            operation: StoreOperation::ApplyPermissions,
            reason: StoreFailure::from_io(&source),
        })
    }
}

fn decode_kimi_account_store_current(value: Value) -> Result<KimiAccountStore, StoreError> {
    let store: KimiAccountStore =
        serde_json::from_value(value).map_err(|_| kimi_account_invalid_payload())?;
    validate_kimi_account_store(&store, KIMI_ACCOUNT_STORE_VERSION)?;
    Ok(store)
}

fn decode_kimi_account_store_v2(value: Value) -> Result<KimiAccountStore, StoreError> {
    let store: KimiAccountStoreV2 =
        serde_json::from_value(value).map_err(|_| kimi_account_invalid_payload())?;
    if store.version != PREVIOUS_KIMI_ACCOUNT_STORE_VERSION {
        return Err(kimi_account_invalid_payload());
    }
    let store = store.into_current();
    validate_kimi_account_store(&store, KIMI_ACCOUNT_STORE_VERSION)?;
    Ok(store)
}

#[cfg(windows)]
const fn default_kimi_account_protection() -> Protection {
    Protection::CurrentUserDpapi
}

#[cfg(not(windows))]
const fn default_kimi_account_protection() -> Protection {
    Protection::Plaintext
}

fn kimi_account_version(value: &Value) -> Result<u32, StoreError> {
    value
        .as_object()
        .and_then(|object| object.get("version"))
        .and_then(Value::as_u64)
        .and_then(|version| u32::try_from(version).ok())
        .ok_or_else(kimi_account_invalid_payload)
}

fn validate_kimi_account_store(
    store: &KimiAccountStore,
    expected_version: u32,
) -> Result<(), StoreError> {
    if store.version != expected_version {
        return Err(kimi_account_invalid_payload());
    }
    let mut account_ids = HashSet::new();
    let mut session_ids = HashSet::new();
    let mut source_ids = HashSet::new();
    let mut identities = HashSet::new();
    for account in &store.accounts {
        if !account_ids.insert(account.account_id)
            || account.identity.user_id_fingerprint.trim().is_empty()
            || account.identity.global_id_fingerprint.trim().is_empty()
            || !identities.insert((
                account.identity.user_id_fingerprint.clone(),
                account.identity.global_id_fingerprint.clone(),
            ))
            || sanitize_display_name(Some(account.display_name.clone())).as_deref()
                != Some(account.display_name.as_str())
        {
            return Err(kimi_account_invalid_payload());
        }
        if let Some(session_id) = &account.webview_session_id
            && (session_id.trim().is_empty() || !session_ids.insert(session_id.clone()))
        {
            return Err(kimi_account_invalid_payload());
        }
        for source in &account.browser_sources {
            if !source_ids.insert(source.source_id)
                || source.profile_name.trim().is_empty()
                || source.profile_name.chars().any(char::is_control)
            {
                return Err(kimi_account_invalid_payload());
            }
        }
        if let Some(quota) = &account.last_quota
            && !quota.used_percent.is_finite()
        {
            return Err(kimi_account_invalid_payload());
        }
    }
    if has_duplicates(&store.suppressed_browser_profiles)
        || has_duplicates(&store.pending_session_cleanup)
        || has_duplicates(&store.pending_browser_secret_cleanup)
        || has_duplicates(&store.pending_browser_secret_provisioning)
        || store
            .pending_browser_secret_provisioning
            .iter()
            .any(|source_id| {
                source_ids.contains(source_id)
                    || store.pending_browser_secret_cleanup.contains(source_id)
            })
    {
        return Err(kimi_account_invalid_payload());
    }
    let value = serde_json::to_value(store).map_err(|_| kimi_account_invalid_payload())?;
    let round_trip: KimiAccountStore =
        serde_json::from_value(value).map_err(|_| kimi_account_invalid_payload())?;
    if round_trip != *store {
        return Err(kimi_account_invalid_payload());
    }
    Ok(())
}

fn has_duplicates<T: Eq + Hash>(values: &[T]) -> bool {
    let mut unique = HashSet::new();
    values.iter().any(|value| !unique.insert(value))
}

fn kimi_account_raw_is_secure_envelope(raw: &[u8]) -> bool {
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

fn kimi_account_load_state_from_error(error: StoreError) -> LoadState<KimiAccountStore> {
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

fn kimi_account_load_state_error(state: LoadState<KimiAccountStore>) -> StoreError {
    match state {
        LoadState::Missing => StoreError::Io {
            store: StoreKind::KimiAccounts,
            operation: StoreOperation::ReadBack,
            reason: StoreFailure::new(StoreFailureKind::NotFound, None),
        },
        LoadState::Loaded(_) => kimi_account_invalid_payload(),
        LoadState::NeedsMigration { source_version, .. } => StoreError::MigrationRequired {
            store: StoreKind::KimiAccounts,
            source_version,
        },
        LoadState::CorruptEnvelope { reason } => StoreError::CorruptEnvelope {
            store: StoreKind::KimiAccounts,
            reason,
        },
        LoadState::DecryptFailed { reason } => StoreError::DecryptFailed {
            store: StoreKind::KimiAccounts,
            reason,
        },
        LoadState::InvalidPayload { reason } => StoreError::InvalidPayload {
            store: StoreKind::KimiAccounts,
            reason,
        },
        LoadState::UnsupportedVersion { found, supported } => StoreError::UnsupportedVersion {
            store: StoreKind::KimiAccounts,
            found,
            supported,
        },
        LoadState::Locked { reason } => StoreError::Locked {
            store: StoreKind::KimiAccounts,
            reason,
        },
        LoadState::IoError { operation, reason } => StoreError::Io {
            store: StoreKind::KimiAccounts,
            operation,
            reason,
        },
    }
}

fn kimi_account_invalid_payload() -> StoreError {
    StoreError::InvalidPayload {
        store: StoreKind::KimiAccounts,
        reason: StoreFailure::new(StoreFailureKind::InvalidData, None),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KimiAccountUpsert {
    pub account_id: Uuid,
    pub replaced_session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KimiAccountSnapshot {
    pub account_id: Uuid,
    pub display_name: String,
    pub used_percent: Option<f64>,
    pub resets_at: Option<String>,
    pub updated_at: Option<String>,
    pub status: &'static str,
    pub matched_credential_ids: Vec<Uuid>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_labels: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KimiBrowserScanResult {
    pub accounts: Vec<KimiAccountSnapshot>,
    pub discovered: usize,
    pub refreshed: usize,
    pub failed: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KimiLoginSession {
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KimiLoginCompletion {
    pub status: &'static str,
    pub snapshot: Option<KimiAccountSnapshot>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct KimiLoginCompletionEvent {
    session_id: String,
    status: &'static str,
    snapshot: Option<KimiAccountSnapshot>,
    error: Option<String>,
}

#[derive(Debug, Clone)]
struct CachedCodeIdentity {
    secret_revision: u64,
    identity: KimiIdentityFingerprint,
}

#[derive(Debug, Clone)]
struct ActiveLogin {
    session_id: String,
    expected_account_id: Option<Uuid>,
    generation: u64,
}

#[derive(Debug, Clone)]
enum LoginTaskOutcome {
    Completed(KimiAccountSnapshot),
    Failed(String),
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum LoginTaskPhase {
    Running = 0,
    Committing = 1,
    Finished = 2,
    Cancelled = 3,
}

#[derive(Debug, Clone)]
struct LoginTaskEntry {
    generation: u64,
    cancel: tokio::sync::watch::Sender<bool>,
    outcome: tokio::sync::watch::Receiver<Option<LoginTaskOutcome>>,
    phase: Arc<AtomicU8>,
    close_after_finish: Arc<AtomicBool>,
    abort: tokio::task::AbortHandle,
}

#[derive(Debug, Clone)]
struct RequestedLoginCancellation {
    active: Option<ActiveLogin>,
    task: Option<LoginTaskEntry>,
    accepted: bool,
}

struct BackgroundWebviewGuard {
    app: AppHandle,
    label: String,
}

struct BackgroundSessionLease {
    session_id: String,
    mutex: Arc<tokio::sync::Mutex<()>>,
}

impl BackgroundSessionLease {
    async fn lock(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.mutex.lock().await
    }
}

impl Drop for BackgroundSessionLease {
    fn drop(&mut self) {
        let mut leases = background_session_leases()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if Arc::strong_count(&self.mutex) == 2
            && leases
                .get(&self.session_id)
                .is_some_and(|current| Arc::ptr_eq(current, &self.mutex))
        {
            leases.remove(&self.session_id);
        }
    }
}

impl Drop for BackgroundWebviewGuard {
    fn drop(&mut self) {
        if let Some(webview) = self.app.get_webview(&self.label) {
            let _ = webview.close();
        }
        let app = self.app.clone();
        let label = self.label.clone();
        tauri::async_runtime::spawn(async move {
            if wait_for_webview_destroyed(&app, &label).await
                && let Ok(mut labels) = background_webviews().lock()
            {
                labels.remove(&label);
            }
        });
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KimiAccountRefreshError {
    Authentication,
    MissingCredential,
    Temporary,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CancellableWaitError {
    Cancelled,
    TimedOut,
}

async fn wait_with_cancel<F, T>(
    future: F,
    cancel: &mut tokio::sync::watch::Receiver<bool>,
    timeout: std::time::Duration,
) -> Result<T, CancellableWaitError>
where
    F: std::future::Future<Output = T>,
{
    if *cancel.borrow() {
        return Err(CancellableWaitError::Cancelled);
    }

    tokio::select! {
        biased;
        _ = cancel.changed() => Err(CancellableWaitError::Cancelled),
        result = tokio::time::timeout(timeout, future) => {
            result.map_err(|_| CancellableWaitError::TimedOut)
        }
    }
}

async fn wait_for_login_outcome(
    outcome: &mut tokio::sync::watch::Receiver<Option<LoginTaskOutcome>>,
    budget: std::time::Duration,
) -> Option<LoginTaskOutcome> {
    if let Some(result) = outcome.borrow().clone() {
        return Some(result);
    }
    tokio::time::timeout(budget, async {
        loop {
            outcome.changed().await.ok()?;
            if let Some(result) = outcome.borrow().clone() {
                return Some(result);
            }
        }
    })
    .await
    .ok()
    .flatten()
}

impl KimiAccountRefreshError {
    fn user_message(self) -> &'static str {
        match self {
            Self::Authentication => "Kimi 登录状态已失效，请重新登录",
            Self::MissingCredential => "未能从 Kimi 登录窗口读取登录凭据，请确认登录完成后重试",
            Self::Temporary => "暂时无法读取 Kimi 账号额度，请稍后重试",
            Self::Cancelled => "Kimi 账号读取已取消",
        }
    }
}

impl KimiAccountStore {
    pub fn upsert_authenticated(
        &mut self,
        session_id: String,
        identity: KimiIdentityFingerprint,
        display_name: Option<String>,
    ) -> KimiAccountUpsert {
        let safe_name = sanitize_display_name(display_name).unwrap_or_else(|| "Kimi 账号".into());
        if let Some(existing) = self
            .accounts
            .iter_mut()
            .find(|account| account.identity.strictly_matches(&identity))
        {
            let replaced_session_id =
                if existing.webview_session_id.as_deref() == Some(session_id.as_str()) {
                    None
                } else {
                    existing.webview_session_id.replace(session_id)
                };
            existing.display_name = safe_name;
            return KimiAccountUpsert {
                account_id: existing.account_id,
                replaced_session_id,
            };
        }

        let account_id = Uuid::new_v4();
        self.accounts.push(KimiAccountRecord {
            account_id,
            webview_session_id: Some(session_id),
            browser_sources: Vec::new(),
            identity,
            display_name: safe_name,
            last_quota: None,
        });
        KimiAccountUpsert {
            account_id,
            replaced_session_id: None,
        }
    }

    pub fn upsert_browser_source(
        &mut self,
        identity: KimiIdentityFingerprint,
        display_name: String,
        source: KimiBrowserSourceRecord,
    ) -> KimiAccountUpsert {
        let safe_name =
            sanitize_display_name(Some(display_name)).unwrap_or_else(|| "Kimi 账号".into());
        if let Some(existing) = self
            .accounts
            .iter_mut()
            .find(|account| account.identity.strictly_matches(&identity))
        {
            if let Some(current) = existing
                .browser_sources
                .iter_mut()
                .find(|current| current.source_id == source.source_id)
            {
                *current = source;
            } else {
                existing.browser_sources.push(source);
            }
            existing.display_name = safe_name;
            return KimiAccountUpsert {
                account_id: existing.account_id,
                replaced_session_id: None,
            };
        }

        let account_id = Uuid::new_v4();
        self.accounts.push(KimiAccountRecord {
            account_id,
            webview_session_id: None,
            browser_sources: vec![source],
            identity,
            display_name: safe_name,
            last_quota: None,
        });
        KimiAccountUpsert {
            account_id,
            replaced_session_id: None,
        }
    }

    pub fn account_id_for_identity(&self, identity: &KimiIdentityFingerprint) -> Option<Uuid> {
        self.accounts
            .iter()
            .find(|account| account.identity.strictly_matches(identity))
            .map(|account| account.account_id)
    }
}

fn sanitize_display_name(name: Option<String>) -> Option<String> {
    let name = name?.trim().to_string();
    (!name.is_empty() && name.len() <= 80 && !name.chars().any(char::is_control)).then_some(name)
}

pub fn is_allowed_kimi_navigation(url: &Url) -> bool {
    if url.scheme() != "https" {
        return false;
    }
    let Some(host) = url.host_str().map(str::to_ascii_lowercase) else {
        return false;
    };
    host == "kimi.com"
        || host.ends_with(".kimi.com")
        || host == "moonshot.cn"
        || host.ends_with(".moonshot.cn")
}

fn cache() -> &'static Mutex<KimiAccountCache> {
    ACCOUNT_CACHE.get_or_init(|| Mutex::new(KimiAccountCache::default()))
}

fn code_identity_cache() -> &'static Mutex<HashMap<Uuid, CachedCodeIdentity>> {
    CODE_IDENTITY_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn account_commit_lock() -> &'static Mutex<()> {
    ACCOUNT_COMMIT_LOCK.get_or_init(|| Mutex::new(()))
}

pub(crate) fn lock_account_commit() -> Result<std::sync::MutexGuard<'static, ()>, String> {
    account_commit_lock()
        .lock()
        .map_err(|_| "Kimi account commit state is unavailable".to_string())
}

fn login_state_lock() -> &'static Mutex<()> {
    LOGIN_STATE_LOCK.get_or_init(|| Mutex::new(()))
}

fn active_login() -> &'static Mutex<Option<ActiveLogin>> {
    ACTIVE_LOGIN.get_or_init(|| Mutex::new(None))
}

fn visible_login_session() -> &'static Mutex<Option<String>> {
    VISIBLE_LOGIN_SESSION.get_or_init(|| Mutex::new(None))
}

fn visible_login_owns_session(session_id: &str) -> bool {
    visible_login_session()
        .lock()
        .ok()
        .and_then(|session| session.clone())
        .is_some_and(|current| current == session_id)
}

fn login_tasks() -> &'static Mutex<HashMap<String, LoginTaskEntry>> {
    LOGIN_TASKS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn background_webviews() -> &'static Mutex<HashSet<String>> {
    BACKGROUND_WEBVIEWS.get_or_init(|| Mutex::new(HashSet::new()))
}

fn background_session_leases() -> &'static Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>> {
    BACKGROUND_SESSION_LEASES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn background_session_lease(session_id: &str) -> BackgroundSessionLease {
    let mut leases = background_session_leases()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mutex = Arc::clone(
        leases
            .entry(session_id.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
    );
    BackgroundSessionLease {
        session_id: session_id.to_string(),
        mutex,
    }
}

async fn wait_for_webview_destroyed(app: &AppHandle, label: &str) -> bool {
    let deadline = tokio::time::Instant::now() + BACKGROUND_WEBVIEW_SHUTDOWN_BUDGET;
    while app.get_webview(label).is_some() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    app.get_webview(label).is_none()
}

async fn wait_for_account_refresh_invalidation(generation: u64) {
    while is_current_account_refresh(generation) {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

fn login_task_phase(phase: &AtomicU8) -> LoginTaskPhase {
    match phase.load(Ordering::SeqCst) {
        value if value == LoginTaskPhase::Running as u8 => LoginTaskPhase::Running,
        value if value == LoginTaskPhase::Committing as u8 => LoginTaskPhase::Committing,
        value if value == LoginTaskPhase::Finished as u8 => LoginTaskPhase::Finished,
        _ => LoginTaskPhase::Cancelled,
    }
}

fn claim_login_cancellation(phase: &AtomicU8) -> bool {
    phase
        .compare_exchange(
            LoginTaskPhase::Running as u8,
            LoginTaskPhase::Cancelled as u8,
            Ordering::SeqCst,
            Ordering::SeqCst,
        )
        .is_ok()
}

fn begin_login_commit(phase: &AtomicU8) -> bool {
    phase
        .compare_exchange(
            LoginTaskPhase::Running as u8,
            LoginTaskPhase::Committing as u8,
            Ordering::SeqCst,
            Ordering::SeqCst,
        )
        .is_ok()
}

fn finish_login_phase(phase: &AtomicU8) -> bool {
    loop {
        let current = login_task_phase(phase);
        match current {
            LoginTaskPhase::Cancelled => return false,
            LoginTaskPhase::Finished => return true,
            LoginTaskPhase::Running | LoginTaskPhase::Committing => {
                if phase
                    .compare_exchange(
                        current as u8,
                        LoginTaskPhase::Finished as u8,
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                    )
                    .is_ok()
                {
                    return true;
                }
            }
        }
    }
}

fn take_active_login(session_id: &str) -> Option<ActiveLogin> {
    active_login().lock().ok().and_then(|mut active| {
        (active.as_ref()?.session_id == session_id)
            .then(|| active.take())
            .flatten()
    })
}

fn take_active_login_generation(session_id: &str, generation: u64) -> Option<ActiveLogin> {
    active_login().lock().ok().and_then(|mut active| {
        let current = active.as_ref()?;
        (current.session_id == session_id && current.generation == generation)
            .then(|| active.take())
            .flatten()
    })
}

fn is_login_session_current(session_id: &str, generation: u64) -> bool {
    let active_current = active_login()
        .lock()
        .ok()
        .and_then(|active| active.clone())
        .is_some_and(|active| active.session_id == session_id && active.generation == generation);
    let task_current = login_tasks()
        .lock()
        .ok()
        .and_then(|tasks| tasks.get(session_id).cloned())
        .is_some_and(|task| {
            task.generation == generation
                && login_task_phase(&task.phase) != LoginTaskPhase::Cancelled
        });
    active_current && task_current
}

fn merge_credential_matches(latest: &mut [KimiAccountSnapshot], matched: &[KimiAccountSnapshot]) {
    for snapshot in latest {
        if let Some(candidate) = matched
            .iter()
            .find(|candidate| candidate.account_id == snapshot.account_id)
        {
            snapshot.matched_credential_ids = candidate.matched_credential_ids.clone();
        }
    }
}

fn add_matched_credential(snapshot: &mut KimiAccountSnapshot, credential_id: Uuid) {
    if !snapshot.matched_credential_ids.contains(&credential_id) {
        snapshot.matched_credential_ids.push(credential_id);
    }
}

fn credential_match_publish_is_valid(
    expected_generation: u64,
    current_generation: u64,
    expected_store: &KimiAccountStore,
    current_store: &KimiAccountStore,
    enabled: bool,
) -> bool {
    enabled
        && expected_generation == current_generation
        && same_account_revision(expected_store, current_store)
}

fn same_credential_revisions(expected: &HashMap<Uuid, u64>, current: &HashMap<Uuid, u64>) -> bool {
    expected == current
}

fn begin_account_refresh() -> u64 {
    ACCOUNT_REFRESH_GENERATION.fetch_add(1, Ordering::SeqCst) + 1
}

fn invalidate_account_refresh() {
    ACCOUNT_REFRESH_GENERATION.fetch_add(1, Ordering::SeqCst);
}

fn is_current_account_refresh(generation: u64) -> bool {
    ACCOUNT_REFRESH_GENERATION.load(Ordering::SeqCst) == generation
}

fn secret_revision(secret: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    secret.hash(&mut hasher);
    hasher.finish()
}

fn background_webview_label(session_id: &str) -> String {
    format!("{KIMI_WEBVIEW_PREFIX}{session_id}-{}", Uuid::new_v4())
}

fn background_webview_prefix(session_id: &str) -> String {
    format!("{KIMI_WEBVIEW_PREFIX}{session_id}-")
}

fn session_root() -> Result<PathBuf, String> {
    KimiAccountRepository::discover()
        .and_then(|repository| repository.session_root())
        .map_err(crate::storage_services::command_store_error)
}

fn session_path(session_id: &str) -> Result<PathBuf, String> {
    if Uuid::parse_str(session_id).is_err() {
        return Err("Kimi session id is invalid".into());
    }
    Ok(session_root()?.join(session_id))
}

fn orphaned_session_ids(
    entries: impl IntoIterator<Item = String>,
    persisted_session_ids: &[String],
) -> Vec<String> {
    entries
        .into_iter()
        .filter(|session_id| Uuid::parse_str(session_id).is_ok())
        .filter(|session_id| {
            !persisted_session_ids
                .iter()
                .any(|saved| saved == session_id)
        })
        .collect()
}

#[cfg(test)]
fn select_auth_token<'a>(cookies: impl IntoIterator<Item = (&'a str, &'a str)>) -> Option<String> {
    cookies
        .into_iter()
        .find(|(name, value)| is_auth_cookie_name(name) && !value.trim().is_empty())
        .map(|(_, value)| value.trim().to_string())
}

fn is_auth_cookie_name(name: &str) -> bool {
    matches!(name, "kimi-auth" | "authorization" | "access_token")
}

fn parse_access_token_script_result(result: &str) -> Result<Option<String>, String> {
    serde_json::from_str::<Option<String>>(result)
        .map(|token| {
            token
                .map(|token| token.trim().to_string())
                .filter(|token| !token.is_empty())
        })
        .map_err(|_| "Kimi access token result could not be parsed".to_string())
}

fn missing_webview_error() -> KimiAccountRefreshError {
    KimiAccountRefreshError::Temporary
}

fn missing_auth_token_error() -> KimiAccountRefreshError {
    KimiAccountRefreshError::MissingCredential
}

#[derive(Default)]
struct KimiAccountCache {
    snapshots: Vec<KimiAccountSnapshot>,
    storage_failed: bool,
    write_failed: bool,
    fault_revision: u64,
}

impl KimiAccountCache {
    fn fail_storage(&mut self, write: bool) {
        self.storage_failed = true;
        self.write_failed |= write;
        self.fault_revision += 1;
        self.mark_quota_stale();
    }

    fn mark_quota_stale(&mut self) {
        for snapshot in &mut self.snapshots {
            if snapshot.status == "ok" {
                snapshot.status = "stale";
            }
        }
    }

    fn presented(&self) -> Vec<KimiAccountSnapshot> {
        self.present(self.snapshots.clone())
    }

    fn present(&self, mut snapshots: Vec<KimiAccountSnapshot>) -> Vec<KimiAccountSnapshot> {
        if self.storage_failed {
            for snapshot in &mut snapshots {
                snapshot.status = "storageError";
            }
        }
        snapshots
    }

    fn storage_succeeded(&mut self, started_at: u64, write: bool) -> bool {
        // A read cannot prove that a failed write is now possible. An operation
        // started before a newer fault cannot acknowledge that newer fault.
        if !self.storage_failed
            || started_at != self.fault_revision
            || (self.write_failed && !write)
        {
            return false;
        }
        self.storage_failed = false;
        self.write_failed = false;
        true
    }
}

fn publish_cache(app: &AppHandle, snapshots: Vec<KimiAccountSnapshot>) -> Vec<KimiAccountSnapshot> {
    publish_cache_with(cache(), snapshots, |snapshots, failed| {
        emit_kimi_state(app, snapshots, failed)
    })
}

fn publish_cache_with(
    account_cache: &Mutex<KimiAccountCache>,
    mut snapshots: Vec<KimiAccountSnapshot>,
    emit: impl FnOnce(Vec<KimiAccountSnapshot>, bool),
) -> Vec<KimiAccountSnapshot> {
    let mut guard = account_cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if snapshots
        .iter()
        .any(|snapshot| snapshot.status == "storageError")
    {
        guard.fail_storage(true);
        for snapshot in &mut snapshots {
            if snapshot.status == "storageError" {
                snapshot.status = if snapshot.used_percent.is_some() {
                    "stale"
                } else {
                    "refreshFailed"
                };
            }
        }
    }
    guard.snapshots = snapshots;
    if guard.storage_failed {
        guard.mark_quota_stale();
    }
    let presented = guard.presented();
    // Serialize both events with the state update; two publishers must not
    // interleave an old healthy event after a newer failure event.
    emit(presented.clone(), guard.storage_failed);
    presented
}

fn observe_kimi_storage<R, E>(
    app: &AppHandle,
    write: bool,
    operation: impl FnOnce() -> Result<Result<R, E>, StoreError>,
) -> Result<Result<R, E>, StoreError> {
    observe_kimi_storage_with(cache(), write, operation, |snapshots, failed| {
        emit_kimi_state(app, snapshots, failed);
    })
}

fn observe_kimi_storage_with<R, E>(
    account_cache: &Mutex<KimiAccountCache>,
    write: bool,
    operation: impl FnOnce() -> Result<Result<R, E>, StoreError>,
    emit: impl FnOnce(Vec<KimiAccountSnapshot>, bool),
) -> Result<Result<R, E>, StoreError> {
    let revision = account_cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .fault_revision;
    // Never hold the presentation lock during repository I/O or external work.
    let result = operation();
    let mut guard = account_cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let changed = match &result {
        Ok(Ok(_)) => guard.storage_succeeded(revision, write),
        Ok(Err(_)) => false, // A cancelled/conflicting mutation did not commit.
        Err(_) => {
            guard.fail_storage(write);
            true
        }
    };
    if changed {
        emit(guard.presented(), guard.storage_failed);
    }
    result
}

#[derive(Clone, Serialize)]
struct KimiStorageState {
    failed: bool,
}

fn emit_kimi_state(app: &AppHandle, snapshots: Vec<KimiAccountSnapshot>, failed: bool) {
    let _ = app.emit(KIMI_ACCOUNT_EVENT, snapshots);
    let _ = app.emit("kimi-account-storage-state", KimiStorageState { failed });
}

fn login_completion_targets() -> [&'static str; 2] {
    ["settings", "main"]
}

fn emit_login_completion(app: &AppHandle, event: KimiLoginCompletionEvent) {
    for target in login_completion_targets() {
        let _ = app.emit_to(target, KIMI_LOGIN_COMPLETION_EVENT, event.clone());
    }
}

pub fn invalidate_credential_matches(app: &AppHandle, credential_id: Option<Uuid>) {
    invalidate_account_refresh();
    if let Ok(mut identities) = code_identity_cache().lock() {
        if let Some(credential_id) = credential_id {
            identities.remove(&credential_id);
        } else {
            identities.clear();
        }
    }
    if let Ok(mut guard) = cache().lock() {
        for snapshot in &mut guard.snapshots {
            if let Some(credential_id) = credential_id {
                snapshot
                    .matched_credential_ids
                    .retain(|id| *id != credential_id);
            } else {
                snapshot.matched_credential_ids.clear();
            }
        }
        emit_kimi_state(app, guard.presented(), guard.storage_failed);
    }
}

fn snapshot_for_record(record: &KimiAccountRecord) -> KimiAccountSnapshot {
    cache()
        .lock()
        .ok()
        .and_then(|guard| {
            guard
                .snapshots
                .iter()
                .find(|snapshot| snapshot.account_id == record.account_id)
                .cloned()
        })
        .unwrap_or_else(|| {
            snapshot_from_stored_quota(record).unwrap_or_else(|| empty_snapshot(record, "loading"))
        })
}

fn empty_snapshot(record: &KimiAccountRecord, status: &'static str) -> KimiAccountSnapshot {
    KimiAccountSnapshot {
        account_id: record.account_id,
        display_name: record.display_name.clone(),
        used_percent: None,
        resets_at: None,
        updated_at: None,
        status,
        matched_credential_ids: Vec::new(),
        source_labels: browser_source_labels(record),
    }
}

fn snapshot_from_stored_quota(record: &KimiAccountRecord) -> Option<KimiAccountSnapshot> {
    snapshot_from_stored_quota_with_status(record, "stale")
}

fn snapshot_from_stored_quota_with_status(
    record: &KimiAccountRecord,
    status: &'static str,
) -> Option<KimiAccountSnapshot> {
    let quota = record.last_quota.as_ref()?;
    Some(KimiAccountSnapshot {
        account_id: record.account_id,
        display_name: record.display_name.clone(),
        used_percent: Some(quota.used_percent),
        resets_at: quota.resets_at.clone(),
        updated_at: Some(quota.updated_at.clone()),
        status,
        matched_credential_ids: Vec::new(),
        source_labels: browser_source_labels(record),
    })
}

fn snapshot_after_refresh_error(
    record: &KimiAccountRecord,
    previous: Option<KimiAccountSnapshot>,
    error: &KimiAccountRefreshError,
) -> KimiAccountSnapshot {
    if *error == KimiAccountRefreshError::Authentication {
        return empty_snapshot(record, "loginRequired");
    }

    if let Some(mut snapshot) = previous
        .filter(|snapshot| snapshot.status != "loginRequired" && snapshot.used_percent.is_some())
        .or_else(|| snapshot_from_stored_quota(record))
    {
        snapshot.display_name = record.display_name.clone();
        snapshot.source_labels = browser_source_labels(record);
        snapshot.status = "stale";
        snapshot.matched_credential_ids.clear();
        return snapshot;
    }

    empty_snapshot(record, "refreshFailed")
}

fn browser_source_labels(record: &KimiAccountRecord) -> Vec<String> {
    record
        .browser_sources
        .iter()
        .map(KimiBrowserSourceRecord::safe_label)
        .collect()
}

fn classify_provider_error(error: &ProviderError) -> KimiAccountRefreshError {
    match error {
        ProviderError::AuthRequired | ProviderError::NoCookies => {
            KimiAccountRefreshError::Authentication
        }
        _ => KimiAccountRefreshError::Temporary,
    }
}

fn create_background_account_webview(
    app: &AppHandle,
    session_id: &str,
) -> Result<BackgroundWebviewGuard, String> {
    let label = background_webview_label(session_id);
    let settings = crate::shell::settings_window::ensure_hidden(app)?;
    let window = settings.as_ref().window().clone();
    let url = Url::parse(KIMI_LOGIN_URL).map_err(|error| error.to_string())?;
    let builder = WebviewBuilder::new(label.clone(), WebviewUrl::External(url))
        .data_directory(session_path(session_id)?)
        .on_navigation(is_allowed_kimi_navigation)
        .on_new_window(|_, _| NewWindowResponse::Deny);
    let webview = window
        .add_child(
            builder,
            LogicalPosition::new(0.0, 0.0),
            LogicalSize::new(1.0, 1.0),
        )
        .map_err(|error| error.to_string())?;
    let _ = webview.hide();
    background_webviews()
        .lock()
        .map_err(|_| "Kimi background WebView state is unavailable".to_string())?
        .insert(label.clone());
    let guard = BackgroundWebviewGuard {
        app: app.clone(),
        label,
    };
    Ok(guard)
}

#[cfg(windows)]
fn begin_access_token_script_request(
    webview: tauri::Webview,
) -> Result<ScriptTokenReceiver, String> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let sender = Arc::new(Mutex::new(Some(sender)));
    let scheduled_sender = Arc::clone(&sender);
    let schedule_result = webview.with_webview(move |platform_webview| {
        let callback_sender = Arc::clone(&scheduled_sender);
        let start_result = (|| -> windows_core::Result<()> {
            let core_webview = unsafe { platform_webview.controller().CoreWebView2()? };
            let script = HSTRING::from("window.localStorage.getItem('access_token')");
            let handler =
                ExecuteScriptCompletedHandler::create(Box::new(move |error_code, result| {
                    let result = match error_code {
                        Ok(()) => parse_access_token_script_result(&result),
                        Err(error) => Err(error.to_string()),
                    };
                    if let Ok(mut slot) = callback_sender.lock()
                        && let Some(sender) = slot.take()
                    {
                        let _ = sender.send(result);
                    }
                    Ok(())
                }));
            unsafe {
                core_webview.ExecuteScript(&script, &handler)?;
            }
            Ok(())
        })();

        if let Err(error) = start_result
            && let Ok(mut slot) = scheduled_sender.lock()
            && let Some(sender) = slot.take()
        {
            let _ = sender.send(Err(error.to_string()));
        }
    });

    if let Err(error) = schedule_result
        && let Ok(mut slot) = sender.lock()
        && let Some(sender) = slot.take()
    {
        let _ = sender.send(Err(error.to_string()));
    }
    Ok(receiver)
}

#[cfg(windows)]
fn begin_cookie_request(webview: tauri::Webview) -> Result<CookieReceiver, String> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let sender = Arc::new(Mutex::new(Some(sender)));
    let scheduled_sender = Arc::clone(&sender);
    let schedule_result = webview.with_webview(move |platform_webview| {
        let callback_sender = Arc::clone(&scheduled_sender);
        let start_result = (|| -> windows_core::Result<()> {
            let core_webview = unsafe { platform_webview.controller().CoreWebView2()? };
            let core_webview = core_webview.cast::<ICoreWebView2_2>()?;
            let cookie_manager = unsafe { core_webview.CookieManager()? };
            let uri = HSTRING::from(KIMI_LOGIN_URL);
            let handler =
                GetCookiesCompletedHandler::create(Box::new(move |error_code, cookies| {
                    let result = (|| -> windows_core::Result<Option<String>> {
                        error_code?;
                        let Some(cookies) = cookies else {
                            return Ok(None);
                        };
                        let mut count = 0;
                        unsafe { cookies.Count(&mut count)? };
                        for index in 0..count {
                            let cookie = unsafe { cookies.GetValueAtIndex(index)? };
                            let mut name = PWSTR::null();
                            unsafe { cookie.Name(&mut name)? };
                            let name = take_pwstr(name);
                            if !is_auth_cookie_name(&name) {
                                continue;
                            }
                            let mut value = PWSTR::null();
                            unsafe { cookie.Value(&mut value)? };
                            let value = take_pwstr(value);
                            if !value.trim().is_empty() {
                                return Ok(Some(value.trim().to_string()));
                            }
                        }
                        Ok(None)
                    })()
                    .map_err(|error| error.to_string());
                    if let Ok(mut slot) = callback_sender.lock()
                        && let Some(sender) = slot.take()
                    {
                        let _ = sender.send(result);
                    }
                    Ok(())
                }));
            unsafe {
                cookie_manager.GetCookies(PCWSTR::from_raw(uri.as_ptr()), &handler)?;
            }
            Ok(())
        })();

        if let Err(error) = start_result
            && let Ok(mut slot) = scheduled_sender.lock()
            && let Some(sender) = slot.take()
        {
            let _ = sender.send(Err(error.to_string()));
        }
    });

    if let Err(error) = schedule_result
        && let Ok(mut slot) = sender.lock()
        && let Some(sender) = slot.take()
    {
        let _ = sender.send(Err(error.to_string()));
    }
    Ok(receiver)
}

async fn token_for_webview(
    app: AppHandle,
    webview_label: String,
    cancel: &mut tokio::sync::watch::Receiver<bool>,
) -> Result<String, KimiAccountRefreshError> {
    let webview = app
        .get_webview(&webview_label)
        .ok_or_else(missing_webview_error)?;
    #[cfg(windows)]
    let local_storage_read_succeeded = {
        match begin_access_token_script_request(webview.clone()) {
            Ok(receiver) => {
                match wait_with_cancel(receiver, cancel, LOCAL_STORAGE_READ_TIMEOUT).await {
                    Ok(Ok(Ok(Some(token)))) => return Ok(token),
                    Ok(Ok(Ok(None))) => true,
                    Ok(Ok(Err(_))) | Ok(Err(_)) | Err(CancellableWaitError::TimedOut) => false,
                    Err(CancellableWaitError::Cancelled) => {
                        return Err(KimiAccountRefreshError::Cancelled);
                    }
                }
            }
            Err(_) => false,
        }
    };
    #[cfg(windows)]
    let receiver = begin_cookie_request(webview).map_err(|_| KimiAccountRefreshError::Temporary)?;
    #[cfg(not(windows))]
    let receiver = {
        let _ = webview;
        return Err(KimiAccountRefreshError::Temporary);
    };

    let token = match wait_with_cancel(receiver, cancel, std::time::Duration::from_secs(10)).await {
        Ok(Ok(Ok(token))) => token,
        Ok(Ok(Err(_))) | Ok(Err(_)) | Err(CancellableWaitError::TimedOut) => {
            return Err(KimiAccountRefreshError::Temporary);
        }
        Err(CancellableWaitError::Cancelled) => {
            return Err(KimiAccountRefreshError::Cancelled);
        }
    };
    match token {
        Some(token) => Ok(token),
        None if local_storage_read_succeeded => Err(missing_auth_token_error()),
        None => Err(KimiAccountRefreshError::Temporary),
    }
}

fn close_background_webviews_for_session(app: &AppHandle, session_id: &str) -> Vec<String> {
    let prefix = background_webview_prefix(session_id);
    let labels = background_webviews()
        .lock()
        .map(|labels| {
            labels
                .iter()
                .filter(|label| label.starts_with(&prefix))
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for label in &labels {
        if let Some(webview) = app.get_webview(label) {
            let _ = webview.close();
        }
    }
    labels
}

async fn close_and_wait_for_background_webviews(app: &AppHandle, session_id: &str) -> bool {
    let deadline = tokio::time::Instant::now() + BACKGROUND_WEBVIEW_SHUTDOWN_BUDGET;
    loop {
        let labels = close_background_webviews_for_session(app, session_id);
        if labels.iter().all(|label| app.get_webview(label).is_none()) {
            if let Ok(mut registered) = background_webviews().lock() {
                for label in labels {
                    registered.remove(&label);
                }
            }
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

fn close_all_background_webviews(app: &AppHandle) {
    let labels = background_webviews()
        .lock()
        .map(|labels| labels.iter().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    for label in labels {
        if let Some(webview) = app.get_webview(&label) {
            let _ = webview.close();
        }
    }
}

fn remove_session_directory_if_ready(
    path: &Path,
    surfaces_destroyed: bool,
) -> Result<bool, String> {
    if !surfaces_destroyed {
        return Ok(false);
    }
    if path.exists() {
        std::fs::remove_dir_all(path).map_err(|error| error.to_string())?;
    }
    Ok(true)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionCleanupDisposition {
    Deferred,
    NoPendingMarker,
    RetainedByLiveAccount,
    Completed,
}

fn retry_session_cleanup_with(
    repository: &KimiAccountRepository,
    session_id: &str,
    session_path: &Path,
    surfaces_destroyed: bool,
    mut delete_session: impl FnMut(&Path) -> Result<(), String>,
) -> Result<Result<SessionCleanupDisposition, String>, StoreError> {
    if !surfaces_destroyed {
        return Ok(Ok(SessionCleanupDisposition::Deferred));
    }
    repository.try_mutate(|store| {
        if !store
            .pending_session_cleanup
            .iter()
            .any(|id| id == session_id)
        {
            return Ok(SessionCleanupDisposition::NoPendingMarker);
        }
        if store
            .accounts
            .iter()
            .any(|account| account.webview_session_id.as_deref() == Some(session_id))
        {
            // A newly committed/reintroduced account owns this profile. The
            // old deletion request must not survive to a later restart.
            store.pending_session_cleanup.retain(|id| id != session_id);
            return Ok(SessionCleanupDisposition::RetainedByLiveAccount);
        }
        delete_session(session_path)?;
        // If the external delete succeeded but this acknowledgement fails,
        // the marker remains on disk and the deletion is idempotently retried.
        store.pending_session_cleanup.retain(|id| id != session_id);
        Ok(SessionCleanupDisposition::Completed)
    })
}

fn mark_session_cleanup(store: &mut KimiAccountStore, session_id: &str) {
    if !store
        .pending_session_cleanup
        .iter()
        .any(|id| id == session_id)
    {
        store.pending_session_cleanup.push(session_id.to_string());
    }
}

fn browser_source_is_live(store: &KimiAccountStore, source_id: Uuid) -> bool {
    store.accounts.iter().any(|account| {
        account
            .browser_sources
            .iter()
            .any(|source| source.source_id == source_id)
    })
}

fn mark_browser_secret_cleanup(store: &mut KimiAccountStore, source_id: Uuid) {
    if !store.pending_browser_secret_cleanup.contains(&source_id) {
        store.pending_browser_secret_cleanup.push(source_id);
    }
}

/// Records the durable compensation intent before a newly discovered browser
/// source can write a credential outside the encrypted account-store file.
///
/// The marker deliberately differs from a deletion intent: background cleanup
/// must not remove a credential while the scan that owns it is still running.
fn reserve_browser_secret_provisioning(
    store: &mut KimiAccountStore,
    source_ids: &[Uuid],
) -> Result<(), String> {
    let mut requested = HashSet::new();
    for source_id in source_ids {
        if !requested.insert(*source_id) {
            return Err("Kimi 浏览器凭据保护记录包含重复来源".to_string());
        }
        if browser_source_is_live(store, *source_id) {
            return Err("Kimi 浏览器凭据来源已存在，无法建立保护记录".to_string());
        }
        if store.pending_browser_secret_cleanup.contains(source_id) {
            return Err("Kimi 浏览器凭据正在等待清理，无法建立保护记录".to_string());
        }
    }
    for source_id in source_ids {
        if !store
            .pending_browser_secret_provisioning
            .contains(source_id)
        {
            store.pending_browser_secret_provisioning.push(*source_id);
        }
    }
    Ok(())
}

/// Resolves only reservations owned by the current scan. A source which made
/// it into durable account metadata releases its reservation; every other
/// source is converted into the existing, retryable deletion intent.
fn resolve_browser_secret_provisioning(
    store: &mut KimiAccountStore,
    source_ids: &[Uuid],
) -> Vec<Uuid> {
    let mut cleanup_ids = Vec::new();
    let mut requested = HashSet::new();
    for source_id in source_ids {
        if !requested.insert(*source_id)
            || !store
                .pending_browser_secret_provisioning
                .contains(source_id)
        {
            continue;
        }
        let is_live = browser_source_is_live(store, *source_id);
        store
            .pending_browser_secret_provisioning
            .retain(|pending| pending != source_id);
        if !is_live {
            mark_browser_secret_cleanup(store, *source_id);
            cleanup_ids.push(*source_id);
        }
    }
    cleanup_ids
}

fn apply_browser_scan_update(
    latest: &mut KimiAccountStore,
    expected: &KimiAccountStore,
    updated: &KimiAccountStore,
    provisioned_source_ids: &[Uuid],
) -> Result<(KimiAccountStore, Vec<Uuid>), String> {
    if !same_browser_scan_revision(expected, latest) {
        return Err("Kimi 账号数据已变化，请重试".to_string());
    }
    if provisioned_source_ids.iter().any(|source_id| {
        !latest
            .pending_browser_secret_provisioning
            .contains(source_id)
    }) {
        return Err("Kimi 浏览器凭据保护记录缺失，已停止提交账号数据".to_string());
    }
    *latest = merge_browser_scan_update(latest, expected, updated);
    let cleanup_ids = resolve_browser_secret_provisioning(latest, provisioned_source_ids);
    Ok((latest.clone(), cleanup_ids))
}

/// A browser scan starts from a durable baseline, while a regular quota refresh
/// may finish in another process before the scan can commit. Browser-source
/// metadata is protected by `same_account_revision`, but quota intentionally is
/// not: it is safe to merge it per account rather than reject the whole scan.
fn merge_browser_scan_update(
    latest: &KimiAccountStore,
    expected: &KimiAccountStore,
    updated: &KimiAccountStore,
) -> KimiAccountStore {
    let mut merged = updated.clone();
    // Pending cleanup/provisioning records represent independent external
    // credential lifecycles. The caller has already verified that this scan's
    // own reservation still exists; retaining `latest` prevents an older
    // snapshot from resurrecting another scan's resolved marker.
    merged.pending_session_cleanup = latest.pending_session_cleanup.clone();
    merged.pending_browser_secret_cleanup = latest.pending_browser_secret_cleanup.clone();
    merged.pending_browser_secret_provisioning = latest.pending_browser_secret_provisioning.clone();
    for account in &mut merged.accounts {
        let Some(expected_account) = expected
            .accounts
            .iter()
            .find(|candidate| candidate.account_id == account.account_id)
        else {
            continue;
        };
        let Some(latest_account) = latest
            .accounts
            .iter()
            .find(|candidate| candidate.account_id == account.account_id)
        else {
            continue;
        };
        account.last_quota = merge_browser_scan_quota(
            expected_account.last_quota.as_ref(),
            latest_account.last_quota.as_ref(),
            account.last_quota.as_ref(),
        );
    }
    merged
}

/// Choose the last-good quota without allowing a scan's stale baseline to
/// overwrite a durable concurrent update. An explicit scan-side removal (for
/// example a confirmed authentication loss) remains authoritative. If both
/// sides changed a present quota, only a provably newer RFC3339 timestamp wins;
/// malformed legacy timestamps fail closed to the already durable value.
fn merge_browser_scan_quota(
    expected: Option<&KimiStoredQuota>,
    latest: Option<&KimiStoredQuota>,
    updated: Option<&KimiStoredQuota>,
) -> Option<KimiStoredQuota> {
    if latest == expected {
        return updated.cloned();
    }
    if updated == expected {
        return latest.cloned();
    }

    match (latest, updated) {
        (Some(latest), Some(updated)) => {
            let latest_at = chrono::DateTime::parse_from_rfc3339(&latest.updated_at).ok();
            let updated_at = chrono::DateTime::parse_from_rfc3339(&updated.updated_at).ok();
            if matches!((latest_at, updated_at), (Some(latest_at), Some(updated_at)) if updated_at > latest_at)
            {
                Some(updated.clone())
            } else {
                Some(latest.clone())
            }
        }
        (_, updated) => updated.cloned(),
    }
}

fn schedule_session_cleanup(app: &AppHandle, session_id: &str, wait_for_login_window: bool) {
    close_background_webviews_for_session(app, session_id);
    if wait_for_login_window {
        crate::shell::kimi_login_window::close(app);
    }
    let app = app.clone();
    let session_id = session_id.to_string();
    tauri::async_runtime::spawn(async move {
        let lease = background_session_lease(&session_id);
        let _lease_guard = lease.lock().await;
        let retry_delay = std::time::Duration::from_millis(250);
        let mut cleanup_failure_reported = false;
        loop {
            let labels = close_background_webviews_for_session(&app, &session_id);
            let background_destroyed = labels.iter().all(|label| app.get_webview(label).is_none());
            if background_destroyed && let Ok(mut registered) = background_webviews().lock() {
                for label in &labels {
                    registered.remove(label);
                }
            }
            let login_window_destroyed = !wait_for_login_window
                || app
                    .get_webview(crate::shell::kimi_login_window::LABEL)
                    .is_none();
            let destroyed = background_destroyed && login_window_destroyed;
            if !destroyed {
                tokio::time::sleep(retry_delay).await;
                continue;
            }
            let path = match session_path(&session_id) {
                Ok(path) => path,
                Err(_) => return,
            };
            let result = observe_kimi_storage(&app, true, || {
                retry_session_cleanup_with(
                    &kimi_account_repository(&app),
                    &session_id,
                    &path,
                    true,
                    |path| remove_session_directory_if_ready(path, true).map(|_| ()),
                )
            });
            match result {
                Ok(Ok(
                    SessionCleanupDisposition::Completed
                    | SessionCleanupDisposition::NoPendingMarker
                    | SessionCleanupDisposition::RetainedByLiveAccount,
                )) => return,
                Ok(Ok(SessionCleanupDisposition::Deferred)) => {}
                Ok(Err(error)) => {
                    if !cleanup_failure_reported {
                        tracing::warn!(error = %error, "Kimi session directory cleanup was deferred");
                        cleanup_failure_reported = true;
                    }
                }
                Err(error) => {
                    if !cleanup_failure_reported {
                        tracing::warn!(error = %error, "Kimi session cleanup state was not committed");
                        cleanup_failure_reported = true;
                    }
                }
            }
            tokio::time::sleep(retry_delay).await;
        }
    });
}

#[derive(Default)]
struct BrowserSecretCleanupRetryPlan {
    next_attempt: usize,
}

impl BrowserSecretCleanupRetryPlan {
    fn next_delay(&mut self) -> Option<Duration> {
        let delay = BROWSER_SECRET_CLEANUP_RETRY_DELAYS
            .get(self.next_attempt)
            .copied()?;
        self.next_attempt += 1;
        Some(delay)
    }
}

fn browser_secret_cleanup_retry_tasks() -> &'static Mutex<HashSet<Uuid>> {
    BROWSER_SECRET_CLEANUP_RETRY_TASKS.get_or_init(|| Mutex::new(HashSet::new()))
}

fn retry_browser_secret_cleanup_once(app: &AppHandle, source_ids: &[Uuid]) -> bool {
    if source_ids.is_empty() {
        return true;
    }
    match observe_kimi_storage(app, true, || {
        retry_browser_secret_cleanup_with(&kimi_account_repository(app), &source_ids, |source_id| {
            browser::delete_browser_source_secrets(&[source_id])
        })
    }) {
        Ok(Ok(())) => true,
        Ok(Err(error)) => {
            tracing::warn!(error = %error, "Kimi browser credential cleanup was deferred");
            false
        }
        Err(error) => {
            tracing::warn!(error = %error, "Kimi browser credential cleanup state was not committed");
            false
        }
    }
}

fn schedule_browser_secret_cleanup_retries(app: &AppHandle, source_ids: &[Uuid]) {
    let scheduled_source_ids = source_ids
        .iter()
        .copied()
        .filter(|source_id| {
            browser_secret_cleanup_retry_tasks()
                .lock()
                .map(|mut tasks| tasks.insert(*source_id))
                .unwrap_or(false)
        })
        .collect::<Vec<_>>();

    for source_id in scheduled_source_ids {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            let mut plan = BrowserSecretCleanupRetryPlan::default();
            while let Some(delay) = plan.next_delay() {
                tokio::time::sleep(delay).await;
                if retry_browser_secret_cleanup_once(&app, &[source_id]) {
                    if let Ok(mut tasks) = browser_secret_cleanup_retry_tasks().lock() {
                        tasks.remove(&source_id);
                    }
                    return;
                }
            }
            if let Ok(mut tasks) = browser_secret_cleanup_retry_tasks().lock() {
                tasks.remove(&source_id);
            }
            tracing::warn!(
                source_id = %source_id,
                "Kimi browser credential cleanup remained pending after bounded retries"
            );
        });
    }
}

fn retry_browser_secret_cleanup(app: &AppHandle, source_ids: Vec<Uuid>) {
    if !retry_browser_secret_cleanup_once(app, &source_ids) {
        schedule_browser_secret_cleanup_retries(app, &source_ids);
    }
}

fn retry_browser_secret_cleanup_with(
    repository: &KimiAccountRepository,
    source_ids: &[Uuid],
    mut delete_secret: impl FnMut(Uuid) -> Result<(), String>,
) -> Result<Result<(), String>, StoreError> {
    repository.try_mutate(|store| {
        for source_id in source_ids {
            // Re-read under the repository locks: only a durable removal intent
            // permits deletion, and a live source always takes precedence.
            if !store.pending_browser_secret_cleanup.contains(source_id) {
                continue;
            }
            let is_live = store.accounts.iter().any(|account| {
                account
                    .browser_sources
                    .iter()
                    .any(|source| source.source_id == *source_id)
            });
            if !is_live {
                let source_lock = browser::browser_source_lock(*source_id);
                let _source_guard = source_lock.try_lock().map_err(|_| {
                    "Kimi browser credential is in use; cleanup will retry".to_string()
                })?;
                delete_secret(*source_id)?;
            }
            store
                .pending_browser_secret_cleanup
                .retain(|pending| pending != source_id);
        }
        // If deletion or acknowledgement fails, the committed intent remains
        // on disk. Deletion is idempotent, so the next retry remains safe.
        Ok(())
    })
}

#[tauri::command]
pub fn list_kimi_accounts(app: AppHandle) -> Result<Vec<KimiAccountSnapshot>, String> {
    list_kimi_account_snapshots(&app)
}

fn list_kimi_account_snapshots(app: &AppHandle) -> Result<Vec<KimiAccountSnapshot>, String> {
    let snapshots: Vec<_> = load_kimi_account_store(app)?
        .accounts
        .iter()
        .map(snapshot_for_record)
        .collect();
    let guard = cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if guard.storage_failed && snapshots.is_empty() {
        // An empty vector cannot convey the failed health state to a newly
        // mounted surface that may have missed the earlier failure event.
        return Err("Kimi account storage has not recovered".to_string());
    }
    Ok(guard.present(snapshots))
}

#[tauri::command]
pub async fn begin_kimi_account_login(
    app: AppHandle,
    expected_account_id: Option<Uuid>,
) -> Result<KimiLoginSession, String> {
    if !kimi_monthly_quota_enabled(&app) {
        return Err("请先开启 Kimi 月额度显示".to_string());
    }
    let store = load_kimi_account_store(&app)?;
    if let Some(account_id) = expected_account_id
        && !store
            .accounts
            .iter()
            .any(|account| account.account_id == account_id)
    {
        return Err("Kimi account was not found".to_string());
    }
    let mut active_guard = active_login()
        .lock()
        .map_err(|_| "Kimi 登录状态不可用".to_string())?;
    if let Some(existing) = active_guard.clone() {
        crate::shell::kimi_login_window::focus(&app)?;
        return Ok(KimiLoginSession {
            session_id: existing.session_id,
        });
    }
    if app
        .get_webview_window(crate::shell::kimi_login_window::LABEL)
        .is_some()
        || visible_login_session()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_some()
    {
        return Err("Kimi 登录窗口正在关闭，请稍后重试".to_string());
    }
    let session_id = Uuid::new_v4().to_string();
    let generation = LOGIN_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    crate::shell::kimi_login_window::open_or_focus(&app, session_path(&session_id)?)?;
    *visible_login_session()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(session_id.clone());
    *active_guard = Some(ActiveLogin {
        session_id: session_id.clone(),
        expected_account_id,
        generation,
    });
    Ok(KimiLoginSession { session_id })
}

#[tauri::command]
pub async fn complete_kimi_account_login(
    app: AppHandle,
    session_id: String,
    expected_account_id: Option<Uuid>,
) -> Result<KimiLoginCompletion, String> {
    let active = active_login()
        .lock()
        .map_err(|_| "Kimi 登录状态不可用".to_string())?
        .clone()
        .filter(|active| active.session_id == session_id)
        .ok_or_else(|| "Kimi 登录会话不存在或已经结束".to_string())?;
    if active.expected_account_id != expected_account_id {
        return Err("Kimi 登录目标已经变化，请重新开始登录".to_string());
    }
    let mut outcome = start_or_observe_login_task(app, active)?;
    match wait_for_login_outcome(&mut outcome, LOGIN_FOREGROUND_BUDGET).await {
        Some(LoginTaskOutcome::Completed(snapshot)) => Ok(KimiLoginCompletion {
            status: "completed",
            snapshot: Some(snapshot),
        }),
        Some(LoginTaskOutcome::Failed(error)) => Err(error),
        Some(LoginTaskOutcome::Cancelled) => Err("Kimi 账号读取已取消".to_string()),
        None => Ok(KimiLoginCompletion {
            status: "pending",
            snapshot: None,
        }),
    }
}

fn start_or_observe_login_task(
    app: AppHandle,
    active: ActiveLogin,
) -> Result<tokio::sync::watch::Receiver<Option<LoginTaskOutcome>>, String> {
    let state_guard = login_state_lock()
        .lock()
        .map_err(|_| "Kimi 登录任务状态不可用".to_string())?;
    let active_is_current = active_login()
        .lock()
        .ok()
        .and_then(|current| current.clone())
        .is_some_and(|current| {
            current.session_id == active.session_id && current.generation == active.generation
        });
    if !active_is_current {
        return Err("Kimi 登录会话不存在或已经结束".to_string());
    }
    let mut tasks = login_tasks()
        .lock()
        .map_err(|_| "Kimi 登录任务状态不可用".to_string())?;
    if let Some(existing) = tasks.get(&active.session_id) {
        if existing.generation == active.generation {
            return Ok(existing.outcome.clone());
        }
        if claim_login_cancellation(&existing.phase) {
            let _ = existing.cancel.send(true);
        }
        existing.abort.abort();
        tasks.remove(&active.session_id);
    }

    let (cancel, cancel_rx) = tokio::sync::watch::channel(false);
    let (outcome_tx, outcome_rx) = tokio::sync::watch::channel(None);
    let (start_tx, start_rx) = tokio::sync::oneshot::channel();
    let phase = Arc::new(AtomicU8::new(LoginTaskPhase::Running as u8));
    let close_after_finish = Arc::new(AtomicBool::new(false));
    let worker_phase = Arc::clone(&phase);
    let worker_app = app.clone();
    let worker_active = active.clone();
    let worker = tokio::spawn(async move {
        if start_rx.await.is_err() {
            return LoginTaskOutcome::Cancelled;
        }
        run_login_worker(worker_app, worker_active, cancel_rx, worker_phase).await
    });
    let abort = worker.abort_handle();
    tasks.insert(
        active.session_id.clone(),
        LoginTaskEntry {
            generation: active.generation,
            cancel,
            outcome: outcome_rx.clone(),
            phase: Arc::clone(&phase),
            close_after_finish: Arc::clone(&close_after_finish),
            abort,
        },
    );
    drop(tasks);
    drop(state_guard);
    let _ = start_tx.send(());

    let session_id = active.session_id.clone();
    let generation = active.generation;
    tauri::async_runtime::spawn(async move {
        let outcome = match worker.await {
            Ok(outcome) => outcome,
            Err(error) if error.is_cancelled() => LoginTaskOutcome::Cancelled,
            Err(_) => LoginTaskOutcome::Failed("Kimi 账号读取任务异常结束，请重试".to_string()),
        };
        let mut outcome = if finish_login_phase(&phase) {
            outcome
        } else {
            LoginTaskOutcome::Cancelled
        };
        if close_after_finish.load(Ordering::SeqCst)
            && matches!(outcome, LoginTaskOutcome::Failed(_))
        {
            outcome = LoginTaskOutcome::Cancelled;
        }
        let _ = outcome_tx.send(Some(outcome.clone()));
        let current = login_tasks()
            .lock()
            .ok()
            .and_then(|tasks| tasks.get(&session_id).cloned())
            .is_some_and(|task| task.generation == generation);
        if current {
            let event = match &outcome {
                LoginTaskOutcome::Completed(snapshot) => KimiLoginCompletionEvent {
                    session_id: session_id.clone(),
                    status: "completed",
                    snapshot: Some(snapshot.clone()),
                    error: None,
                },
                LoginTaskOutcome::Failed(error) => KimiLoginCompletionEvent {
                    session_id: session_id.clone(),
                    status: "failed",
                    snapshot: None,
                    error: Some(error.clone()),
                },
                LoginTaskOutcome::Cancelled => KimiLoginCompletionEvent {
                    session_id: session_id.clone(),
                    status: "cancelled",
                    snapshot: None,
                    error: None,
                },
            };
            emit_login_completion(&app, event);
        }
        let closed_or_requested = close_after_finish.load(Ordering::SeqCst)
            || app
                .get_webview(crate::shell::kimi_login_window::LABEL)
                .is_none();
        if !matches!(outcome, LoginTaskOutcome::Completed(_))
            && closed_or_requested
            && take_active_login_generation(&session_id, generation).is_some()
        {
            let cleanup_committed = mutate_kimi_account_store(&app, |store| {
                if store.accounts.iter().any(|account| {
                    account.webview_session_id.as_deref() == Some(session_id.as_str())
                }) {
                    return false;
                }
                mark_session_cleanup(store, &session_id);
                true
            })
            .unwrap_or(false);
            if cleanup_committed {
                schedule_session_cleanup(
                    &app,
                    &session_id,
                    close_after_finish.load(Ordering::SeqCst),
                );
            }
        }
        if let Ok(mut tasks) = login_tasks().lock()
            && tasks
                .get(&session_id)
                .is_some_and(|task| task.generation == generation)
        {
            tasks.remove(&session_id);
        }
    });

    Ok(outcome_rx)
}

async fn run_login_worker(
    app: AppHandle,
    active: ActiveLogin,
    mut cancel: tokio::sync::watch::Receiver<bool>,
    phase: Arc<AtomicU8>,
) -> LoginTaskOutcome {
    let token = match token_for_webview(
        app.clone(),
        crate::shell::kimi_login_window::LABEL.to_string(),
        &mut cancel,
    )
    .await
    {
        Ok(token) => token,
        Err(KimiAccountRefreshError::Cancelled) => return LoginTaskOutcome::Cancelled,
        Err(error) => return LoginTaskOutcome::Failed(error.user_message().to_string()),
    };
    let account = match wait_with_cancel(
        fetch_web_account(&token),
        &mut cancel,
        std::time::Duration::from_secs(30),
    )
    .await
    {
        Ok(Ok(account)) => account,
        Ok(Err(error)) => {
            return LoginTaskOutcome::Failed(
                classify_provider_error(&error).user_message().to_string(),
            );
        }
        Err(CancellableWaitError::Cancelled) => return LoginTaskOutcome::Cancelled,
        Err(CancellableWaitError::TimedOut) => {
            return LoginTaskOutcome::Failed(
                KimiAccountRefreshError::Temporary
                    .user_message()
                    .to_string(),
            );
        }
    };
    if !is_login_session_current(&active.session_id, active.generation) {
        return LoginTaskOutcome::Cancelled;
    }
    if !begin_login_commit(&phase) {
        return LoginTaskOutcome::Cancelled;
    }
    match persist_authenticated_login(&app, &active, account.0, account.1) {
        Ok(snapshot) => LoginTaskOutcome::Completed(snapshot),
        Err(_) if !kimi_monthly_quota_enabled(&app) => {
            let _ = take_active_login_generation(&active.session_id, active.generation);
            crate::shell::kimi_login_window::close(&app);
            LoginTaskOutcome::Cancelled
        }
        Err(_error) if !is_login_session_current(&active.session_id, active.generation) => {
            LoginTaskOutcome::Cancelled
        }
        Err(error) => LoginTaskOutcome::Failed(error),
    }
}

fn persist_authenticated_login(
    app: &AppHandle,
    active: &ActiveLogin,
    identity: KimiAccountIdentity,
    quota: KimiMonthlyQuota,
) -> Result<KimiAccountSnapshot, String> {
    let commit_guard = lock_account_commit()?;
    if !is_login_session_current(&active.session_id, active.generation)
        || !kimi_monthly_quota_enabled(app)
    {
        return Err("Kimi 账号读取已取消".to_string());
    }
    let updated_at = Utc::now().to_rfc3339();
    let resets_at = quota.resets_at.map(|value| value.to_rfc3339());
    let identity_fingerprint = identity.identity;
    let display_name = identity.display_name;
    let (store, snapshot, replaced_session_id) = try_mutate_kimi_account_store(app, |store| {
        if !is_login_session_current(&active.session_id, active.generation)
            || !kimi_monthly_quota_enabled(app)
        {
            return Err("Kimi 账号读取已取消".to_string());
        }
        if let Some(account_id) = active.expected_account_id {
            let expected = store
                .accounts
                .iter()
                .find(|account| account.account_id == account_id)
                .ok_or_else(|| "Kimi account was not found".to_string())?;
            if !expected.identity.strictly_matches(&identity_fingerprint) {
                return Err("重新登录的 Kimi 账号与原账号不一致".to_string());
            }
        }
        let upsert = store.upsert_authenticated(
            active.session_id.clone(),
            identity_fingerprint,
            display_name,
        );
        let record = store
            .accounts
            .iter_mut()
            .find(|record| record.account_id == upsert.account_id)
            .ok_or_else(|| "Kimi account was not saved".to_string())?;
        record.last_quota = Some(KimiStoredQuota {
            used_percent: quota.used_percent,
            resets_at: resets_at.clone(),
            updated_at: updated_at.clone(),
        });
        let snapshot = KimiAccountSnapshot {
            account_id: record.account_id,
            display_name: record.display_name.clone(),
            used_percent: Some(quota.used_percent),
            resets_at: resets_at.clone(),
            updated_at: Some(updated_at.clone()),
            status: "ok",
            matched_credential_ids: Vec::new(),
            source_labels: browser_source_labels(record),
        };
        if let Some(replaced) = upsert.replaced_session_id.as_deref() {
            mark_session_cleanup(store, replaced);
        }
        if !is_login_session_current(&active.session_id, active.generation)
            || !kimi_monthly_quota_enabled(app)
        {
            return Err("Kimi 账号读取已取消".to_string());
        }
        Ok((store.clone(), snapshot, upsert.replaced_session_id))
    })?;
    invalidate_account_refresh();
    if let Some(replaced) = replaced_session_id.as_deref() {
        schedule_session_cleanup(app, replaced, false);
    }
    let mut snapshots = store
        .accounts
        .iter()
        .map(snapshot_for_record)
        .collect::<Vec<_>>();
    if let Some(existing) = snapshots
        .iter_mut()
        .find(|item| item.account_id == snapshot.account_id)
    {
        *existing = snapshot.clone();
    } else {
        snapshots.push(snapshot.clone());
    }
    apply_cached_credential_matches(&store, &mut snapshots);
    let matching_store = store.clone();
    if !is_login_session_current(&active.session_id, active.generation)
        || !kimi_monthly_quota_enabled(app)
    {
        return Err("Kimi 账号读取已取消".to_string());
    }
    publish_cache(app, snapshots);
    let snapshot = cache()
        .lock()
        .ok()
        .and_then(|items| {
            items
                .presented()
                .iter()
                .find(|item| item.account_id == snapshot.account_id)
                .cloned()
        })
        .unwrap_or(snapshot);
    drop(commit_guard);
    spawn_credential_match_refresh(app.clone(), matching_store);
    if take_active_login_generation(&active.session_id, active.generation).is_some() {
        crate::shell::kimi_login_window::close(app);
    }
    Ok(snapshot)
}

#[tauri::command]
pub async fn cancel_kimi_account_login(app: AppHandle, session_id: String) -> Result<bool, String> {
    cancel_login_session(app, session_id, true, false).await
}

fn request_login_cancellation(
    session_id: &str,
    close_after_finish: bool,
) -> Result<RequestedLoginCancellation, String> {
    let _state_guard = login_state_lock()
        .lock()
        .map_err(|_| "Kimi 登录提交状态不可用".to_string())?;
    let task = login_tasks()
        .lock()
        .ok()
        .and_then(|tasks| tasks.get(session_id).cloned());
    let has_matching_active = active_login()
        .lock()
        .ok()
        .and_then(|active| active.clone())
        .is_some_and(|active| active.session_id == session_id);
    let accepted = task.as_ref().map_or(has_matching_active, |task| {
        claim_login_cancellation(&task.phase)
    });
    if !accepted
        && close_after_finish
        && let Some(task) = &task
    {
        task.close_after_finish.store(true, Ordering::SeqCst);
    }
    let active = accepted.then(|| take_active_login(session_id)).flatten();
    if accepted && let Some(task) = &task {
        let _ = task.cancel.send(true);
    }
    Ok(RequestedLoginCancellation {
        active,
        task,
        accepted,
    })
}

async fn settle_requested_cancellation(
    requested: &RequestedLoginCancellation,
) -> Option<LoginTaskOutcome> {
    let task = requested.task.as_ref()?;
    let mut outcome = task.outcome.clone();
    let settled = wait_for_login_outcome(
        &mut outcome,
        if requested.accepted {
            LOGIN_CANCEL_COOPERATIVE_BUDGET
        } else {
            LOGIN_FOREGROUND_BUDGET
        },
    )
    .await;
    if settled.is_some() || !requested.accepted {
        return settled;
    }
    task.abort.abort();
    wait_for_login_outcome(&mut outcome, LOGIN_CANCEL_ABORT_BUDGET).await
}

async fn finish_requested_login_cancellation(
    app: AppHandle,
    session_id: String,
    close_window: bool,
    notify_settings: bool,
    requested: RequestedLoginCancellation,
) -> Result<bool, String> {
    let settled = settle_requested_cancellation(&requested).await;
    if !requested.accepted {
        if close_window && notify_settings {
            crate::shell::kimi_login_window::close(&app);
        }
        if notify_settings
            && matches!(
                settled,
                Some(LoginTaskOutcome::Failed(_) | LoginTaskOutcome::Cancelled)
            )
            && take_active_login_generation(
                &session_id,
                requested.task.as_ref().map_or(0, |task| task.generation),
            )
            .is_some()
        {
            let cleanup_committed = mutate_kimi_account_store(&app, |store| {
                if store.accounts.iter().any(|account| {
                    account.webview_session_id.as_deref() == Some(session_id.as_str())
                }) {
                    return false;
                }
                mark_session_cleanup(store, &session_id);
                true
            })?;
            if cleanup_committed {
                schedule_session_cleanup(&app, &session_id, close_window);
            }
            emit_login_completion(
                &app,
                KimiLoginCompletionEvent {
                    session_id,
                    status: "cancelled",
                    snapshot: None,
                    error: None,
                },
            );
        }
        return Ok(false);
    }
    let wait_for_login_window = requested.active.is_some()
        && (close_window
            || app
                .get_webview(crate::shell::kimi_login_window::LABEL)
                .is_some());
    let cleanup_committed = mutate_kimi_account_store(&app, |store| {
        if store
            .accounts
            .iter()
            .any(|account| account.webview_session_id.as_deref() == Some(session_id.as_str()))
        {
            return false;
        }
        mark_session_cleanup(store, &session_id);
        true
    })?;
    if cleanup_committed {
        schedule_session_cleanup(&app, &session_id, wait_for_login_window);
    }
    if notify_settings && requested.task.is_none() {
        emit_login_completion(
            &app,
            KimiLoginCompletionEvent {
                session_id,
                status: "cancelled",
                snapshot: None,
                error: None,
            },
        );
    }
    Ok(true)
}

async fn cancel_login_session(
    app: AppHandle,
    session_id: String,
    close_window: bool,
    notify_settings: bool,
) -> Result<bool, String> {
    let requested = request_login_cancellation(&session_id, notify_settings)?;
    finish_requested_login_cancellation(app, session_id, close_window, notify_settings, requested)
        .await
}

pub fn cancel_active_login_in_background(
    app: &AppHandle,
    close_window: bool,
    notify_settings: bool,
) {
    let session_id = active_login()
        .lock()
        .ok()
        .and_then(|active| active.as_ref().map(|active| active.session_id.clone()));
    let Some(session_id) = session_id else {
        return;
    };
    let Ok(requested) = request_login_cancellation(&session_id, notify_settings) else {
        return;
    };
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let _ = finish_requested_login_cancellation(
            app,
            session_id,
            close_window,
            notify_settings,
            requested,
        )
        .await;
    });
}

fn retry_pending_session_cleanup(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let Ok(store) = load_kimi_account_store(&app) else {
            return;
        };
        for session_id in store.pending_session_cleanup {
            schedule_session_cleanup(&app, &session_id, false);
        }
    });
}

pub fn handle_window_event(window: &tauri::Window, event: &tauri::WindowEvent) -> bool {
    if window.label() != crate::shell::kimi_login_window::LABEL {
        return false;
    }
    if matches!(event, tauri::WindowEvent::CloseRequested { .. }) {
        cancel_active_login_in_background(window.app_handle(), false, true);
    } else if matches!(event, tauri::WindowEvent::Destroyed) {
        if let Ok(mut session) = visible_login_session().lock() {
            *session = None;
        }
        retry_pending_session_cleanup(window.app_handle());
    }
    true
}

#[tauri::command]
pub async fn remove_kimi_account(app: AppHandle, account_id: Uuid) -> Result<(), String> {
    invalidate_account_refresh();
    let active_relogin = active_login()
        .lock()
        .ok()
        .and_then(|active| active.clone())
        .filter(|active| active.expected_account_id == Some(account_id));
    if let Some(active) = active_relogin {
        let _ = cancel_login_session(app.clone(), active.session_id, true, true).await?;
    }
    invalidate_account_refresh();
    let (committed, session_id, source_ids) = try_mutate_kimi_account_store(&app, |store| {
        let index = store
            .accounts
            .iter()
            .position(|account| account.account_id == account_id)
            .ok_or_else(|| "Kimi account was not found".to_string())?;
        let removed = store.accounts[index].clone();
        browser::suppress_browser_sources(store, &removed);
        let source_ids = removed
            .browser_sources
            .iter()
            .map(|source| source.source_id)
            .collect::<Vec<_>>();
        for source_id in &source_ids {
            if !store.pending_browser_secret_cleanup.contains(source_id) {
                store.pending_browser_secret_cleanup.push(*source_id);
            }
        }
        store.accounts.remove(index);
        if let Some(session_id) = removed.webview_session_id.as_deref() {
            mark_session_cleanup(store, session_id);
        }
        Ok((store.clone(), removed.webview_session_id, source_ids))
    })?;
    if let Some(session_id) = session_id.as_deref() {
        schedule_session_cleanup(&app, session_id, false);
    }
    retry_browser_secret_cleanup(&app, source_ids);
    publish_cache(
        &app,
        committed.accounts.iter().map(snapshot_for_record).collect(),
    );
    Ok(())
}

async fn fetch_account_snapshot(
    app: AppHandle,
    record: &KimiAccountRecord,
    generation: u64,
) -> Result<KimiAccountSnapshot, KimiAccountRefreshError> {
    let session_id = record
        .webview_session_id
        .as_deref()
        .ok_or(KimiAccountRefreshError::MissingCredential)?;
    let lease = background_session_lease(session_id);
    let lease_guard = tokio::select! {
        guard = tokio::time::timeout(BACKGROUND_WEBVIEW_SHUTDOWN_BUDGET, lease.lock()) => {
            guard.map_err(|_| KimiAccountRefreshError::Temporary)?
        },
        _ = wait_for_account_refresh_invalidation(generation) => {
            return Err(KimiAccountRefreshError::Cancelled);
        }
    };
    let previous_destroyed = tokio::select! {
        destroyed = close_and_wait_for_background_webviews(&app, session_id) => destroyed,
        _ = wait_for_account_refresh_invalidation(generation) => {
            return Err(KimiAccountRefreshError::Cancelled);
        }
    };
    if !previous_destroyed {
        return Err(KimiAccountRefreshError::Temporary);
    }
    if visible_login_owns_session(session_id)
        && app
            .get_webview(crate::shell::kimi_login_window::LABEL)
            .is_some()
    {
        let visible_destroyed = tokio::select! {
            destroyed = wait_for_webview_destroyed(
                &app,
                crate::shell::kimi_login_window::LABEL,
            ) => destroyed,
            _ = wait_for_account_refresh_invalidation(generation) => {
                return Err(KimiAccountRefreshError::Cancelled);
            }
        };
        if !visible_destroyed {
            return Err(KimiAccountRefreshError::Temporary);
        }
    }
    if !is_current_account_refresh(generation) || !kimi_monthly_quota_enabled(&app) {
        return Err(KimiAccountRefreshError::Cancelled);
    }
    let webview = create_background_account_webview(&app, session_id)
        .map_err(|_| KimiAccountRefreshError::Temporary)?;
    if !is_current_account_refresh(generation) || !kimi_monthly_quota_enabled(&app) {
        let label = webview.label.clone();
        drop(webview);
        let _ = wait_for_webview_destroyed(&app, &label).await;
        return Err(KimiAccountRefreshError::Cancelled);
    }
    let (_cancel_tx, mut cancel_rx) = tokio::sync::watch::channel(false);
    let token_result = tokio::select! {
        result = token_for_webview(app.clone(), webview.label.clone(), &mut cancel_rx) => result,
        _ = wait_for_account_refresh_invalidation(generation) => {
            Err(KimiAccountRefreshError::Cancelled)
        }
    };
    let label = webview.label.clone();
    drop(webview);
    if !wait_for_webview_destroyed(&app, &label).await {
        return Err(KimiAccountRefreshError::Temporary);
    }
    drop(lease_guard);
    drop(lease);
    let token = token_result?;
    let permit = tokio::select! {
        permit = crate::commands::provider_fetch_permits().acquire_owned() => {
            permit.map_err(|_| KimiAccountRefreshError::Temporary)?
        }
        _ = wait_for_account_refresh_invalidation(generation) => {
            return Err(KimiAccountRefreshError::Cancelled);
        }
    };
    let (identity, quota) = tokio::select! {
        result = fetch_web_account(&token) => {
            result.map_err(|error| classify_provider_error(&error))?
        }
        _ = wait_for_account_refresh_invalidation(generation) => {
            return Err(KimiAccountRefreshError::Cancelled);
        }
    };
    drop(permit);
    if !record.identity.strictly_matches(&identity.identity) {
        return Err(KimiAccountRefreshError::Authentication);
    }
    Ok(KimiAccountSnapshot {
        account_id: record.account_id,
        display_name: record.display_name.clone(),
        used_percent: Some(quota.used_percent),
        resets_at: quota.resets_at.map(|value| value.to_rfc3339()),
        updated_at: Some(Utc::now().to_rfc3339()),
        status: "ok",
        matched_credential_ids: Vec::new(),
        source_labels: browser_source_labels(record),
    })
}

fn same_account_revision(left: &KimiAccountStore, right: &KimiAccountStore) -> bool {
    same_browser_scan_revision(left, right)
        && left.pending_session_cleanup == right.pending_session_cleanup
        && left.pending_browser_secret_cleanup == right.pending_browser_secret_cleanup
        && left.pending_browser_secret_provisioning == right.pending_browser_secret_provisioning
}

/// Browser scan commits conflict on account structure and discovery state, not
/// on independent credential-cleanup lifecycles. `apply_browser_scan_update`
/// separately verifies the current scan's own reservation and preserves the
/// newest lifecycle markers during its field-level merge.
fn same_browser_scan_revision(left: &KimiAccountStore, right: &KimiAccountStore) -> bool {
    left.suppressed_browser_profiles == right.suppressed_browser_profiles
        && left.accounts.len() == right.accounts.len()
        && left.accounts.iter().all(|account| {
            right.accounts.iter().any(|candidate| {
                candidate.account_id == account.account_id
                    && candidate.webview_session_id == account.webview_session_id
                    && candidate.browser_sources == account.browser_sources
                    && candidate.identity == account.identity
                    && candidate.display_name == account.display_name
            })
        })
}

fn apply_snapshot_to_record(
    record: &mut KimiAccountRecord,
    snapshot: &KimiAccountSnapshot,
) -> bool {
    if snapshot.status == "loginRequired" {
        return record.last_quota.take().is_some();
    }
    if snapshot.status != "ok" {
        return false;
    }
    let (Some(used_percent), Some(updated_at)) =
        (snapshot.used_percent, snapshot.updated_at.clone())
    else {
        return false;
    };
    record.last_quota = Some(KimiStoredQuota {
        used_percent,
        resets_at: snapshot.resets_at.clone(),
        updated_at,
    });
    true
}

pub fn disable_kimi_accounts(app: &AppHandle) {
    invalidate_account_refresh();
    cancel_active_login_in_background(app, true, true);
    close_all_background_webviews(app);
    publish_cache(app, Vec::new());
}

fn validate_browser_scan_settings(
    monthly_quota_enabled: bool,
    browser_consent_accepted: bool,
) -> Result<(), String> {
    if !monthly_quota_enabled {
        return Err("请先开启 Kimi 月额度".into());
    }
    if !browser_consent_accepted {
        return Err("请先允许读取浏览器 Kimi 登录状态".into());
    }
    Ok(())
}

fn snapshots_after_browser_refresh(
    store: &KimiAccountStore,
    summary: &browser::KimiBrowserScanSummary,
) -> Vec<KimiAccountSnapshot> {
    let mut snapshots = store
        .accounts
        .iter()
        .map(|record| snapshot_after_browser_refresh(record, summary))
        .collect::<Vec<_>>();
    apply_cached_credential_matches(store, &mut snapshots);
    snapshots
}

fn snapshot_after_browser_refresh(
    record: &KimiAccountRecord,
    summary: &browser::KimiBrowserScanSummary,
) -> KimiAccountSnapshot {
    if summary.refreshed_account_ids.contains(&record.account_id) {
        snapshot_from_stored_quota_with_status(record, "ok")
            .unwrap_or_else(|| empty_snapshot(record, "refreshFailed"))
    } else if summary
        .authentication_account_ids
        .contains(&record.account_id)
    {
        empty_snapshot(record, "loginRequired")
    } else if summary
        .temporary_failure_account_ids
        .contains(&record.account_id)
    {
        snapshot_after_refresh_error(
            record,
            Some(snapshot_for_record(record)),
            &KimiAccountRefreshError::Temporary,
        )
    } else {
        snapshot_for_record(record)
    }
}

fn snapshot_without_webview_refresh(
    record: &KimiAccountRecord,
    summary: &browser::KimiBrowserScanSummary,
) -> Option<KimiAccountSnapshot> {
    if summary.refreshed_account_ids.contains(&record.account_id) {
        return Some(
            snapshot_from_stored_quota_with_status(record, "ok")
                .unwrap_or_else(|| empty_snapshot(record, "refreshFailed")),
        );
    }
    if record.webview_session_id.is_some() {
        return None;
    }
    let error = if summary
        .authentication_account_ids
        .contains(&record.account_id)
    {
        KimiAccountRefreshError::Authentication
    } else {
        KimiAccountRefreshError::Temporary
    };
    Some(snapshot_after_refresh_error(
        record,
        Some(snapshot_for_record(record)),
        &error,
    ))
}

fn recover_browser_secret_provisioning(app: &AppHandle, source_ids: &[Uuid]) {
    if source_ids.is_empty() {
        return;
    }
    let cleanup_ids = match try_mutate_kimi_account_store(app, |store| {
        Ok::<_, String>(resolve_browser_secret_provisioning(store, source_ids))
    }) {
        Ok(cleanup_ids) => cleanup_ids,
        Err(error) => {
            // The durable provisioning marker remains untouched if this write
            // cannot be verified. Startup will retry before any deletion.
            tracing::warn!(error = %error, "Kimi browser credential recovery was deferred");
            return;
        }
    };
    retry_browser_secret_cleanup(app, cleanup_ids);
}

async fn refresh_browser_account_scan_transaction(
    app: &AppHandle,
    baseline: &KimiAccountStore,
    generation: u64,
    force_discovery: bool,
) -> Result<(KimiAccountStore, browser::KimiBrowserScanSummary), String> {
    if !is_current_account_refresh(generation) {
        return Err("Kimi 浏览器账号读取已取消".to_string());
    }
    let scan = browser::BrowserAccountScan::discover(baseline, force_discovery);
    let provisioned_source_ids = scan.provisional_source_ids();
    let reserved = if provisioned_source_ids.is_empty() {
        baseline.clone()
    } else {
        try_mutate_kimi_account_store(app, |latest| {
            if !same_browser_scan_revision(baseline, latest) {
                return Err("Kimi 账号数据已变化，请重试".to_string());
            }
            reserve_browser_secret_provisioning(latest, &provisioned_source_ids)?;
            Ok(latest.clone())
        })?
    };

    let mut updated = reserved.clone();
    let summary = scan.refresh(&mut updated, generation).await;
    if !is_current_account_refresh(generation) {
        recover_browser_secret_provisioning(app, &provisioned_source_ids);
        return Err("Kimi 浏览器账号读取已取消".to_string());
    }
    if provisioned_source_ids.is_empty() && updated == reserved {
        return Ok((reserved, summary));
    }

    match try_mutate_kimi_account_store(app, |latest| {
        apply_browser_scan_update(latest, &reserved, &updated, &provisioned_source_ids)
    }) {
        Ok((committed, cleanup_ids)) => {
            retry_browser_secret_cleanup(app, cleanup_ids);
            Ok((committed, summary))
        }
        Err(error) => {
            recover_browser_secret_provisioning(app, &provisioned_source_ids);
            Err(error)
        }
    }
}

#[tauri::command]
pub async fn refresh_kimi_browser_accounts(
    app: AppHandle,
) -> Result<KimiBrowserScanResult, String> {
    let settings = kimi_settings(&app);
    validate_browser_scan_settings(
        settings.kimi_monthly_quota_enabled,
        settings.kimi_browser_consent_accepted,
    )?;
    let generation = begin_account_refresh();
    let baseline = load_kimi_account_store(&app)?;
    let (committed, summary) =
        refresh_browser_account_scan_transaction(&app, &baseline, generation, true).await?;
    let accounts = snapshots_after_browser_refresh(&committed, &summary);
    let accounts = publish_cache(&app, accounts);
    spawn_credential_match_refresh(app, committed);
    Ok(KimiBrowserScanResult {
        accounts,
        discovered: summary.discovered,
        refreshed: summary.refreshed,
        failed: summary.failed,
    })
}

pub async fn refresh_kimi_accounts(app: &AppHandle) {
    let generation = begin_account_refresh();
    let settings = kimi_settings(app);
    if !settings.kimi_monthly_quota_enabled {
        close_all_background_webviews(app);
        if is_current_account_refresh(generation) {
            publish_cache(app, Vec::new());
        }
        return;
    }
    let baseline = match load_kimi_account_store(app) {
        Ok(store) => store,
        Err(_) => return,
    };
    let (store, browser_summary) = if settings.kimi_browser_consent_accepted {
        match refresh_browser_account_scan_transaction(app, &baseline, generation, false).await {
            Ok(result) => result,
            Err(_) => return,
        }
    } else {
        (baseline.clone(), browser::KimiBrowserScanSummary::default())
    };
    if !is_current_account_refresh(generation) {
        return;
    }

    let needs_webview = store.accounts.iter().any(|record| {
        record.webview_session_id.is_some()
            && !browser_summary
                .refreshed_account_ids
                .contains(&record.account_id)
    });
    if needs_webview && crate::shell::settings_window::ensure_hidden(app).is_err() {
        let mut snapshots = store
            .accounts
            .iter()
            .map(|record| {
                snapshot_without_webview_refresh(record, &browser_summary).unwrap_or_else(|| {
                    snapshot_after_refresh_error(
                        record,
                        Some(snapshot_for_record(record)),
                        &KimiAccountRefreshError::Temporary,
                    )
                })
            })
            .collect::<Vec<_>>();
        apply_cached_credential_matches(&store, &mut snapshots);
        let Ok(latest) = load_kimi_account_store(app) else {
            return;
        };
        if is_current_account_refresh(generation) && store == latest {
            publish_cache(app, snapshots);
            spawn_credential_match_refresh(app.clone(), latest);
        }
        return;
    }

    let mut handles = Vec::with_capacity(store.accounts.len());
    let mut snapshots = Vec::with_capacity(store.accounts.len());
    for record in store.accounts.iter().cloned() {
        if let Some(snapshot) = snapshot_without_webview_refresh(&record, &browser_summary) {
            snapshots.push(snapshot);
            continue;
        }
        let app = app.clone();
        let previous = snapshot_for_record(&record);
        handles.push(tauri::async_runtime::spawn(async move {
            let result = fetch_account_snapshot(app, &record, generation).await;
            let snapshot = result.unwrap_or_else(|error| {
                snapshot_after_refresh_error(&record, Some(previous), &error)
            });
            (record, snapshot)
        }));
    }

    for handle in handles {
        if let Ok((_record, snapshot)) = handle.await {
            snapshots.push(snapshot);
        }
    }
    apply_cached_credential_matches(&store, &mut snapshots);

    if !is_current_account_refresh(generation) {
        return;
    }
    if !kimi_monthly_quota_enabled(app) {
        close_all_background_webviews(app);
        publish_cache(app, Vec::new());
        return;
    }
    let latest = match try_mutate_kimi_account_store(app, |latest| {
        if !is_current_account_refresh(generation) || *latest != store {
            return Err("Kimi 账号数据已变化，请重试".to_string());
        }
        for snapshot in &snapshots {
            if let Some(record) = latest
                .accounts
                .iter_mut()
                .find(|record| record.account_id == snapshot.account_id)
            {
                apply_snapshot_to_record(record, snapshot);
            }
        }
        Ok(latest.clone())
    }) {
        Ok(latest) => latest,
        Err(_) => return,
    };
    if is_current_account_refresh(generation) {
        publish_cache(app, snapshots);
    }
    if is_current_account_refresh(generation) {
        spawn_credential_match_refresh(app.clone(), latest);
    }
}

fn apply_cached_credential_matches(
    store: &KimiAccountStore,
    snapshots: &mut [KimiAccountSnapshot],
) {
    let keys = match load_api_keys_for_matching() {
        Ok(keys) => keys,
        Err(error) => {
            tracing::warn!(error = %error, "Kimi credential matching skipped: API key store unavailable");
            return;
        }
    };
    for snapshot in snapshots.iter_mut() {
        snapshot.matched_credential_ids.clear();
    }
    let revisions = keys
        .active_entries("kimi")
        .map(|entry| (entry.id, secret_revision(&entry.secret)))
        .collect::<HashMap<_, _>>();
    let identities = code_identity_cache()
        .lock()
        .map(|mut cached| {
            cached.retain(|id, value| revisions.get(id) == Some(&value.secret_revision));
            cached
                .iter()
                .map(|(id, value)| (*id, value.identity.clone()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for (credential_id, identity) in identities {
        let Some(account_id) = store.account_id_for_identity(&identity) else {
            continue;
        };
        if let Some(snapshot) = snapshots
            .iter_mut()
            .find(|snapshot| snapshot.account_id == account_id)
        {
            add_matched_credential(snapshot, credential_id);
        }
    }
}

fn spawn_credential_match_refresh(app: AppHandle, store: KimiAccountStore) {
    let generation = ACCOUNT_REFRESH_GENERATION.load(Ordering::SeqCst);
    tauri::async_runtime::spawn(async move {
        let mut matched = store
            .accounts
            .iter()
            .map(snapshot_for_record)
            .collect::<Vec<_>>();
        let Ok(source_revisions) = attach_matched_credentials(&store, &mut matched).await else {
            return;
        };
        let Ok(latest_store) = load_kimi_account_store(&app) else {
            return;
        };
        let enabled = kimi_monthly_quota_enabled(&app);
        let Ok(latest_keys) = load_api_keys_for_matching() else {
            return;
        };
        let latest_revisions = latest_keys
            .active_entries("kimi")
            .map(|entry| (entry.id, secret_revision(&entry.secret)))
            .collect::<HashMap<_, _>>();
        if !credential_match_publish_is_valid(
            generation,
            ACCOUNT_REFRESH_GENERATION.load(Ordering::SeqCst),
            &store,
            &latest_store,
            enabled,
        ) || !same_credential_revisions(&source_revisions, &latest_revisions)
        {
            return;
        }
        if let Ok(mut guard) = cache().lock() {
            merge_credential_matches(&mut guard.snapshots, &matched);
            emit_kimi_state(&app, guard.presented(), guard.storage_failed);
        }
    });
}

async fn attach_matched_credentials(
    store: &KimiAccountStore,
    snapshots: &mut [KimiAccountSnapshot],
) -> Result<HashMap<Uuid, u64>, String> {
    let keys = load_api_keys_for_matching()?;
    for snapshot in snapshots.iter_mut() {
        snapshot.matched_credential_ids.clear();
    }
    let entries = keys
        .active_entries("kimi")
        .map(|entry| {
            (
                entry.id,
                entry.secret.clone(),
                secret_revision(&entry.secret),
            )
        })
        .collect::<Vec<_>>();
    let revisions = entries
        .iter()
        .map(|(id, _, revision)| (*id, *revision))
        .collect::<HashMap<_, _>>();

    let mut pending = Vec::new();
    if let Ok(mut cached) = code_identity_cache().lock() {
        cached.retain(|id, value| revisions.get(id) == Some(&value.secret_revision));
        for (id, secret, revision) in &entries {
            if !cached.contains_key(id) {
                pending.push((*id, secret.clone(), *revision));
            }
        }
    }

    let mut handles = Vec::with_capacity(pending.len());
    for (credential_id, secret, revision) in pending {
        handles.push(tauri::async_runtime::spawn(async move {
            let permit = crate::commands::provider_fetch_permits()
                .acquire_owned()
                .await;
            let identity = match permit {
                Ok(_permit) => fetch_code_api_identity(&secret).await.ok(),
                Err(_) => None,
            };
            (credential_id, revision, identity)
        }));
    }

    let mut fetched = Vec::new();
    for handle in handles {
        if let Ok(result) = handle.await {
            fetched.push(result);
        }
    }
    let latest_keys = load_api_keys_for_matching()?;
    let latest_revisions = latest_keys
        .active_entries("kimi")
        .map(|entry| (entry.id, secret_revision(&entry.secret)))
        .collect::<HashMap<_, _>>();
    if let Ok(mut cached) = code_identity_cache().lock() {
        for (credential_id, revision, identity) in fetched {
            if latest_revisions.get(&credential_id) == Some(&revision)
                && let Some(identity) = identity
            {
                cached.insert(
                    credential_id,
                    CachedCodeIdentity {
                        secret_revision: revision,
                        identity,
                    },
                );
            }
        }
        cached.retain(|id, value| latest_revisions.get(id) == Some(&value.secret_revision));
    }

    let identities = code_identity_cache()
        .lock()
        .map(|cached| {
            cached
                .iter()
                .filter(|(id, value)| latest_revisions.get(id) == Some(&value.secret_revision))
                .map(|(id, value)| (*id, value.identity.clone()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for (credential_id, identity) in identities {
        let Some(account_id) = store.account_id_for_identity(&identity) else {
            continue;
        };
        if let Some(snapshot) = snapshots
            .iter_mut()
            .find(|snapshot| snapshot.account_id == account_id)
        {
            add_matched_credential(snapshot, credential_id);
        }
    }
    Ok(latest_revisions)
}

fn should_refresh_on_install(
    enabled: bool,
    browser_consent_accepted: bool,
    account_count: usize,
) -> bool {
    enabled && (browser_consent_accepted || account_count > 0)
}

pub fn install(app: AppHandle, enabled: bool, browser_consent_accepted: bool) {
    let Ok(mut store) = load_kimi_account_store(&app) else {
        return;
    };
    let mut pending_sessions = store.pending_session_cleanup.clone();
    let pending_browser_provisioning = store.pending_browser_secret_provisioning.clone();
    if !pending_browser_provisioning.is_empty() {
        match mutate_kimi_account_store(&app, |latest| {
            let cleanup_ids =
                resolve_browser_secret_provisioning(latest, &pending_browser_provisioning);
            (latest.clone(), cleanup_ids)
        }) {
            Ok((committed, _)) => store = committed,
            Err(error) => {
                // Keep the original marker on disk; deleting first would turn a
                // recoverable crash into an untracked credential orphan.
                tracing::warn!(error = %error, "Kimi browser credential startup recovery was deferred");
            }
        }
    }
    let pending_browser_secrets = store.pending_browser_secret_cleanup.clone();
    let persisted = store
        .accounts
        .iter()
        .filter_map(|account| account.webview_session_id.clone())
        .collect::<Vec<_>>();
    let directory_names = session_root()
        .ok()
        .and_then(|root| std::fs::read_dir(root).ok())
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            entry
                .file_type()
                .ok()
                .filter(|kind| kind.is_dir())
                .map(|_| entry)
        })
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect::<Vec<_>>();
    let orphaned = orphaned_session_ids(directory_names, &persisted);
    if !orphaned.is_empty() {
        match mutate_kimi_account_store(&app, |latest| {
            for session_id in &orphaned {
                if !latest.accounts.iter().any(|account| {
                    account.webview_session_id.as_deref() == Some(session_id.as_str())
                }) {
                    mark_session_cleanup(latest, session_id);
                }
            }
            latest.clone()
        }) {
            Ok(committed) => {
                store = committed;
                for session_id in orphaned {
                    if !pending_sessions.contains(&session_id) {
                        pending_sessions.push(session_id);
                    }
                }
            }
            Err(_) => return,
        }
    }
    for session_id in pending_sessions {
        schedule_session_cleanup(&app, &session_id, false);
    }
    retry_browser_secret_cleanup(&app, pending_browser_secrets);
    if enabled {
        publish_cache(
            &app,
            store.accounts.iter().map(snapshot_for_record).collect(),
        );
    }
    if should_refresh_on_install(enabled, browser_consent_accepted, store.accounts.len()) {
        tauri::async_runtime::spawn(async move {
            refresh_kimi_accounts(&app).await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codexbar::secure_file::SecretProtector;
    use codexbar::storage::{
        LoadState, Protection, StoragePaths, StoreError, StoreKind, UserScopeId,
    };
    use std::io;

    #[test]
    fn cache_publication_cannot_clear_a_storage_failure_without_verified_io() {
        for clear_accounts in [false, true] {
            let record = stored_account(Uuid::new_v4(), "fixture", "user", "global", "Fixture");
            let account_cache = Mutex::new(KimiAccountCache::default());
            let failed = snapshot_from_stored_quota_with_status(&record, "storageError").unwrap();
            publish_cache_with(&account_cache, vec![failed], |_, failed| assert!(failed));
            let stale_publication = if clear_accounts {
                Vec::new()
            } else {
                vec![snapshot_from_stored_quota_with_status(&record, "ok").unwrap()]
            };
            publish_cache_with(&account_cache, stale_publication, |snapshots, failed| {
                assert!(
                    failed,
                    "an unverified cache publication cleared the storage failure"
                );
                if !clear_accounts {
                    assert_eq!(snapshots[0].status, "storageError");
                    assert_eq!(snapshots[0].used_percent, Some(42.5));
                }
            });
        }
    }

    #[test]
    fn storage_failure_without_accounts_survives_an_empty_publication() {
        let account_cache = Mutex::new(KimiAccountCache::default());
        let result: Result<Result<(), String>, StoreError> = observe_kimi_storage_with(
            &account_cache,
            false,
            || Err(kimi_account_invalid_payload()),
            |snapshots, failed| {
                assert!(snapshots.is_empty());
                assert!(failed);
            },
        );
        assert!(result.is_err());
        publish_cache_with(&account_cache, Vec::new(), |snapshots, failed| {
            assert!(snapshots.is_empty());
            assert!(failed);
        });
    }

    #[test]
    fn verified_read_recovers_read_failure_but_keeps_last_good_quota_stale() {
        let record = stored_account(Uuid::new_v4(), "fixture", "user", "global", "Fixture");
        let account_cache = Mutex::new(KimiAccountCache::default());
        publish_cache_with(
            &account_cache,
            vec![snapshot_from_stored_quota_with_status(&record, "ok").unwrap()],
            |_, _| {},
        );
        let failed: Result<Result<(), String>, StoreError> = observe_kimi_storage_with(
            &account_cache,
            false,
            || Err(kimi_account_invalid_payload()),
            |snapshots, failed| {
                assert!(failed);
                assert_eq!(snapshots[0].status, "storageError");
                assert_eq!(snapshots[0].used_percent, Some(42.5));
            },
        );
        assert!(failed.is_err());
        let recovered = observe_kimi_storage_with(
            &account_cache,
            false,
            || Ok(Ok::<_, String>(())),
            |snapshots, failed| {
                assert!(!failed);
                assert_eq!(snapshots[0].status, "stale");
                assert_eq!(snapshots[0].used_percent, Some(42.5));
                assert_eq!(
                    snapshots[0].resets_at.as_deref(),
                    Some("2026-09-01T00:00:00Z")
                );
            },
        );
        assert!(matches!(recovered, Ok(Ok(()))));
        assert!(!account_cache.lock().unwrap().storage_failed);
    }

    #[test]
    fn write_failure_requires_a_verified_write_not_a_read_or_cancelled_mutation() {
        let account_cache = Mutex::new(KimiAccountCache::default());
        let failed: Result<Result<(), String>, StoreError> = observe_kimi_storage_with(
            &account_cache,
            true,
            || Err(kimi_account_invalid_payload()),
            |_, _| {},
        );
        assert!(failed.is_err());
        let read = observe_kimi_storage_with(
            &account_cache,
            false,
            || Ok(Ok::<_, String>(())),
            |_, _| panic!("read acknowledged a failed write"),
        );
        assert!(matches!(read, Ok(Ok(()))));
        let cancelled = observe_kimi_storage_with(
            &account_cache,
            true,
            || Ok(Err::<(), _>("cancelled")),
            |_, _| panic!("uncommitted mutation acknowledged a failed write"),
        );
        assert!(matches!(cancelled, Ok(Err("cancelled"))));
        assert!(account_cache.lock().unwrap().storage_failed);
        let saved = observe_kimi_storage_with(
            &account_cache,
            true,
            || Ok(Ok::<_, String>(())),
            |_, failed| assert!(!failed),
        );
        assert!(matches!(saved, Ok(Ok(()))));
        assert!(!account_cache.lock().unwrap().storage_failed);
    }

    #[test]
    fn an_inflight_success_cannot_acknowledge_a_newer_storage_failure() {
        for write in [false, true] {
            let account_cache = Mutex::new(KimiAccountCache::default());
            let completed = observe_kimi_storage_with(
                &account_cache,
                write,
                || {
                    let failed: Result<Result<(), String>, StoreError> = observe_kimi_storage_with(
                        &account_cache,
                        false,
                        || Err(kimi_account_invalid_payload()),
                        |_, failed| assert!(failed),
                    );
                    assert!(failed.is_err());
                    Ok(Ok::<_, String>(()))
                },
                |_, _| panic!("older operation cleared the newer failure"),
            );
            assert!(matches!(completed, Ok(Ok(()))));
            assert!(account_cache.lock().unwrap().storage_failed);
        }
    }

    #[test]
    fn recovery_after_first_load_failure_publishes_the_accounts_actually_read() {
        let (root, _paths, repository) = repository_fixture();
        let record = stored_account(Uuid::new_v4(), "fixture", "user", "global", "Fixture");
        repository
            .mutate(|store| store.accounts.push(record.clone()))
            .unwrap();
        let account_cache = Mutex::new(KimiAccountCache::default());
        account_cache.lock().unwrap().fail_storage(false);
        let loaded =
            load_kimi_account_store_with(&repository, &account_cache, |snapshots, failed| {
                assert!(!failed);
                assert_eq!(
                    snapshots.len(),
                    1,
                    "recovery published the old empty cache instead of the saved accounts"
                );
                assert_eq!(snapshots[0].account_id, record.account_id);
                assert_eq!(snapshots[0].status, "stale");
                assert_eq!(snapshots[0].used_percent, Some(42.5));
            })
            .unwrap()
            .unwrap();
        assert_eq!(loaded.accounts.len(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[derive(Debug)]
    struct IdentityProtector;

    impl SecretProtector for IdentityProtector {
        fn protect_current_user(&self, plain: &[u8]) -> io::Result<Vec<u8>> {
            Ok(plain.to_vec())
        }

        fn unprotect_current_user(&self, encrypted: &[u8]) -> io::Result<Vec<u8>> {
            Ok(encrypted.to_vec())
        }
    }

    fn repository_fixture() -> (PathBuf, StoragePaths, KimiAccountRepository) {
        let root =
            std::env::temp_dir().join(format!("codexbar-kimi-repository-{}", Uuid::new_v4()));
        let paths = StoragePaths::from_roots(root.join("roaming"), root.join("local"));
        let repository = KimiAccountRepository::with_protection(
            paths.clone(),
            UserScopeId::from_stable_identifier(format!("kimi-test-{}", Uuid::new_v4()).as_bytes()),
            Protection::CurrentUserDpapi,
            Arc::new(IdentityProtector),
        );
        (root, paths, repository)
    }

    fn stored_account(
        account_id: Uuid,
        session_id: &str,
        user: &str,
        global: &str,
        display_name: &str,
    ) -> KimiAccountRecord {
        KimiAccountRecord {
            account_id,
            webview_session_id: Some(session_id.to_string()),
            browser_sources: vec![browser_source(KimiBrowserKind::Edge, "Default")],
            identity: identity(user, global),
            display_name: display_name.to_string(),
            last_quota: Some(KimiStoredQuota {
                used_percent: 42.5,
                resets_at: Some("2026-09-01T00:00:00Z".into()),
                updated_at: "2026-08-30T00:00:00Z".into(),
            }),
        }
    }

    #[test]
    fn login_webview_creation_command_remains_async() {
        fn assert_async_command<F, Fut>(_command: F)
        where
            F: Fn(AppHandle, Option<Uuid>) -> Fut,
            Fut: std::future::Future<Output = Result<KimiLoginSession, String>>,
        {
        }

        assert_async_command(begin_kimi_account_login);
    }

    fn identity(user: &str, global: &str) -> KimiIdentityFingerprint {
        KimiIdentityFingerprint {
            user_id_fingerprint: user.to_string(),
            global_id_fingerprint: global.to_string(),
        }
    }

    fn browser_source(browser: KimiBrowserKind, profile_name: &str) -> KimiBrowserSourceRecord {
        KimiBrowserSourceRecord {
            source_id: Uuid::new_v4(),
            browser,
            profile_name: profile_name.to_string(),
            profile_path: PathBuf::from(format!(r"C:\Browser\{profile_name}")),
        }
    }

    #[test]
    fn install_refreshes_an_empty_store_when_browser_consent_is_enabled() {
        assert!(should_refresh_on_install(true, true, 0));
        assert!(!should_refresh_on_install(true, false, 0));
        assert!(!should_refresh_on_install(false, true, 0));
        assert!(should_refresh_on_install(true, false, 1));
    }

    #[test]
    fn browser_snapshot_contains_only_safe_source_labels() {
        let record = KimiAccountRecord {
            account_id: Uuid::new_v4(),
            webview_session_id: None,
            browser_sources: vec![KimiBrowserSourceRecord {
                source_id: Uuid::new_v4(),
                browser: KimiBrowserKind::Chrome,
                profile_name: "Profile 1".into(),
                profile_path: PathBuf::from(r"C:\Users\Example\Chrome\User Data\Profile 1"),
            }],
            identity: identity("chrome-user", "chrome-global"),
            display_name: "Chrome 账号".into(),
            last_quota: None,
        };

        let json = serde_json::to_string(&empty_snapshot(&record, "loading")).unwrap();

        assert!(json.contains("Google Chrome · Profile 1"));
        assert!(!json.contains(r"C:\\Users"));
        assert!(!json.contains("sourceId"));
        assert!(!json.contains("profilePath"));
    }

    #[test]
    fn manual_scan_requires_enabled_feature_and_browser_consent() {
        assert_eq!(
            validate_browser_scan_settings(false, true).unwrap_err(),
            "请先开启 Kimi 月额度"
        );
        assert_eq!(
            validate_browser_scan_settings(true, false).unwrap_err(),
            "请先允许读取浏览器 Kimi 登录状态"
        );
        assert!(validate_browser_scan_settings(true, true).is_ok());
    }

    #[test]
    fn current_session_id_json_loads_as_webview_session_source() {
        let (dir, paths, repository) = repository_fixture();
        let path = paths.legacy_kimi_accounts();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{
              "version": 1,
              "accounts": [{
                "account_id": "00000000-0000-0000-0000-000000000001",
                "session_id": "00000000-0000-0000-0000-000000000002",
                "identity": {
                  "user_id_fingerprint": "user-a",
                  "global_id_fingerprint": "global-a"
                },
                "display_name": "账号 A",
                "last_quota": null
              }],
              "pending_session_cleanup": []
            }"#,
        )
        .unwrap();

        let LoadState::Loaded(loaded) = repository.load().unwrap() else {
            panic!("legacy account was not migrated");
        };

        assert_eq!(
            loaded.accounts[0].webview_session_id.as_deref(),
            Some("00000000-0000-0000-0000-000000000002")
        );
        assert!(loaded.accounts[0].browser_sources.is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn same_identity_adds_browser_source_without_a_duplicate_account() {
        let mut store = KimiAccountStore::default();
        let web = store.upsert_authenticated(
            "session-a".into(),
            identity("user-a", "global-a"),
            Some("账号 A".into()),
        );
        let browser = store.upsert_browser_source(
            identity("user-a", "global-a"),
            "账号 A".into(),
            browser_source(KimiBrowserKind::Edge, "Default"),
        );

        assert_eq!(store.accounts.len(), 1);
        assert_eq!(web.account_id, browser.account_id);
        assert_eq!(store.accounts[0].browser_sources.len(), 1);
    }

    #[test]
    fn different_browser_identities_remain_independent_accounts() {
        let mut store = KimiAccountStore::default();
        store.upsert_browser_source(
            identity("edge-user", "edge-global"),
            "Edge 账号".into(),
            browser_source(KimiBrowserKind::Edge, "Default"),
        );
        store.upsert_browser_source(
            identity("chrome-user", "chrome-global"),
            "Chrome 账号".into(),
            browser_source(KimiBrowserKind::Chrome, "Profile 1"),
        );

        assert_eq!(store.accounts.len(), 2);
        assert_ne!(store.accounts[0].account_id, store.accounts[1].account_id);
    }

    #[test]
    fn same_account_revision_changes_when_browser_source_changes() {
        let mut before = KimiAccountStore::default();
        before.upsert_browser_source(
            identity("user-a", "global-a"),
            "账号 A".into(),
            browser_source(KimiBrowserKind::Edge, "Default"),
        );
        let mut after = before.clone();
        after.accounts[0].browser_sources[0].profile_name = "Profile 2".into();

        assert!(!same_account_revision(&before, &after));
    }

    #[test]
    fn regular_account_revision_still_tracks_pending_credential_lifecycle_changes() {
        let before = KimiAccountStore::default();
        let mut after = before.clone();
        after
            .pending_browser_secret_cleanup
            .push(Uuid::new_v4());

        assert!(!same_account_revision(&before, &after));
        assert!(same_browser_scan_revision(&before, &after));
    }

    #[test]
    fn navigation_allows_only_kimi_https_hosts() {
        assert!(is_allowed_kimi_navigation(
            &"https://www.kimi.com/".parse().unwrap()
        ));
        assert!(is_allowed_kimi_navigation(
            &"https://login.moonshot.cn/authorize".parse().unwrap()
        ));
        assert!(!is_allowed_kimi_navigation(
            &"http://www.kimi.com/".parse().unwrap()
        ));
        assert!(!is_allowed_kimi_navigation(
            &"https://kimi.com.example.invalid/".parse().unwrap()
        ));
        assert!(!is_allowed_kimi_navigation(
            &"https://accounts.google.com/".parse().unwrap()
        ));
    }

    #[test]
    fn cookie_selection_accepts_only_known_nonempty_names() {
        assert_eq!(
            select_auth_token([("other", "x"), ("kimi-auth", " token ")]),
            Some("token".into())
        );
        assert_eq!(select_auth_token([("kimi-auth", "  ")]), None);
    }

    #[test]
    fn rejected_kimi_credential_reports_an_expired_login() {
        assert_eq!(
            KimiAccountRefreshError::Authentication.user_message(),
            "Kimi 登录状态已失效，请重新登录"
        );
    }

    #[test]
    fn missing_webview_credential_has_its_own_error() {
        assert_eq!(
            missing_auth_token_error(),
            KimiAccountRefreshError::MissingCredential
        );
        assert_eq!(
            missing_auth_token_error().user_message(),
            "未能从 Kimi 登录窗口读取登录凭据，请确认登录完成后重试"
        );
    }

    #[test]
    fn local_storage_script_result_returns_the_access_token() {
        assert_eq!(
            parse_access_token_script_result(r#""  access-token  ""#).unwrap(),
            Some("access-token".into())
        );
        assert_eq!(parse_access_token_script_result("null").unwrap(), None);
    }

    #[test]
    fn account_store_deduplicates_strict_identity_and_replaces_session() {
        let mut store = KimiAccountStore::default();
        let first = store.upsert_authenticated(
            "session-a".into(),
            identity("user-a", "global-a"),
            Some("账号 A".into()),
        );
        let duplicate = store.upsert_authenticated(
            "session-b".into(),
            identity("user-a", "global-a"),
            Some("账号 A 新会话".into()),
        );

        assert_eq!(first.account_id, duplicate.account_id);
        assert_eq!(store.accounts.len(), 1);
        assert_eq!(
            store.accounts[0].webview_session_id.as_deref(),
            Some("session-b")
        );
        assert_eq!(duplicate.replaced_session_id.as_deref(), Some("session-a"));
    }

    #[test]
    fn account_store_keeps_different_identity_pairs_separate() {
        let mut store = KimiAccountStore::default();
        store.upsert_authenticated("session-a".into(), identity("user-a", "global-a"), None);
        store.upsert_authenticated("session-b".into(), identity("user-a", "global-b"), None);
        assert_eq!(store.accounts.len(), 2);
    }

    #[test]
    fn secure_store_roundtrip_keeps_only_fingerprints() {
        let (dir, paths, repository) = repository_fixture();
        let mut store = KimiAccountStore::default();
        store.upsert_authenticated(
            "session-a".into(),
            identity("hashed-user", "hashed-global"),
            Some("账号 A".into()),
        );

        repository.mutate(|saved| *saved = store.clone()).unwrap();
        let LoadState::Loaded(loaded) = repository.load().unwrap() else {
            panic!("saved account was not loaded");
        };
        assert_eq!(loaded.accounts, store.accounts);
        let raw = std::fs::read_to_string(paths.kimi_accounts()).unwrap();
        assert!(!raw.contains("hashed-user"));
        assert!(!raw.contains("hashed-global"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn corrupt_secure_store_is_reported_instead_of_becoming_empty() {
        let (dir, paths, repository) = repository_fixture();
        paths.ensure_directories().unwrap();
        let path = paths.kimi_accounts();
        std::fs::write(path, b"not-a-secure-store").unwrap();

        assert!(matches!(
            repository.load().unwrap(),
            LoadState::InvalidPayload { .. }
        ));
        assert!(repository.mutate(|store| store.accounts.clear()).is_err());
        assert_eq!(std::fs::read(path).unwrap(), b"not-a-secure-store");

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn repository_migrates_legacy_accounts_without_changing_identity_or_session_mapping() {
        let (root, paths, repository) = repository_fixture();
        paths.ensure_directories().unwrap();
        let account_id = Uuid::new_v4();
        let session_id = Uuid::new_v4().to_string();
        let source_id = Uuid::new_v4();
        let legacy = serde_json::json!({
            "version": 1,
            "accounts": [{
                "account_id": account_id,
                "session_id": session_id,
                "browser_sources": [{
                    "source_id": source_id,
                    "browser": "edge",
                    "profile_name": "Default",
                    "profile_path": "C:\\Browser\\Default"
                }],
                "identity": {
                    "user_id_fingerprint": "legacy-user-fingerprint",
                    "global_id_fingerprint": "legacy-global-fingerprint"
                },
                "display_name": "旧账号",
                "last_quota": {
                    "usedPercent": 42.5,
                    "resetsAt": "2026-09-01T00:00:00Z",
                    "updatedAt": "2026-08-30T00:00:00Z"
                }
            }],
            "suppressed_browser_profiles": ["suppressed-fingerprint"],
            "pending_session_cleanup": ["pending-session"]
        });
        let protected = codexbar::secure_file::protected_file_bytes_with(
            &serde_json::to_string(&legacy).unwrap(),
            Protection::CurrentUserDpapi,
            &IdentityProtector,
        )
        .unwrap();
        std::fs::create_dir_all(paths.legacy_kimi_accounts().parent().unwrap()).unwrap();
        std::fs::write(paths.legacy_kimi_accounts(), protected).unwrap();

        let LoadState::Loaded(migrated) = repository.load().unwrap() else {
            panic!("legacy Kimi account store was not migrated");
        };

        assert_eq!(migrated.accounts.len(), 1);
        let account = &migrated.accounts[0];
        assert_eq!(account.account_id, account_id);
        assert_eq!(
            account.webview_session_id.as_deref(),
            Some(session_id.as_str())
        );
        assert_eq!(account.browser_sources[0].source_id, source_id);
        assert_eq!(
            account.identity.user_id_fingerprint,
            "legacy-user-fingerprint"
        );
        assert_eq!(
            account.identity.global_id_fingerprint,
            "legacy-global-fingerprint"
        );
        assert_eq!(account.display_name, "旧账号");
        assert_eq!(account.last_quota.as_ref().unwrap().used_percent, 42.5);
        assert_eq!(
            migrated.suppressed_browser_profiles,
            vec!["suppressed-fingerprint"]
        );
        assert_eq!(migrated.pending_session_cleanup, vec!["pending-session"]);
        assert!(paths.kimi_accounts().exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn repository_ignores_a_later_legacy_write_after_v2_migration() {
        let (root, paths, repository) = repository_fixture();
        paths.ensure_directories().unwrap();
        let original_id = Uuid::new_v4();
        let original = KimiAccountStore {
            version: 1,
            accounts: vec![stored_account(
                original_id,
                "00000000-0000-0000-0000-000000000003",
                "original-user",
                "original-global",
                "原账号",
            )],
            suppressed_browser_profiles: Vec::new(),
            pending_session_cleanup: Vec::new(),
            pending_browser_secret_cleanup: Vec::new(),
            pending_browser_secret_provisioning: Vec::new(),
        };
        let legacy_bytes = codexbar::secure_file::protected_file_bytes_with(
            &serde_json::to_string(&original).unwrap(),
            Protection::CurrentUserDpapi,
            &IdentityProtector,
        )
        .unwrap();
        std::fs::create_dir_all(paths.legacy_kimi_accounts().parent().unwrap()).unwrap();
        std::fs::write(paths.legacy_kimi_accounts(), legacy_bytes).unwrap();
        assert!(matches!(repository.load().unwrap(), LoadState::Loaded(_)));

        let later = KimiAccountStore {
            version: 1,
            accounts: vec![stored_account(
                Uuid::new_v4(),
                "00000000-0000-0000-0000-000000000004",
                "later-user",
                "later-global",
                "旧版覆盖账号",
            )],
            suppressed_browser_profiles: Vec::new(),
            pending_session_cleanup: Vec::new(),
            pending_browser_secret_cleanup: Vec::new(),
            pending_browser_secret_provisioning: Vec::new(),
        };
        let later_bytes = codexbar::secure_file::protected_file_bytes_with(
            &serde_json::to_string(&later).unwrap(),
            Protection::CurrentUserDpapi,
            &IdentityProtector,
        )
        .unwrap();
        std::fs::write(paths.legacy_kimi_accounts(), later_bytes).unwrap();

        let LoadState::Loaded(reloaded) = repository.load().unwrap() else {
            panic!("v2 Kimi account store was not loaded");
        };
        assert_eq!(reloaded.accounts.len(), 1);
        assert_eq!(reloaded.accounts[0].account_id, original_id);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn repository_migration_preserves_the_webview_session_files_and_ignores_later_legacy_edits() {
        let (root, paths, repository) = repository_fixture();
        let session_id = Uuid::new_v4().to_string();
        let legacy_session = paths
            .legacy_kimi_accounts()
            .parent()
            .unwrap()
            .join("kimi-account-sessions")
            .join(&session_id);
        let legacy_data = legacy_session
            .join("Default")
            .join("Local Storage")
            .join("fixture.data");
        std::fs::create_dir_all(legacy_data.parent().unwrap()).unwrap();
        std::fs::write(&legacy_data, b"fixture-session-before-upgrade").unwrap();
        let mut legacy = KimiAccountStore {
            version: 1,
            ..KimiAccountStore::default()
        };
        legacy.accounts.push(stored_account(
            Uuid::new_v4(),
            &session_id,
            "user",
            "global",
            "账号",
        ));
        std::fs::write(
            paths.legacy_kimi_accounts(),
            serde_json::to_vec(&legacy).unwrap(),
        )
        .unwrap();

        assert!(matches!(repository.load().unwrap(), LoadState::Loaded(_)));
        let migrated_data = repository
            .session_root()
            .unwrap()
            .join(&session_id)
            .join("Default")
            .join("Local Storage")
            .join("fixture.data");
        assert!(
            migrated_data.is_file(),
            "metadata migration left the login session behind"
        );
        assert_eq!(
            std::fs::read(&migrated_data).unwrap(),
            b"fixture-session-before-upgrade"
        );
        assert_eq!(
            std::fs::read(&legacy_data).unwrap(),
            b"fixture-session-before-upgrade"
        );
        std::fs::write(&legacy_data, b"later-old-version-write").unwrap();
        assert!(matches!(repository.load().unwrap(), LoadState::Loaded(_)));
        assert_eq!(
            std::fs::read(migrated_data).unwrap(),
            b"fixture-session-before-upgrade"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn session_migration_conflict_preserves_both_profiles_and_does_not_commit_metadata() {
        let (root, paths, repository) = repository_fixture();
        let session_id = Uuid::new_v4().to_string();
        let legacy_session = paths
            .legacy_kimi_accounts()
            .parent()
            .unwrap()
            .join("kimi-account-sessions")
            .join(&session_id);
        let destination = repository.session_root().unwrap().join(&session_id);
        std::fs::create_dir_all(&legacy_session).unwrap();
        std::fs::create_dir_all(&destination).unwrap();
        std::fs::write(legacy_session.join("fixture.data"), b"legacy-session").unwrap();
        std::fs::write(destination.join("fixture.data"), b"newer-session").unwrap();
        let legacy = KimiAccountStore {
            version: 1,
            accounts: vec![stored_account(
                Uuid::new_v4(),
                &session_id,
                "user",
                "global",
                "Account",
            )],
            ..KimiAccountStore::default()
        };
        let before = serde_json::to_vec(&legacy).unwrap();
        std::fs::write(paths.legacy_kimi_accounts(), &before).unwrap();

        assert!(repository.load().is_err());
        assert!(
            !paths.kimi_accounts().exists(),
            "metadata committed despite a session conflict"
        );
        assert_eq!(std::fs::read(paths.legacy_kimi_accounts()).unwrap(), before);
        assert_eq!(
            std::fs::read(legacy_session.join("fixture.data")).unwrap(),
            b"legacy-session"
        );
        assert_eq!(
            std::fs::read(destination.join("fixture.data")).unwrap(),
            b"newer-session"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn corrupt_v2_store_blocks_mutation_without_overwriting_original_bytes() {
        let (root, paths, repository) = repository_fixture();
        paths.ensure_directories().unwrap();
        let corrupt = b"truncated-kimi-store".to_vec();
        std::fs::write(paths.kimi_accounts(), &corrupt).unwrap();

        assert!(matches!(
            repository.load().unwrap(),
            LoadState::InvalidPayload { .. }
        ));
        let error = repository
            .mutate(|store| {
                store.accounts.push(stored_account(
                    Uuid::new_v4(),
                    "new-session",
                    "new-user",
                    "new-global",
                    "新账号",
                ));
            })
            .unwrap_err();
        assert!(matches!(
            error,
            StoreError::InvalidPayload {
                store: StoreKind::KimiAccounts,
                ..
            }
        ));
        assert_eq!(std::fs::read(paths.kimi_accounts()).unwrap(), corrupt);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn future_v2_version_blocks_mutation_without_downgrading_the_file() {
        let (root, paths, repository) = repository_fixture();
        paths.ensure_directories().unwrap();
        let future_json = serde_json::json!({
            "version": 99,
            "accounts": [],
            "suppressed_browser_profiles": [],
            "pending_session_cleanup": []
        });
        let future_bytes = codexbar::secure_file::protected_file_bytes_with(
            &serde_json::to_string(&future_json).unwrap(),
            Protection::CurrentUserDpapi,
            &IdentityProtector,
        )
        .unwrap();
        std::fs::write(paths.kimi_accounts(), &future_bytes).unwrap();

        assert!(matches!(
            repository.load().unwrap(),
            LoadState::UnsupportedVersion {
                found: 99,
                supported: KIMI_ACCOUNT_STORE_VERSION
            }
        ));
        assert!(repository.mutate(|_| ()).is_err());
        assert_eq!(std::fs::read(paths.kimi_accounts()).unwrap(), future_bytes);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn existing_v2_store_is_upgraded_to_v3_before_provisioning_markers_are_written() {
        let (root, paths, repository) = repository_fixture();
        paths.ensure_directories().unwrap();
        let v2_store = KimiAccountStore {
            version: 2,
            accounts: vec![stored_account(
                Uuid::new_v4(),
                "v2-session",
                "v2-user",
                "v2-global",
                "V2 account",
            )],
            ..KimiAccountStore::default()
        };
        let v2_bytes = codexbar::secure_file::protected_file_bytes_with(
            &serde_json::to_string(&v2_store).unwrap(),
            Protection::CurrentUserDpapi,
            &IdentityProtector,
        )
        .unwrap();
        std::fs::write(paths.kimi_accounts(), v2_bytes).unwrap();

        let LoadState::Loaded(migrated) = repository.load().unwrap() else {
            panic!("existing v2 store was not upgraded")
        };
        assert_eq!(migrated.version, 3);
        assert!(migrated.pending_browser_secret_provisioning.is_empty());

        let source_id = Uuid::new_v4();
        repository
            .mutate(|store| reserve_browser_secret_provisioning(store, &[source_id]).unwrap())
            .unwrap();

        let raw = std::fs::read(paths.kimi_accounts()).unwrap();
        let payload = repository.decode_payload(&raw).unwrap();
        let value: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(value["version"], 3);
        assert_eq!(
            value["pending_browser_secret_provisioning"],
            serde_json::json!([source_id])
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn concurrent_repository_transactions_preserve_both_accounts() {
        let (root, _paths, repository) = repository_fixture();
        let first = repository.clone();
        let second = repository.clone();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let first_barrier = Arc::clone(&barrier);
        let second_barrier = Arc::clone(&barrier);

        let first_id = Uuid::new_v4();
        let second_id = Uuid::new_v4();
        let first_thread = std::thread::spawn(move || {
            first_barrier.wait();
            first
                .mutate(|store| {
                    store.accounts.push(stored_account(
                        first_id,
                        "first-session",
                        "first-user",
                        "first-global",
                        "账号一",
                    ));
                })
                .unwrap();
        });
        let second_thread = std::thread::spawn(move || {
            second_barrier.wait();
            second
                .mutate(|store| {
                    store.accounts.push(stored_account(
                        second_id,
                        "second-session",
                        "second-user",
                        "second-global",
                        "账号二",
                    ));
                })
                .unwrap();
        });
        first_thread.join().unwrap();
        second_thread.join().unwrap();

        let LoadState::Loaded(saved) = repository.load().unwrap() else {
            panic!("Kimi accounts were not saved");
        };
        let ids = saved
            .accounts
            .iter()
            .map(|account| account.account_id)
            .collect::<HashSet<_>>();
        assert_eq!(ids, HashSet::from([first_id, second_id]));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn browser_cleanup_rechecks_storage_before_deleting_any_secret() {
        let (root, paths, repository) = repository_fixture();
        let source_id = Uuid::new_v4();
        repository
            .mutate(|store| store.pending_browser_secret_cleanup.push(source_id))
            .unwrap();
        let secret = root.join("isolated-secret-fixture");
        std::fs::write(&secret, b"keep-me").unwrap();
        std::fs::write(paths.kimi_accounts(), b"broken-store").unwrap();

        assert!(
            retry_browser_secret_cleanup_with(&repository, &[source_id], |_| {
                std::fs::remove_file(&secret).map_err(|_| "fixture deletion failed".into())
            })
            .is_err()
        );

        assert!(
            secret.is_file(),
            "cleanup deleted a credential despite unreadable metadata"
        );
        assert_eq!(
            std::fs::read(paths.kimi_accounts()).unwrap(),
            b"broken-store"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn browser_cleanup_preserves_live_and_no_longer_pending_sources() {
        let (root, _paths, repository) = repository_fixture();
        let mut account = stored_account(Uuid::new_v4(), "session", "user", "global", "Account");
        account.webview_session_id = None;
        let live_source = account.browser_sources[0].source_id;
        let stale_request = Uuid::new_v4();
        repository
            .mutate(|store| {
                store.accounts.push(account.clone());
                store.pending_browser_secret_cleanup.push(live_source);
            })
            .unwrap();
        let live_secret = root.join(live_source.to_string());
        let stale_secret = root.join(stale_request.to_string());
        std::fs::write(&live_secret, b"live-secret").unwrap();
        std::fs::write(&stale_secret, b"newer-secret").unwrap();

        retry_browser_secret_cleanup_with(
            &repository,
            &[live_source, stale_request],
            |source_id| {
                std::fs::remove_file(root.join(source_id.to_string()))
                    .map_err(|_| "fixture deletion failed".into())
            },
        )
        .unwrap()
        .unwrap();

        assert!(
            live_secret.is_file(),
            "cleanup deleted an active account credential"
        );
        assert!(
            stale_secret.is_file(),
            "an obsolete cleanup request deleted a credential"
        );
        let LoadState::Loaded(saved) = repository.load().unwrap() else {
            panic!("store not loaded")
        };
        assert_eq!(saved.accounts, vec![account]);
        assert!(!saved.pending_browser_secret_cleanup.contains(&live_source));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn browser_cleanup_failure_keeps_durable_intent_for_retry() {
        let (root, paths, repository) = repository_fixture();
        let source_id = Uuid::new_v4();
        repository
            .mutate(|store| store.pending_browser_secret_cleanup.push(source_id))
            .unwrap();
        let before = std::fs::read(paths.kimi_accounts()).unwrap();
        let secret = root.join("isolated-secret-fixture");
        std::fs::write(&secret, b"pending-secret").unwrap();

        assert!(
            retry_browser_secret_cleanup_with(&repository, &[source_id], |_| {
                Err("credential backend unavailable".into())
            })
            .unwrap()
            .is_err()
        );
        assert_eq!(std::fs::read(paths.kimi_accounts()).unwrap(), before);
        assert!(secret.is_file());

        retry_browser_secret_cleanup_with(&repository, &[source_id], |_| {
            std::fs::remove_file(&secret).map_err(|_| "fixture deletion failed".into())
        })
        .unwrap()
        .unwrap();
        assert!(!secret.exists());
        let LoadState::Loaded(saved) = repository.load().unwrap() else {
            panic!("store not loaded")
        };
        assert!(saved.pending_browser_secret_cleanup.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn browser_secret_cleanup_retry_plan_is_bounded_and_uses_backoff() {
        let mut plan = BrowserSecretCleanupRetryPlan::default();

        assert_eq!(plan.next_delay(), Some(Duration::from_millis(250)));
        assert_eq!(plan.next_delay(), Some(Duration::from_millis(500)));
        assert_eq!(plan.next_delay(), Some(Duration::from_secs(1)));
        assert_eq!(plan.next_delay(), Some(Duration::from_secs(2)));
        assert_eq!(plan.next_delay(), None);
    }

    #[test]
    fn browser_cleanup_defers_while_a_refresh_owns_the_source() {
        let (root, _paths, repository) = repository_fixture();
        let source_id = Uuid::new_v4();
        repository
            .mutate(|store| store.pending_browser_secret_cleanup.push(source_id))
            .unwrap();
        let secret = root.join("isolated-secret-fixture");
        std::fs::write(&secret, b"refresh-in-flight").unwrap();
        let source_lock = browser::browser_source_lock(source_id);
        let refresh_guard = source_lock.try_lock().unwrap();

        let result = retry_browser_secret_cleanup_with(&repository, &[source_id], |_| {
            std::fs::remove_file(&secret).map_err(|_| "fixture deletion failed".into())
        });
        assert!(
            result.unwrap().is_err(),
            "cleanup ran while the refresh held the credential lock"
        );
        assert!(secret.is_file());
        let LoadState::Loaded(saved) = repository.load().unwrap() else {
            panic!("store not loaded")
        };
        assert_eq!(saved.pending_browser_secret_cleanup, vec![source_id]);

        drop(refresh_guard);
        retry_browser_secret_cleanup_with(&repository, &[source_id], |_| {
            std::fs::remove_file(&secret).map_err(|_| "fixture deletion failed".into())
        })
        .unwrap()
        .unwrap();
        assert!(!secret.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn browser_cleanup_acknowledgement_failure_keeps_intent_and_can_retry_idempotently() {
        struct RejectProtection;
        impl SecretProtector for RejectProtection {
            fn protect_current_user(&self, _plain: &[u8]) -> io::Result<Vec<u8>> {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "injected protection failure",
                ))
            }

            fn unprotect_current_user(&self, encrypted: &[u8]) -> io::Result<Vec<u8>> {
                Ok(encrypted.to_vec())
            }
        }

        let (root, paths, repository) = repository_fixture();
        let source_id = Uuid::new_v4();
        repository
            .mutate(|store| store.pending_browser_secret_cleanup.push(source_id))
            .unwrap();
        let before = std::fs::read(paths.kimi_accounts()).unwrap();
        let secret = root.join("isolated-secret-fixture");
        std::fs::write(&secret, b"pending-removal").unwrap();
        let mut failing_repository = repository.clone();
        failing_repository.protector = Arc::new(RejectProtection);

        assert!(
            retry_browser_secret_cleanup_with(&failing_repository, &[source_id], |_| {
                std::fs::remove_file(&secret).map_err(|_| "fixture deletion failed".into())
            })
            .is_err()
        );
        assert!(!secret.exists());
        assert_eq!(std::fs::read(paths.kimi_accounts()).unwrap(), before);

        retry_browser_secret_cleanup_with(
            &repository,
            &[source_id],
            |_| match std::fs::remove_file(&secret) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(_) => Err("fixture deletion failed".into()),
            },
        )
        .unwrap()
        .unwrap();
        let LoadState::Loaded(saved) = repository.load().unwrap() else {
            panic!("store not loaded")
        };
        assert!(saved.pending_browser_secret_cleanup.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cancelled_browser_secret_provision_is_reclaimed_without_account_metadata() {
        let (root, _paths, repository) = repository_fixture();
        let source_id = Uuid::new_v4();
        repository
            .try_mutate(|store| {
                reserve_browser_secret_provisioning(store, &[source_id])?;
                Ok::<_, String>(store.clone())
            })
            .unwrap()
            .unwrap();
        let secret = root.join("isolated-browser-secret");
        std::fs::write(&secret, b"fixture-only-secret").unwrap();

        let cleanup_ids = repository
            .try_mutate(|store| {
                Ok::<_, String>(resolve_browser_secret_provisioning(store, &[source_id]))
            })
            .unwrap()
            .unwrap();
        assert_eq!(cleanup_ids, vec![source_id]);
        retry_browser_secret_cleanup_with(&repository, &cleanup_ids, |_| {
            std::fs::remove_file(&secret).map_err(|_| "fixture deletion failed".into())
        })
        .unwrap()
        .unwrap();

        assert!(
            !secret.exists(),
            "a cancelled browser refresh left an unowned credential behind"
        );
        let LoadState::Loaded(saved) = repository.load().unwrap() else {
            panic!("store not loaded")
        };
        assert!(saved.pending_browser_secret_provisioning.is_empty());
        assert!(saved.pending_browser_secret_cleanup.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn browser_secret_provision_survives_metadata_commit_failure_until_cleanup_retries() {
        struct RejectProtection;
        impl SecretProtector for RejectProtection {
            fn protect_current_user(&self, _plain: &[u8]) -> io::Result<Vec<u8>> {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "injected protection failure",
                ))
            }

            fn unprotect_current_user(&self, encrypted: &[u8]) -> io::Result<Vec<u8>> {
                Ok(encrypted.to_vec())
            }
        }

        let (root, paths, repository) = repository_fixture();
        let source_id = Uuid::new_v4();
        let reserved = repository
            .try_mutate(|store| {
                reserve_browser_secret_provisioning(store, &[source_id])?;
                Ok::<_, String>(store.clone())
            })
            .unwrap()
            .unwrap();
        let before_failed_commit = std::fs::read(paths.kimi_accounts()).unwrap();
        let secret = root.join("isolated-browser-secret");
        std::fs::write(&secret, b"fixture-only-secret").unwrap();
        let mut updated = reserved.clone();
        updated.upsert_browser_source(
            identity("new-user", "new-global"),
            "New browser account".into(),
            KimiBrowserSourceRecord {
                source_id,
                browser: KimiBrowserKind::Edge,
                profile_name: "Default".into(),
                profile_path: root.join("browser-profile"),
            },
        );
        let mut failing_repository = repository.clone();
        failing_repository.protector = Arc::new(RejectProtection);

        assert!(
            failing_repository
                .try_mutate(|latest| {
                    apply_browser_scan_update(latest, &reserved, &updated, &[source_id])
                })
                .is_err(),
            "the fixture must fail while committing browser metadata"
        );
        assert_eq!(
            std::fs::read(paths.kimi_accounts()).unwrap(),
            before_failed_commit,
            "a failed metadata commit overwrote the durable provision record"
        );

        let cleanup_ids = repository
            .try_mutate(|store| {
                Ok::<_, String>(resolve_browser_secret_provisioning(store, &[source_id]))
            })
            .unwrap()
            .unwrap();
        retry_browser_secret_cleanup_with(&repository, &cleanup_ids, |_| {
            std::fs::remove_file(&secret).map_err(|_| "fixture deletion failed".into())
        })
        .unwrap()
        .unwrap();

        assert!(
            !secret.exists(),
            "metadata failure orphaned the newly saved browser credential"
        );
        let LoadState::Loaded(saved) = repository.load().unwrap() else {
            panic!("store not loaded")
        };
        assert!(saved.pending_browser_secret_provisioning.is_empty());
        assert!(saved.pending_browser_secret_cleanup.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn committed_browser_source_releases_provision_without_queuing_deletion() {
        let (root, _paths, repository) = repository_fixture();
        let source_id = Uuid::new_v4();
        let reserved = repository
            .try_mutate(|store| {
                reserve_browser_secret_provisioning(store, &[source_id])?;
                Ok::<_, String>(store.clone())
            })
            .unwrap()
            .unwrap();
        let mut updated = reserved.clone();
        updated.upsert_browser_source(
            identity("new-user", "new-global"),
            "New browser account".into(),
            KimiBrowserSourceRecord {
                source_id,
                browser: KimiBrowserKind::Chrome,
                profile_name: "Default".into(),
                profile_path: root.join("browser-profile"),
            },
        );

        let (committed, cleanup_ids) = repository
            .try_mutate(|latest| {
                apply_browser_scan_update(latest, &reserved, &updated, &[source_id])
            })
            .unwrap()
            .unwrap();

        assert!(cleanup_ids.is_empty());
        assert!(browser_source_is_live(&committed, source_id));
        assert!(committed.pending_browser_secret_provisioning.is_empty());
        assert!(committed.pending_browser_secret_cleanup.is_empty());
        let LoadState::Loaded(saved) = repository.load().unwrap() else {
            panic!("store not loaded")
        };
        assert_eq!(saved, committed);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn latest_scan_commits_when_an_older_scan_resolves_only_its_own_provisioning_marker() {
        let source_a = Uuid::new_v4();
        let source_b = Uuid::new_v4();
        let mut latest = KimiAccountStore::default();

        // Scan A has already persisted its external-credential reservation.
        reserve_browser_secret_provisioning(&mut latest, &[source_a]).unwrap();
        // Scan B starts from that state and persists its own reservation.
        reserve_browser_secret_provisioning(&mut latest, &[source_b]).unwrap();
        let reserved_for_b = latest.clone();

        // A is cancelled while B is still reading the browser. Its reservation
        // must move to cleanup without invalidating B's independent work.
        let cleanup_for_a = resolve_browser_secret_provisioning(&mut latest, &[source_a]);
        assert_eq!(cleanup_for_a, vec![source_a]);
        assert_eq!(latest.pending_browser_secret_cleanup, vec![source_a]);
        assert_eq!(latest.pending_browser_secret_provisioning, vec![source_b]);

        let mut updated_by_b = reserved_for_b.clone();
        updated_by_b.upsert_browser_source(
            identity("new-user", "new-global"),
            "New browser account".into(),
            KimiBrowserSourceRecord {
                source_id: source_b,
                browser: KimiBrowserKind::Edge,
                profile_name: "Default".into(),
                profile_path: PathBuf::from("fixture-profile"),
            },
        );

        let (committed, cleanup_for_b) = apply_browser_scan_update(
            &mut latest,
            &reserved_for_b,
            &updated_by_b,
            &[source_b],
        )
        .expect("an unrelated cancellation marker must not reject the newer scan");

        assert!(cleanup_for_b.is_empty());
        assert!(browser_source_is_live(&committed, source_b));
        assert_eq!(committed.pending_browser_secret_cleanup, vec![source_a]);
        assert!(committed.pending_browser_secret_provisioning.is_empty());
    }

    #[test]
    fn browser_scan_commit_keeps_a_newer_durable_quota_from_another_repository_handle() {
        let (root, _paths, repository) = repository_fixture();
        let account_id = Uuid::new_v4();
        repository
            .mutate(|store| {
                store.accounts.push(stored_account(
                    account_id,
                    "fixture-session",
                    "fixture-user",
                    "fixture-global",
                    "Fixture account",
                ));
            })
            .unwrap();

        let source_id = Uuid::new_v4();
        let reserved = repository
            .try_mutate(|store| {
                reserve_browser_secret_provisioning(store, &[source_id])?;
                Ok::<_, String>(store.clone())
            })
            .unwrap()
            .unwrap();
        let mut updated = reserved.clone();
        updated.upsert_browser_source(
            identity("fixture-user", "fixture-global"),
            "Fixture account".into(),
            KimiBrowserSourceRecord {
                source_id,
                browser: KimiBrowserKind::Chrome,
                profile_name: "Profile 1".into(),
                profile_path: root.join("browser-profile"),
            },
        );

        // A separate repository handle models another process finishing a
        // quota refresh while this browser scan is still using its baseline.
        let concurrent_writer = repository.clone();
        concurrent_writer
            .mutate(|store| {
                let account = store
                    .accounts
                    .iter_mut()
                    .find(|account| account.account_id == account_id)
                    .expect("fixture account exists");
                account.last_quota = Some(KimiStoredQuota {
                    used_percent: 66.6,
                    resets_at: Some("2026-10-01T00:00:00Z".into()),
                    updated_at: "2026-09-08T00:00:00Z".into(),
                });
            })
            .unwrap();

        let (committed, cleanup_ids) = repository
            .try_mutate(|latest| {
                apply_browser_scan_update(latest, &reserved, &updated, &[source_id])
            })
            .unwrap()
            .unwrap();

        assert!(cleanup_ids.is_empty());
        assert_eq!(
            committed
                .accounts
                .iter()
                .find(|account| account.account_id == account_id)
                .and_then(|account| account.last_quota.as_ref())
                .map(|quota| quota.used_percent),
            Some(66.6),
            "a browser scan must not roll back a quota refreshed after its baseline"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn account_store_rejects_a_live_source_with_an_unresolved_provision() {
        let source_id = Uuid::new_v4();
        let mut store = KimiAccountStore::default();
        store.upsert_browser_source(
            identity("user", "global"),
            "Browser account".into(),
            KimiBrowserSourceRecord {
                source_id,
                browser: KimiBrowserKind::Edge,
                profile_name: "Default".into(),
                profile_path: PathBuf::from("fixture-profile"),
            },
        );
        store.pending_browser_secret_provisioning.push(source_id);

        assert!(validate_kimi_account_store(&store, KIMI_ACCOUNT_STORE_VERSION).is_err());
    }

    #[test]
    fn repository_debug_output_contains_no_path_session_or_identity_fingerprint() {
        let (root, _paths, repository) = repository_fixture();
        let debug = format!("{repository:?}");
        assert!(!debug.contains(root.to_string_lossy().as_ref()));
        assert!(!debug.contains("session"));
        assert!(!debug.contains("fingerprint"));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn temporary_refresh_error_keeps_the_last_successful_quota() {
        let record = KimiAccountRecord {
            account_id: Uuid::new_v4(),
            webview_session_id: Some(Uuid::new_v4().to_string()),
            browser_sources: Vec::new(),
            identity: identity("user-a", "global-a"),
            display_name: "账号 A".into(),
            last_quota: None,
        };
        let previous = KimiAccountSnapshot {
            account_id: record.account_id,
            display_name: record.display_name.clone(),
            used_percent: Some(35.0),
            resets_at: Some("2026-09-01T00:00:00Z".into()),
            updated_at: Some("2026-08-14T00:00:00Z".into()),
            status: "ok",
            matched_credential_ids: vec![],
            source_labels: vec![],
        };

        let snapshot = snapshot_after_refresh_error(
            &record,
            Some(previous),
            &KimiAccountRefreshError::Temporary,
        );

        assert_eq!(snapshot.status, "stale");
        assert_eq!(snapshot.used_percent, Some(35.0));
        assert_eq!(snapshot.resets_at.as_deref(), Some("2026-09-01T00:00:00Z"));
    }

    #[test]
    fn authentication_error_hides_the_previous_quota() {
        let record = KimiAccountRecord {
            account_id: Uuid::new_v4(),
            webview_session_id: Some(Uuid::new_v4().to_string()),
            browser_sources: Vec::new(),
            identity: identity("user-a", "global-a"),
            display_name: "账号 A".into(),
            last_quota: None,
        };
        let previous = KimiAccountSnapshot {
            account_id: record.account_id,
            display_name: record.display_name.clone(),
            used_percent: Some(35.0),
            resets_at: None,
            updated_at: Some("2026-08-14T00:00:00Z".into()),
            status: "ok",
            matched_credential_ids: vec![],
            source_labels: vec![],
        };

        let snapshot = snapshot_after_refresh_error(
            &record,
            Some(previous),
            &KimiAccountRefreshError::Authentication,
        );

        assert_eq!(snapshot.status, "loginRequired");
        assert_eq!(snapshot.used_percent, None);
    }

    #[test]
    fn authentication_error_clears_persisted_quota_before_restart() {
        let mut record = KimiAccountRecord {
            account_id: Uuid::new_v4(),
            webview_session_id: Some(Uuid::new_v4().to_string()),
            browser_sources: Vec::new(),
            identity: identity("user-a", "global-a"),
            display_name: "账号 A".into(),
            last_quota: Some(KimiStoredQuota {
                used_percent: 35.0,
                resets_at: Some("2026-09-01T00:00:00Z".into()),
                updated_at: "2026-08-14T00:00:00Z".into(),
            }),
        };
        let snapshot = empty_snapshot(&record, "loginRequired");

        assert!(apply_snapshot_to_record(&mut record, &snapshot));
        assert!(record.last_quota.is_none());
        assert!(snapshot_from_stored_quota(&record).is_none());
    }

    #[test]
    fn account_revision_changes_when_an_account_is_removed() {
        let mut original = KimiAccountStore::default();
        original.upsert_authenticated(
            Uuid::new_v4().to_string(),
            identity("user-a", "global-a"),
            None,
        );
        let mut changed = original.clone();
        changed.accounts.clear();

        assert!(!same_account_revision(&original, &changed));
    }

    #[test]
    fn cancelled_async_stage_finishes_without_waiting_for_its_timeout() {
        tauri::async_runtime::block_on(async {
            let (cancel_tx, mut cancel_rx) = tokio::sync::watch::channel(false);
            let started = std::time::Instant::now();
            cancel_tx.send(true).unwrap();

            let result = wait_with_cancel(
                std::future::pending::<()>(),
                &mut cancel_rx,
                std::time::Duration::from_secs(10),
            )
            .await;

            assert_eq!(result, Err(CancellableWaitError::Cancelled));
            assert!(started.elapsed() < std::time::Duration::from_millis(500));
        });
    }

    #[test]
    fn async_stage_has_an_independent_hard_timeout() {
        tauri::async_runtime::block_on(async {
            let (_cancel_tx, mut cancel_rx) = tokio::sync::watch::channel(false);

            let result = wait_with_cancel(
                std::future::pending::<()>(),
                &mut cancel_rx,
                std::time::Duration::from_millis(20),
            )
            .await;

            assert_eq!(result, Err(CancellableWaitError::TimedOut));
        });
    }

    #[test]
    fn foreground_wait_returns_pending_without_cancelling_the_worker() {
        tauri::async_runtime::block_on(async {
            let (_outcome_tx, mut outcome_rx) =
                tokio::sync::watch::channel(None::<LoginTaskOutcome>);
            let started = std::time::Instant::now();

            let result =
                wait_for_login_outcome(&mut outcome_rx, std::time::Duration::from_millis(20)).await;

            assert!(result.is_none());
            assert!(started.elapsed() < std::time::Duration::from_millis(500));
        });
    }

    #[test]
    fn cancellation_only_wins_before_commit_starts() {
        let running = std::sync::atomic::AtomicU8::new(LoginTaskPhase::Running as u8);
        assert!(claim_login_cancellation(&running));
        assert_eq!(login_task_phase(&running), LoginTaskPhase::Cancelled);

        let committing = std::sync::atomic::AtomicU8::new(LoginTaskPhase::Committing as u8);
        assert!(!claim_login_cancellation(&committing));
        assert_eq!(login_task_phase(&committing), LoginTaskPhase::Committing);

        let completion = std::sync::atomic::AtomicU8::new(LoginTaskPhase::Running as u8);
        assert!(finish_login_phase(&completion));
        assert!(!claim_login_cancellation(&completion));
        assert_eq!(login_task_phase(&completion), LoginTaskPhase::Finished);

        let cancellation = std::sync::atomic::AtomicU8::new(LoginTaskPhase::Running as u8);
        assert!(claim_login_cancellation(&cancellation));
        assert!(!finish_login_phase(&cancellation));
        assert_eq!(login_task_phase(&cancellation), LoginTaskPhase::Cancelled);
    }

    #[test]
    fn forced_cancellation_terminates_a_stuck_worker_within_the_budget() {
        tauri::async_runtime::block_on(async {
            let (cancel, _cancel_rx) = tokio::sync::watch::channel(false);
            let (outcome_tx, outcome) = tokio::sync::watch::channel(None);
            let phase = Arc::new(AtomicU8::new(LoginTaskPhase::Cancelled as u8));
            let worker = tokio::spawn(std::future::pending::<()>());
            let abort = worker.abort_handle();
            let supervisor = tokio::spawn(async move {
                let _ = worker.await;
                let _ = outcome_tx.send(Some(LoginTaskOutcome::Cancelled));
            });
            let requested = RequestedLoginCancellation {
                active: None,
                task: Some(LoginTaskEntry {
                    generation: 1,
                    cancel,
                    outcome,
                    phase,
                    close_after_finish: Arc::new(AtomicBool::new(false)),
                    abort,
                }),
                accepted: true,
            };
            let started = std::time::Instant::now();

            let result = settle_requested_cancellation(&requested).await;

            assert!(matches!(result, Some(LoginTaskOutcome::Cancelled)));
            assert!(started.elapsed() < std::time::Duration::from_millis(600));
            supervisor.await.unwrap();
        });
    }

    #[test]
    fn missing_webview_is_a_temporary_refresh_failure() {
        assert_eq!(missing_webview_error(), KimiAccountRefreshError::Temporary);
    }

    #[test]
    fn matched_credentials_are_added_once() {
        let credential_id = Uuid::new_v4();
        let mut snapshot = empty_snapshot(
            &KimiAccountRecord {
                account_id: Uuid::new_v4(),
                webview_session_id: Some(Uuid::new_v4().to_string()),
                browser_sources: Vec::new(),
                identity: identity("user-a", "global-a"),
                display_name: "Account A".into(),
                last_quota: None,
            },
            "ok",
        );

        add_matched_credential(&mut snapshot, credential_id);
        add_matched_credential(&mut snapshot, credential_id);

        assert_eq!(snapshot.matched_credential_ids, vec![credential_id]);
    }

    #[test]
    fn stale_credential_match_results_are_not_publishable() {
        let mut original = KimiAccountStore::default();
        original.upsert_authenticated(
            Uuid::new_v4().to_string(),
            identity("user-a", "global-a"),
            None,
        );
        let mut removed = original.clone();
        removed.accounts.clear();

        assert!(!credential_match_publish_is_valid(
            4, 5, &original, &original, true,
        ));
        assert!(!credential_match_publish_is_valid(
            4, 4, &original, &removed, true,
        ));
        assert!(!credential_match_publish_is_valid(
            4, 4, &original, &original, false,
        ));
    }

    #[test]
    fn deleted_or_replaced_key_revisions_invalidate_match_results() {
        let credential_id = Uuid::new_v4();
        let original = HashMap::from([(credential_id, 1)]);
        let deleted = HashMap::new();
        let replaced = HashMap::from([(credential_id, 2)]);

        assert!(!same_credential_revisions(&original, &deleted));
        assert!(!same_credential_revisions(&original, &replaced));
        assert!(same_credential_revisions(&original, &original));
    }

    #[test]
    fn session_directory_waits_until_its_surface_is_destroyed() {
        let root = std::env::temp_dir().join(format!("codexbar-kimi-cleanup-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();

        assert!(!remove_session_directory_if_ready(&root, false).unwrap());
        assert!(root.exists());
        assert!(remove_session_directory_if_ready(&root, true).unwrap());
        assert!(!root.exists());
    }

    #[test]
    fn session_cleanup_never_deletes_without_a_durable_marker() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let (root, _paths, repository) = repository_fixture();
        let session_id = Uuid::new_v4().to_string();
        let session_path = root.join("isolated-session");
        std::fs::create_dir_all(&session_path).unwrap();
        let delete_called = AtomicBool::new(false);

        let result =
            retry_session_cleanup_with(&repository, &session_id, &session_path, true, |_| {
                delete_called.store(true, Ordering::SeqCst);
                Ok(())
            })
            .unwrap()
            .unwrap();

        assert_eq!(result, SessionCleanupDisposition::NoPendingMarker);
        assert!(!delete_called.load(Ordering::SeqCst));
        assert!(session_path.is_dir());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn session_cleanup_keeps_the_directory_until_the_surface_is_destroyed() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let (root, _paths, repository) = repository_fixture();
        let session_id = Uuid::new_v4().to_string();
        repository
            .mutate(|store| mark_session_cleanup(store, &session_id))
            .unwrap();
        let session_path = root.join("isolated-session");
        std::fs::create_dir_all(&session_path).unwrap();
        let delete_called = AtomicBool::new(false);

        let result =
            retry_session_cleanup_with(&repository, &session_id, &session_path, false, |_| {
                delete_called.store(true, Ordering::SeqCst);
                Ok(())
            })
            .unwrap()
            .unwrap();

        assert_eq!(result, SessionCleanupDisposition::Deferred);
        assert!(!delete_called.load(Ordering::SeqCst));
        assert!(session_path.is_dir());
        let LoadState::Loaded(saved) = repository.load().unwrap() else {
            panic!("store not loaded")
        };
        assert_eq!(saved.pending_session_cleanup, vec![session_id]);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn session_cleanup_live_session_clears_the_stale_marker_without_deleting() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let (root, _paths, repository) = repository_fixture();
        let session_id = Uuid::new_v4().to_string();
        repository
            .mutate(|store| {
                store.accounts.push(stored_account(
                    Uuid::new_v4(),
                    &session_id,
                    "user",
                    "global",
                    "Account",
                ));
                mark_session_cleanup(store, &session_id);
            })
            .unwrap();
        let session_path = root.join("isolated-session");
        std::fs::create_dir_all(&session_path).unwrap();
        let delete_called = AtomicBool::new(false);

        let result =
            retry_session_cleanup_with(&repository, &session_id, &session_path, true, |_| {
                delete_called.store(true, Ordering::SeqCst);
                Ok(())
            })
            .unwrap()
            .unwrap();

        assert_eq!(result, SessionCleanupDisposition::RetainedByLiveAccount);
        assert!(!delete_called.load(Ordering::SeqCst));
        assert!(session_path.is_dir());
        let LoadState::Loaded(saved) = repository.load().unwrap() else {
            panic!("store not loaded")
        };
        assert!(saved.pending_session_cleanup.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn session_cleanup_acknowledgement_failure_keeps_the_marker_for_idempotent_retry() {
        struct RejectProtection;
        impl SecretProtector for RejectProtection {
            fn protect_current_user(&self, _plain: &[u8]) -> io::Result<Vec<u8>> {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "injected protection failure",
                ))
            }

            fn unprotect_current_user(&self, encrypted: &[u8]) -> io::Result<Vec<u8>> {
                Ok(encrypted.to_vec())
            }
        }

        let (root, paths, repository) = repository_fixture();
        let session_id = Uuid::new_v4().to_string();
        repository
            .mutate(|store| mark_session_cleanup(store, &session_id))
            .unwrap();
        let before = std::fs::read(paths.kimi_accounts()).unwrap();
        let session_path = root.join("isolated-session");
        std::fs::create_dir_all(&session_path).unwrap();
        let mut failing_repository = repository.clone();
        failing_repository.protector = Arc::new(RejectProtection);

        assert!(
            retry_session_cleanup_with(
                &failing_repository,
                &session_id,
                &session_path,
                true,
                |path| remove_session_directory_if_ready(path, true).map(|_| ()),
            )
            .is_err()
        );
        assert!(!session_path.exists());
        assert_eq!(std::fs::read(paths.kimi_accounts()).unwrap(), before);

        retry_session_cleanup_with(&repository, &session_id, &session_path, true, |path| {
            remove_session_directory_if_ready(path, true).map(|_| ())
        })
        .unwrap()
        .unwrap();
        let LoadState::Loaded(saved) = repository.load().unwrap() else {
            panic!("store not loaded")
        };
        assert!(saved.pending_session_cleanup.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn background_webviews_share_one_lease_per_session_only() {
        let first_session = Uuid::new_v4().to_string();
        let second_session = Uuid::new_v4().to_string();

        let first = background_session_lease(&first_session);
        let same = background_session_lease(&first_session);
        let other = background_session_lease(&second_session);

        assert!(Arc::ptr_eq(&first.mutex, &same.mutex));
        assert!(!Arc::ptr_eq(&first.mutex, &other.mutex));

        drop(first);
        drop(same);
        drop(other);

        let leases = background_session_leases()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert!(!leases.contains_key(&first_session));
        assert!(!leases.contains_key(&second_session));
    }

    #[test]
    fn completion_events_target_only_local_settings_surfaces() {
        assert_eq!(login_completion_targets(), ["settings", "main"]);
    }

    #[test]
    fn credential_match_merge_does_not_overwrite_newer_quota_fields() {
        let account_id = Uuid::new_v4();
        let credential_id = Uuid::new_v4();
        let mut latest = vec![KimiAccountSnapshot {
            account_id,
            display_name: "账号 A".into(),
            used_percent: Some(62.0),
            resets_at: Some("new-reset".into()),
            updated_at: Some("new-update".into()),
            status: "ok",
            matched_credential_ids: vec![],
            source_labels: vec![],
        }];
        let matched = vec![KimiAccountSnapshot {
            account_id,
            display_name: "旧名称".into(),
            used_percent: Some(35.0),
            resets_at: Some("old-reset".into()),
            updated_at: Some("old-update".into()),
            status: "stale",
            matched_credential_ids: vec![credential_id],
            source_labels: vec![],
        }];

        merge_credential_matches(&mut latest, &matched);

        assert_eq!(latest[0].used_percent, Some(62.0));
        assert_eq!(latest[0].resets_at.as_deref(), Some("new-reset"));
        assert_eq!(latest[0].status, "ok");
        assert_eq!(latest[0].matched_credential_ids, vec![credential_id]);
    }

    #[test]
    fn startup_cleanup_selects_only_valid_unowned_session_directories() {
        let kept = Uuid::new_v4().to_string();
        let orphan = Uuid::new_v4().to_string();
        let selected = orphaned_session_ids(
            vec![kept.clone(), orphan.clone(), "not-a-session".into()],
            &[kept],
        );

        assert_eq!(selected, vec![orphan]);
    }
}
