use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};

use chrono::Utc;
use codexbar::browser::detection::{BrowserDetector, BrowserType, DetectedBrowser};
use codexbar::browser::local_storage::{
    LatestLocalStorageValue, LocalStorageReadError, read_latest_local_storage_values,
};
use codexbar::core::ProviderError;
use codexbar::core::credentials::{CredentialError, CredentialStore};
use codexbar::providers::kimi::{
    KimiAccountIdentity, KimiMonthlyQuota, KimiWebRequestContext, KimiWebTokenPair,
    fetch_web_account, parse_web_request_context, refresh_web_session,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{
    KimiAccountStore, KimiBrowserKind, KimiBrowserSourceRecord, KimiStoredQuota,
    is_current_account_refresh, wait_for_account_refresh_invalidation,
};

const KIMI_BROWSER_CREDENTIAL_SERVICE: &str = "com.codexbar.desktop.kimi-browser-session";
const BROWSER_SESSION_SECRET_VERSION: u32 = 1;
const KIMI_WEB_ORIGIN: &str = "https://www.kimi.com";
const KIMI_ACCESS_TOKEN_KEY: &str = "access_token";
const KIMI_REFRESH_TOKEN_KEY: &str = "refresh_token";
const KIMI_TOKEN_CONTEXT_KEY: &str = "volcano-token-info";

static BROWSER_SOURCE_LOCKS: OnceLock<Mutex<HashMap<Uuid, Arc<tokio::sync::Mutex<()>>>>> =
    OnceLock::new();

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KimiBrowserScanSummary {
    pub discovered: usize,
    pub refreshed: usize,
    pub failed: usize,
    #[serde(skip)]
    pub(super) refreshed_account_ids: HashSet<Uuid>,
    #[serde(skip)]
    pub(super) authentication_account_ids: HashSet<Uuid>,
}

#[derive(Clone, PartialEq, Eq)]
struct SupportedBrowserProfile {
    browser: KimiBrowserKind,
    profile_name: String,
    profile_path: PathBuf,
}

impl SupportedBrowserProfile {
    #[cfg(test)]
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

#[derive(Clone)]
struct BrowserTokens {
    access_token: String,
    refresh_token: String,
    context: KimiWebRequestContext,
}

type BrowserValueReadResult = Result<Option<BrowserTokens>, BrowserValueReadError>;
type AccountFetchFuture<'a> = Pin<
    Box<
        dyn Future<Output = Result<(KimiAccountIdentity, KimiMonthlyQuota), ProviderError>>
            + Send
            + 'a,
    >,
>;
type SessionRefreshFuture<'a> =
    Pin<Box<dyn Future<Output = Result<KimiWebTokenPair, ProviderError>> + Send + 'a>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BrowserValueReadError {
    Temporary,
}

trait BrowserValueReader: Send + Sync {
    fn read(&self, profile: &SupportedBrowserProfile) -> BrowserValueReadResult;
}

struct LocalStorageBrowserValueReader;

impl BrowserValueReader for LocalStorageBrowserValueReader {
    fn read(&self, profile: &SupportedBrowserProfile) -> BrowserValueReadResult {
        read_browser_values(profile).map_err(|_| BrowserValueReadError::Temporary)
    }
}

trait KimiBrowserClient: Send + Sync {
    fn fetch_account<'a>(&'a self, access_token: &'a str) -> AccountFetchFuture<'a>;

    fn refresh<'a>(
        &'a self,
        refresh_token: &'a str,
        context: &'a KimiWebRequestContext,
    ) -> SessionRefreshFuture<'a>;
}

struct ProductionKimiBrowserClient;

impl KimiBrowserClient for ProductionKimiBrowserClient {
    fn fetch_account<'a>(&'a self, access_token: &'a str) -> AccountFetchFuture<'a> {
        Box::pin(fetch_web_account(access_token))
    }

    fn refresh<'a>(
        &'a self,
        refresh_token: &'a str,
        context: &'a KimiWebRequestContext,
    ) -> SessionRefreshFuture<'a> {
        Box::pin(refresh_web_session(refresh_token, context))
    }
}

trait BrowserSecretStorage: Send + Sync {
    fn load_secret(&self, source_id: Uuid) -> Result<Option<BrowserSessionSecret>, String>;
    fn save_secret(&self, source_id: Uuid, secret: &BrowserSessionSecret) -> Result<(), String>;
    fn delete_secret(&self, source_id: Uuid) -> Result<(), String>;
}

impl<S: CredentialStore> BrowserSecretStorage for BrowserSessionSecretStore<S> {
    fn load_secret(&self, source_id: Uuid) -> Result<Option<BrowserSessionSecret>, String> {
        self.load(source_id)
    }

    fn save_secret(&self, source_id: Uuid, secret: &BrowserSessionSecret) -> Result<(), String> {
        self.save(source_id, secret)
    }

    fn delete_secret(&self, source_id: Uuid) -> Result<(), String> {
        self.delete(source_id)
    }
}

enum BrowserProfileRefreshStatus {
    Success {
        identity: KimiAccountIdentity,
        quota: KimiMonthlyQuota,
    },
    Authentication,
    NotAuthorized,
    Temporary,
    Cancelled,
}

struct BrowserProfileRefreshOutcome {
    source: KimiBrowserSourceRecord,
    was_existing: bool,
    profile_fingerprint: String,
    status: BrowserProfileRefreshStatus,
}

struct BrowserScanCandidate {
    profile: SupportedBrowserProfile,
    source: KimiBrowserSourceRecord,
    was_existing: bool,
}

struct SelectedRefreshToken {
    refresh_token: String,
    observed_browser_refresh_fingerprint: String,
}

struct RefreshStart {
    browser_tokens: BrowserTokens,
    selected: SelectedRefreshToken,
}

fn select_refresh_token(
    browser_refresh_token: &str,
    saved: Option<BrowserSessionSecret>,
) -> SelectedRefreshToken {
    let observed_browser_refresh_fingerprint = refresh_fingerprint(browser_refresh_token);
    let refresh_token = saved
        .filter(|secret| {
            secret.observed_browser_refresh_fingerprint == observed_browser_refresh_fingerprint
        })
        .map(|secret| secret.active_refresh_token)
        .unwrap_or_else(|| browser_refresh_token.to_string());
    SelectedRefreshToken {
        refresh_token,
        observed_browser_refresh_fingerprint,
    }
}

/// Browser storage is read synchronously and can outlive a cancelled refresh
/// generation. Keep the final generation check adjacent to the destructive
/// Credential Manager operation so an old scan cannot erase a still-live
/// credential after returning from `BrowserValueReader::read`.
fn delete_secret_if_current<S, F>(secrets: &S, source_id: Uuid, is_current: &F) -> bool
where
    S: BrowserSecretStorage,
    F: Fn() -> bool + ?Sized,
{
    if !is_current() {
        return false;
    }
    let _ = secrets.delete_secret(source_id);
    true
}

pub(super) fn browser_source_lock(source_id: Uuid) -> Arc<tokio::sync::Mutex<()>> {
    let mut locks = BROWSER_SOURCE_LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    Arc::clone(
        locks
            .entry(source_id)
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
    )
}

async fn refresh_profile_with<R, C, S, F>(
    reader: &R,
    client: &C,
    secrets: &S,
    profile: &SupportedBrowserProfile,
    source_id: Uuid,
    is_current: F,
) -> BrowserProfileRefreshStatus
where
    R: BrowserValueReader,
    C: KimiBrowserClient,
    S: BrowserSecretStorage,
    F: Fn() -> bool,
{
    if !is_current() {
        return BrowserProfileRefreshStatus::Cancelled;
    }
    let browser_tokens = match reader.read(profile) {
        Ok(Some(tokens)) => tokens,
        Ok(None) => {
            return if delete_secret_if_current(secrets, source_id, &is_current) {
                BrowserProfileRefreshStatus::NotAuthorized
            } else {
                BrowserProfileRefreshStatus::Cancelled
            };
        }
        Err(BrowserValueReadError::Temporary) => {
            return BrowserProfileRefreshStatus::Temporary;
        }
    };
    let saved = match secrets.load_secret(source_id) {
        Ok(saved) => saved,
        Err(_) => return BrowserProfileRefreshStatus::Temporary,
    };
    let browser_fingerprint = refresh_fingerprint(&browser_tokens.refresh_token);
    let browser_changed = saved
        .as_ref()
        .is_none_or(|secret| secret.observed_browser_refresh_fingerprint != browser_fingerprint);
    let selected = select_refresh_token(&browser_tokens.refresh_token, saved);

    if !is_current() {
        return BrowserProfileRefreshStatus::Cancelled;
    }
    match client.fetch_account(&browser_tokens.access_token).await {
        Ok((identity, quota)) => {
            if !is_current() {
                return BrowserProfileRefreshStatus::Cancelled;
            }
            if browser_changed
                && secrets
                    .save_secret(
                        source_id,
                        &BrowserSessionSecret {
                            version: BROWSER_SESSION_SECRET_VERSION,
                            observed_browser_refresh_fingerprint: selected
                                .observed_browser_refresh_fingerprint,
                            active_refresh_token: selected.refresh_token,
                        },
                    )
                    .is_err()
            {
                return BrowserProfileRefreshStatus::Temporary;
            }
            return BrowserProfileRefreshStatus::Success { identity, quota };
        }
        Err(ProviderError::AuthRequired) => {}
        Err(_) => return BrowserProfileRefreshStatus::Temporary,
    }

    refresh_after_authentication_failure(
        reader,
        client,
        secrets,
        profile,
        source_id,
        RefreshStart {
            browser_tokens,
            selected,
        },
        &is_current,
    )
    .await
}

async fn refresh_after_authentication_failure<R, C, S, F>(
    reader: &R,
    client: &C,
    secrets: &S,
    profile: &SupportedBrowserProfile,
    source_id: Uuid,
    start: RefreshStart,
    is_current: &F,
) -> BrowserProfileRefreshStatus
where
    R: BrowserValueReader,
    C: KimiBrowserClient,
    S: BrowserSecretStorage,
    F: Fn() -> bool,
{
    let mut context = start.browser_tokens.context;
    let mut selected = start.selected;
    let initial_browser_fingerprint = refresh_fingerprint(&start.browser_tokens.refresh_token);

    for attempt in 0..2 {
        if !is_current() {
            return BrowserProfileRefreshStatus::Cancelled;
        }
        let refresh_result = client.refresh(&selected.refresh_token, &context).await;
        if !is_current() {
            return BrowserProfileRefreshStatus::Cancelled;
        }
        let rotated = match refresh_result {
            Ok(tokens) => tokens,
            Err(ProviderError::AuthRequired) if attempt == 0 => {
                let reread_result = reader.read(profile);
                if !is_current() {
                    return BrowserProfileRefreshStatus::Cancelled;
                }
                let reread = match reread_result {
                    Ok(Some(tokens)) => tokens,
                    Ok(None) => {
                        return if delete_secret_if_current(secrets, source_id, is_current) {
                            BrowserProfileRefreshStatus::Authentication
                        } else {
                            BrowserProfileRefreshStatus::Cancelled
                        };
                    }
                    Err(BrowserValueReadError::Temporary) => {
                        return BrowserProfileRefreshStatus::Temporary;
                    }
                };
                let reread_fingerprint = refresh_fingerprint(&reread.refresh_token);
                if reread_fingerprint == initial_browser_fingerprint {
                    return if delete_secret_if_current(secrets, source_id, is_current) {
                        BrowserProfileRefreshStatus::Authentication
                    } else {
                        BrowserProfileRefreshStatus::Cancelled
                    };
                }
                context = reread.context;
                selected = SelectedRefreshToken {
                    refresh_token: reread.refresh_token,
                    observed_browser_refresh_fingerprint: reread_fingerprint,
                };
                continue;
            }
            Err(ProviderError::AuthRequired) => {
                return if delete_secret_if_current(secrets, source_id, is_current) {
                    BrowserProfileRefreshStatus::Authentication
                } else {
                    BrowserProfileRefreshStatus::Cancelled
                };
            }
            Err(_) => return BrowserProfileRefreshStatus::Temporary,
        };

        if !is_current() {
            return BrowserProfileRefreshStatus::Cancelled;
        }
        if secrets
            .save_secret(
                source_id,
                &BrowserSessionSecret {
                    version: BROWSER_SESSION_SECRET_VERSION,
                    observed_browser_refresh_fingerprint: selected
                        .observed_browser_refresh_fingerprint,
                    active_refresh_token: rotated.refresh_token,
                },
            )
            .is_err()
        {
            return BrowserProfileRefreshStatus::Temporary;
        }
        if !is_current() {
            return BrowserProfileRefreshStatus::Cancelled;
        }
        return match client.fetch_account(&rotated.access_token).await {
            Ok((identity, quota)) => BrowserProfileRefreshStatus::Success { identity, quota },
            Err(ProviderError::AuthRequired) => BrowserProfileRefreshStatus::Authentication,
            Err(_) => BrowserProfileRefreshStatus::Temporary,
        };
    }

    BrowserProfileRefreshStatus::Authentication
}

fn discover_supported_profiles(browsers: &[DetectedBrowser]) -> Vec<SupportedBrowserProfile> {
    browsers
        .iter()
        .filter_map(|browser| {
            let browser_kind = match browser.browser_type {
                BrowserType::Edge => KimiBrowserKind::Edge,
                BrowserType::Chrome => KimiBrowserKind::Chrome,
                _ => return None,
            };
            Some((browser_kind, &browser.profiles))
        })
        .flat_map(|(browser, profiles)| {
            profiles
                .iter()
                .filter(|profile| is_supported_profile_name(&profile.name))
                .map(move |profile| SupportedBrowserProfile {
                    browser,
                    profile_name: profile.name.clone(),
                    profile_path: profile.path.clone(),
                })
        })
        .collect()
}

fn is_supported_profile_name(name: &str) -> bool {
    name == "Default"
        || name.strip_prefix("Profile ").is_some_and(|suffix| {
            !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
        })
}

fn kimi_leveldb_path(profile: &SupportedBrowserProfile) -> PathBuf {
    profile.profile_path.join("Local Storage").join("leveldb")
}

fn authorized_tokens(
    access_token: Option<String>,
    refresh_token: Option<String>,
    raw_context: Option<String>,
) -> Option<BrowserTokens> {
    let access_token = access_token.map(|value| value.trim().to_string())?;
    let refresh_token = refresh_token.map(|value| value.trim().to_string())?;
    if access_token.is_empty() || refresh_token.is_empty() {
        return None;
    }
    Some(BrowserTokens {
        access_token,
        refresh_token,
        context: parse_web_request_context(raw_context.as_deref()),
    })
}

fn read_browser_values(
    profile: &SupportedBrowserProfile,
) -> Result<Option<BrowserTokens>, LocalStorageReadError> {
    let values = read_latest_local_storage_values(
        &kimi_leveldb_path(profile),
        KIMI_WEB_ORIGIN,
        &[
            KIMI_ACCESS_TOKEN_KEY,
            KIMI_REFRESH_TOKEN_KEY,
            KIMI_TOKEN_CONTEXT_KEY,
        ],
    )?;
    Ok(authorized_tokens(
        present_value(values.get(KIMI_ACCESS_TOKEN_KEY)),
        present_value(values.get(KIMI_REFRESH_TOKEN_KEY)),
        present_value(values.get(KIMI_TOKEN_CONTEXT_KEY)),
    ))
}

fn present_value(value: Option<&LatestLocalStorageValue>) -> Option<String> {
    match value {
        Some(LatestLocalStorageValue::Present(value)) => Some(value.clone()),
        Some(LatestLocalStorageValue::Deleted | LatestLocalStorageValue::Missing) | None => None,
    }
}

fn browser_profile_fingerprint(profile: &SupportedBrowserProfile) -> String {
    let normalized = normalized_profile_path(&profile.profile_path);
    let browser = match profile.browser {
        KimiBrowserKind::Edge => "edge",
        KimiBrowserKind::Chrome => "chrome",
    };
    refresh_fingerprint(&format!("{browser}:{normalized}"))
}

fn normalized_profile_path(path: &std::path::Path) -> String {
    std::fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .replace('/', "\\")
        .to_lowercase()
}

fn source_matches_profile(
    source: &KimiBrowserSourceRecord,
    profile: &SupportedBrowserProfile,
) -> bool {
    source.browser == profile.browser
        && normalized_profile_path(&source.profile_path)
            == normalized_profile_path(&profile.profile_path)
}

fn profile_from_source(source: &KimiBrowserSourceRecord) -> SupportedBrowserProfile {
    SupportedBrowserProfile {
        browser: source.browser,
        profile_name: source.profile_name.clone(),
        profile_path: source.profile_path.clone(),
    }
}

pub(super) fn suppress_browser_sources(
    store: &mut KimiAccountStore,
    record: &super::KimiAccountRecord,
) {
    for fingerprint in record
        .browser_sources
        .iter()
        .map(|source| browser_profile_fingerprint(&profile_from_source(source)))
    {
        if !store.suppressed_browser_profiles.contains(&fingerprint) {
            store.suppressed_browser_profiles.push(fingerprint);
        }
    }
}

#[cfg(windows)]
pub(super) fn delete_browser_source_secrets(source_ids: &[Uuid]) -> Result<(), String> {
    let secrets = production_secret_store();
    for source_id in source_ids {
        secrets.delete_secret(*source_id)?;
    }
    Ok(())
}

#[cfg(not(windows))]
pub(super) fn delete_browser_source_secrets(source_ids: &[Uuid]) -> Result<(), String> {
    if source_ids.is_empty() {
        Ok(())
    } else {
        Err("当前平台不支持安全保存 Kimi 浏览器登录凭据".into())
    }
}

#[cfg(windows)]
async fn refresh_production_profile(
    profile: SupportedBrowserProfile,
    source: KimiBrowserSourceRecord,
    was_existing: bool,
    generation: u64,
) -> BrowserProfileRefreshOutcome {
    let source_lock = browser_source_lock(source.source_id);
    let _guard = source_lock.lock().await;
    let status = refresh_profile_with(
        &LocalStorageBrowserValueReader,
        &ProductionKimiBrowserClient,
        &production_secret_store(),
        &profile,
        source.source_id,
        || is_current_account_refresh(generation),
    )
    .await;
    BrowserProfileRefreshOutcome {
        source,
        was_existing,
        profile_fingerprint: browser_profile_fingerprint(&profile),
        status,
    }
}

#[cfg(not(windows))]
async fn refresh_production_profile(
    profile: SupportedBrowserProfile,
    source: KimiBrowserSourceRecord,
    was_existing: bool,
    _generation: u64,
) -> BrowserProfileRefreshOutcome {
    BrowserProfileRefreshOutcome {
        source,
        was_existing,
        profile_fingerprint: browser_profile_fingerprint(&profile),
        status: BrowserProfileRefreshStatus::Temporary,
    }
}

pub(super) struct BrowserAccountScan {
    candidates: Vec<BrowserScanCandidate>,
    force_discovery: bool,
}

impl BrowserAccountScan {
    /// Discovers profiles once, so the UUID allocated for a new profile is the
    /// same UUID reserved by the account-store transaction before any external
    /// credential write can occur.
    pub(super) fn discover(store: &KimiAccountStore, force_discovery: bool) -> Self {
        let profiles = discover_supported_profiles(&BrowserDetector::detect_all());
        Self {
            candidates: build_scan_candidates(store, profiles, force_discovery),
            force_discovery,
        }
    }

    pub(super) fn provisional_source_ids(&self) -> Vec<Uuid> {
        // Non-Windows builds cannot persist or delete browser credentials at
        // all. Their scan stub never creates an external side effect, so it
        // must not leave an unfulfillable cleanup marker behind.
        if !cfg!(windows) {
            return Vec::new();
        }
        let mut seen = HashSet::new();
        self.candidates
            .iter()
            .filter_map(|candidate| {
                (!candidate.was_existing && seen.insert(candidate.source.source_id))
                    .then_some(candidate.source.source_id)
            })
            .collect()
    }

    pub(super) async fn refresh(
        self,
        store: &mut KimiAccountStore,
        generation: u64,
    ) -> KimiBrowserScanSummary {
        if !is_current_account_refresh(generation) {
            return KimiBrowserScanSummary::default();
        }
        let mut handles = tokio::task::JoinSet::new();
        for candidate in self.candidates {
            handles.spawn(refresh_production_profile(
                candidate.profile,
                candidate.source,
                candidate.was_existing,
                generation,
            ));
        }

        let mut summary = KimiBrowserScanSummary::default();
        loop {
            let next = tokio::select! {
                next = handles.join_next() => next,
                _ = wait_for_account_refresh_invalidation(generation) => {
                    // A detached task may save a credential after its caller
                    // has already converted the reservation into cleanup. Stop
                    // and join every task before returning that cancellation.
                    handles.abort_all();
                    while handles.join_next().await.is_some() {}
                    return KimiBrowserScanSummary::default();
                }
            };
            let Some(result) = next else {
                break;
            };
            let Ok(outcome) = result else {
                summary.failed += 1;
                continue;
            };
            if !is_current_account_refresh(generation) {
                handles.abort_all();
                while handles.join_next().await.is_some() {}
                return KimiBrowserScanSummary::default();
            }
            apply_profile_outcome(store, outcome, self.force_discovery, &mut summary);
        }
        summary
    }
}

fn build_scan_candidates(
    store: &KimiAccountStore,
    profiles: Vec<SupportedBrowserProfile>,
    force_discovery: bool,
) -> Vec<BrowserScanCandidate> {
    profiles
        .into_iter()
        .filter(|profile| {
            force_discovery
                || !store
                    .suppressed_browser_profiles
                    .contains(&browser_profile_fingerprint(profile))
        })
        .map(|profile| {
            let existing = store
                .accounts
                .iter()
                .flat_map(|account| &account.browser_sources)
                .find(|source| source_matches_profile(source, &profile))
                .cloned();
            let was_existing = existing.is_some();
            let source = existing.unwrap_or_else(|| KimiBrowserSourceRecord {
                source_id: Uuid::new_v4(),
                browser: profile.browser,
                profile_name: profile.profile_name.clone(),
                profile_path: profile.profile_path.clone(),
            });
            BrowserScanCandidate {
                profile,
                source,
                was_existing,
            }
        })
        .collect()
}

fn apply_profile_outcome(
    store: &mut KimiAccountStore,
    outcome: BrowserProfileRefreshOutcome,
    force_discovery: bool,
    summary: &mut KimiBrowserScanSummary,
) {
    match outcome.status {
        BrowserProfileRefreshStatus::Success { identity, quota } => {
            for account in &mut store.accounts {
                let before = account.browser_sources.len();
                account
                    .browser_sources
                    .retain(|source| source.source_id != outcome.source.source_id);
                if before != account.browser_sources.len()
                    && account.browser_sources.is_empty()
                    && account.webview_session_id.is_none()
                {
                    account.last_quota = None;
                }
            }
            let upsert = store.upsert_browser_source(
                identity.identity,
                identity
                    .display_name
                    .unwrap_or_else(|| outcome.source.safe_label()),
                outcome.source,
            );
            if let Some(account) = store
                .accounts
                .iter_mut()
                .find(|account| account.account_id == upsert.account_id)
            {
                account.last_quota = Some(KimiStoredQuota {
                    used_percent: quota.used_percent,
                    resets_at: quota.resets_at.map(|value| value.to_rfc3339()),
                    updated_at: Utc::now().to_rfc3339(),
                });
            }
            summary.refreshed_account_ids.insert(upsert.account_id);
            summary
                .authentication_account_ids
                .remove(&upsert.account_id);
            if force_discovery {
                store
                    .suppressed_browser_profiles
                    .retain(|fingerprint| fingerprint != &outcome.profile_fingerprint);
            }
            summary.discovered += usize::from(!outcome.was_existing);
            summary.refreshed += 1;
        }
        BrowserProfileRefreshStatus::Authentication
        | BrowserProfileRefreshStatus::NotAuthorized => {
            if outcome.was_existing {
                for account in &mut store.accounts {
                    if account
                        .browser_sources
                        .iter()
                        .any(|source| source.source_id == outcome.source.source_id)
                    {
                        account.last_quota = None;
                        if !summary.refreshed_account_ids.contains(&account.account_id) {
                            summary
                                .authentication_account_ids
                                .insert(account.account_id);
                        }
                    }
                }
                summary.failed += 1;
            }
        }
        BrowserProfileRefreshStatus::Temporary => {
            if outcome.was_existing {
                summary.failed += 1;
            }
        }
        BrowserProfileRefreshStatus::Cancelled => {}
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BrowserSessionSecret {
    pub(super) version: u32,
    pub(super) observed_browser_refresh_fingerprint: String,
    pub(super) active_refresh_token: String,
}

pub(super) struct BrowserSessionSecretStore<S> {
    backend: S,
}

impl<S: CredentialStore> BrowserSessionSecretStore<S> {
    pub(super) fn new(backend: S) -> Self {
        Self { backend }
    }

    pub(super) fn load(&self, source_id: Uuid) -> Result<Option<BrowserSessionSecret>, String> {
        let raw = match self
            .backend
            .get(KIMI_BROWSER_CREDENTIAL_SERVICE, &source_id.to_string())
        {
            Ok(raw) => raw,
            Err(CredentialError::NotFound) => return Ok(None),
            Err(error) => return Err(storage_error("load", &error)),
        };
        let secret = serde_json::from_str::<BrowserSessionSecret>(&raw)
            .map_err(|_| "Kimi 浏览器登录凭据格式无效".to_string())?;
        validate_secret(&secret)?;
        Ok(Some(secret))
    }

    pub(super) fn save(
        &self,
        source_id: Uuid,
        secret: &BrowserSessionSecret,
    ) -> Result<(), String> {
        validate_secret(secret)?;
        let raw =
            serde_json::to_string(secret).map_err(|_| "Kimi 浏览器登录凭据格式无效".to_string())?;
        self.backend
            .set(
                KIMI_BROWSER_CREDENTIAL_SERVICE,
                &source_id.to_string(),
                &raw,
            )
            .map_err(|error| storage_error("save", &error))
    }

    pub(super) fn delete(&self, source_id: Uuid) -> Result<(), String> {
        match self
            .backend
            .delete(KIMI_BROWSER_CREDENTIAL_SERVICE, &source_id.to_string())
        {
            Ok(()) | Err(CredentialError::NotFound) => Ok(()),
            Err(error) => Err(storage_error("delete", &error)),
        }
    }
}

fn validate_secret(secret: &BrowserSessionSecret) -> Result<(), String> {
    if secret.version != BROWSER_SESSION_SECRET_VERSION
        || secret
            .observed_browser_refresh_fingerprint
            .trim()
            .is_empty()
        || secret.active_refresh_token.trim().is_empty()
    {
        return Err("Kimi 浏览器登录凭据格式无效".to_string());
    }
    Ok(())
}

fn storage_error(operation: &'static str, error: &CredentialError) -> String {
    let error_kind = match error {
        CredentialError::NotFound => "not_found",
        CredentialError::AccessDenied => "access_denied",
        CredentialError::Storage(_) => "storage",
        CredentialError::InvalidFormat => "invalid_format",
    };
    tracing::warn!(
        operation,
        error_kind,
        "Kimi browser credential operation failed"
    );
    "无法安全访问 Kimi 浏览器登录凭据".to_string()
}

pub(super) fn refresh_fingerprint(refresh_token: &str) -> String {
    codexbar::providers::kimi::web_refresh_token_fingerprint(refresh_token)
}

#[cfg(windows)]
pub(super) fn production_secret_store()
-> BrowserSessionSecretStore<codexbar::core::credentials::WindowsCredentialStore> {
    BrowserSessionSecretStore::new(codexbar::core::credentials::WindowsCredentialStore::new())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use codexbar::browser::detection::{BrowserProfile, BrowserType, DetectedBrowser};
    use codexbar::core::credentials::{CredentialError, CredentialStore};
    use uuid::Uuid;

    use super::*;
    use crate::kimi_accounts::{
        KimiAccountStore, KimiBrowserKind, KimiBrowserSourceRecord, KimiIdentityFingerprint,
    };

    type FakeBrowserReadQueue = Arc<Mutex<VecDeque<BrowserValueReadResult>>>;

    #[derive(Clone, Default)]
    struct MemoryCredentialStore {
        values: Arc<Mutex<HashMap<(String, String), String>>>,
    }

    impl MemoryCredentialStore {
        fn keys(&self) -> Vec<(String, String)> {
            let mut keys = self
                .values
                .lock()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>();
            keys.sort();
            keys
        }
    }

    impl CredentialStore for MemoryCredentialStore {
        fn get(&self, service: &str, key: &str) -> Result<String, CredentialError> {
            self.values
                .lock()
                .unwrap()
                .get(&(service.to_string(), key.to_string()))
                .cloned()
                .ok_or(CredentialError::NotFound)
        }

        fn set(&self, service: &str, key: &str, value: &str) -> Result<(), CredentialError> {
            self.values
                .lock()
                .unwrap()
                .insert((service.to_string(), key.to_string()), value.to_string());
            Ok(())
        }

        fn delete(&self, service: &str, key: &str) -> Result<(), CredentialError> {
            self.values
                .lock()
                .unwrap()
                .remove(&(service.to_string(), key.to_string()))
                .map(|_| ())
                .ok_or(CredentialError::NotFound)
        }
    }

    fn identity(user: &str, global: &str) -> KimiIdentityFingerprint {
        KimiIdentityFingerprint {
            user_id_fingerprint: user.to_string(),
            global_id_fingerprint: global.to_string(),
        }
    }

    fn browser_source(
        source_id: Uuid,
        browser: KimiBrowserKind,
        profile_name: &str,
    ) -> KimiBrowserSourceRecord {
        KimiBrowserSourceRecord {
            source_id,
            browser,
            profile_name: profile_name.to_string(),
            profile_path: PathBuf::from(format!(r"C:\Browser\{profile_name}")),
        }
    }

    #[derive(Clone, Default)]
    struct FakeBrowserValueReader {
        reads: FakeBrowserReadQueue,
    }

    impl FakeBrowserValueReader {
        fn with_reads(reads: Vec<Option<BrowserTokens>>) -> Self {
            Self {
                reads: Arc::new(Mutex::new(
                    reads.into_iter().map(Ok).collect::<VecDeque<_>>(),
                )),
            }
        }
    }

    impl BrowserValueReader for FakeBrowserValueReader {
        fn read(&self, _profile: &SupportedBrowserProfile) -> BrowserValueReadResult {
            let mut reads = self.reads.lock().unwrap();
            if reads.len() > 1 {
                reads.pop_front().unwrap()
            } else {
                reads.front().cloned().unwrap_or(Ok(None))
            }
        }
    }

    enum FakeFetchResponse {
        Account(KimiAccountIdentity, KimiMonthlyQuota),
        Authentication,
        Temporary,
    }

    enum FakeRefreshResponse {
        Success(KimiWebTokenPair),
        Authentication,
        Temporary,
    }

    #[derive(Clone, Default)]
    struct FakeKimiBrowserClient {
        fetches: Arc<Mutex<VecDeque<FakeFetchResponse>>>,
        refreshes: Arc<Mutex<VecDeque<FakeRefreshResponse>>>,
        refresh_request_tokens: Arc<Mutex<Vec<String>>>,
        operations: Arc<Mutex<Vec<&'static str>>>,
    }

    impl FakeKimiBrowserClient {
        fn queue_fetch(&self, response: FakeFetchResponse) {
            self.fetches.lock().unwrap().push_back(response);
        }

        fn queue_refresh(&self, response: FakeRefreshResponse) {
            self.refreshes.lock().unwrap().push_back(response);
        }

        fn refresh_request_tokens(&self) -> Vec<String> {
            self.refresh_request_tokens.lock().unwrap().clone()
        }
    }

    impl KimiBrowserClient for FakeKimiBrowserClient {
        fn fetch_account<'a>(&'a self, _access_token: &'a str) -> AccountFetchFuture<'a> {
            let response = self.fetches.lock().unwrap().pop_front();
            let operations = Arc::clone(&self.operations);
            Box::pin(async move {
                match response.unwrap_or(FakeFetchResponse::Temporary) {
                    FakeFetchResponse::Account(identity, quota) => {
                        operations.lock().unwrap().push("fetch-account");
                        Ok((identity, quota))
                    }
                    FakeFetchResponse::Authentication => Err(ProviderError::AuthRequired),
                    FakeFetchResponse::Temporary => {
                        Err(ProviderError::Other("test temporary failure".into()))
                    }
                }
            })
        }

        fn refresh<'a>(
            &'a self,
            refresh_token: &'a str,
            _context: &'a KimiWebRequestContext,
        ) -> SessionRefreshFuture<'a> {
            self.refresh_request_tokens
                .lock()
                .unwrap()
                .push(refresh_token.to_string());
            self.operations.lock().unwrap().push("refresh");
            let response = self.refreshes.lock().unwrap().pop_front();
            Box::pin(async move {
                match response.unwrap_or(FakeRefreshResponse::Temporary) {
                    FakeRefreshResponse::Success(tokens) => Ok(tokens),
                    FakeRefreshResponse::Authentication => Err(ProviderError::AuthRequired),
                    FakeRefreshResponse::Temporary => {
                        Err(ProviderError::Other("test temporary failure".into()))
                    }
                }
            })
        }
    }

    #[derive(Clone, Default)]
    struct FakeSecretStorage {
        value: Arc<Mutex<Option<BrowserSessionSecret>>>,
        operations: Arc<Mutex<Vec<&'static str>>>,
    }

    impl FakeSecretStorage {
        fn with_secret(secret: BrowserSessionSecret) -> Self {
            Self {
                value: Arc::new(Mutex::new(Some(secret))),
                operations: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn secret(&self) -> Option<BrowserSessionSecret> {
            self.value.lock().unwrap().clone()
        }
    }

    impl BrowserSecretStorage for FakeSecretStorage {
        fn load_secret(&self, _source_id: Uuid) -> Result<Option<BrowserSessionSecret>, String> {
            Ok(self.value.lock().unwrap().clone())
        }

        fn save_secret(
            &self,
            _source_id: Uuid,
            secret: &BrowserSessionSecret,
        ) -> Result<(), String> {
            self.operations.lock().unwrap().push("save-secret");
            *self.value.lock().unwrap() = Some(secret.clone());
            Ok(())
        }

        fn delete_secret(&self, _source_id: Uuid) -> Result<(), String> {
            self.operations.lock().unwrap().push("delete-secret");
            *self.value.lock().unwrap() = None;
            Ok(())
        }
    }

    fn tokens(access_token: &str, refresh_token: &str) -> BrowserTokens {
        BrowserTokens {
            access_token: access_token.into(),
            refresh_token: refresh_token.into(),
            context: KimiWebRequestContext::default(),
        }
    }

    fn account(user: &str, used_percent: f64) -> FakeFetchResponse {
        FakeFetchResponse::Account(
            KimiAccountIdentity {
                identity: identity(user, &format!("global-{user}")),
                display_name: Some(user.to_string()),
            },
            KimiMonthlyQuota {
                used_percent,
                resets_at: None,
            },
        )
    }

    fn supported_profile(browser: KimiBrowserKind, profile_name: &str) -> SupportedBrowserProfile {
        SupportedBrowserProfile {
            browser,
            profile_name: profile_name.to_string(),
            profile_path: PathBuf::from(format!(r"C:\Browsers\{profile_name}")),
        }
    }

    fn detected_browser(browser_type: BrowserType, profile_names: &[&str]) -> DetectedBrowser {
        let user_data_dir = PathBuf::from(format!(r"C:\Browsers\{}", browser_type.display_name()));
        DetectedBrowser {
            browser_type,
            profiles: profile_names
                .iter()
                .map(|name| BrowserProfile {
                    name: (*name).to_string(),
                    path: user_data_dir.join(name),
                    is_default: *name == "Default",
                })
                .collect(),
            user_data_dir,
        }
    }

    #[test]
    fn discovery_accepts_only_edge_and_chrome_default_or_profile_n() {
        let browsers = vec![
            detected_browser(BrowserType::Edge, &["Default", "Guest Profile"]),
            detected_browser(BrowserType::Chrome, &["Profile 1", "System Profile"]),
            detected_browser(BrowserType::Brave, &["Default"]),
            detected_browser(BrowserType::Firefox, &["abc.default-release"]),
        ];

        let found = discover_supported_profiles(&browsers);
        let labels = found
            .iter()
            .map(SupportedBrowserProfile::safe_label)
            .collect::<Vec<_>>();

        assert_eq!(
            labels,
            vec!["Microsoft Edge · Default", "Google Chrome · Profile 1"]
        );
    }

    #[test]
    fn discovery_uses_exact_local_storage_directory() {
        let browser = detected_browser(BrowserType::Chrome, &["Profile 1"]);
        let profile = discover_supported_profiles(&[browser]).remove(0);

        assert_eq!(
            kimi_leveldb_path(&profile),
            profile.profile_path.join("Local Storage").join("leveldb")
        );
    }

    #[test]
    fn profiles_without_both_access_and_refresh_tokens_are_not_authorized() {
        assert!(authorized_tokens(Some("access".into()), None, None).is_none());
        assert!(authorized_tokens(None, Some("refresh".into()), None).is_none());
        assert!(authorized_tokens(Some("access".into()), Some("refresh".into()), None).is_some());
    }

    #[test]
    fn unchanged_browser_selects_persisted_rotated_refresh_token() {
        let saved = BrowserSessionSecret {
            version: 1,
            observed_browser_refresh_fingerprint: refresh_fingerprint("browser-refresh"),
            active_refresh_token: "app-rotated-refresh".into(),
        };

        let selected = select_refresh_token("browser-refresh", Some(saved));

        assert_eq!(selected.refresh_token, "app-rotated-refresh");
        assert_eq!(
            selected.observed_browser_refresh_fingerprint,
            refresh_fingerprint("browser-refresh")
        );
    }

    #[test]
    fn changed_browser_token_selects_a_new_rotation_start() {
        let saved = BrowserSessionSecret {
            version: 1,
            observed_browser_refresh_fingerprint: refresh_fingerprint("browser-old-refresh"),
            active_refresh_token: "app-old-refresh".into(),
        };

        let selected = select_refresh_token("browser-new-refresh", Some(saved));

        assert_eq!(selected.refresh_token, "browser-new-refresh");
        assert_eq!(
            selected.observed_browser_refresh_fingerprint,
            refresh_fingerprint("browser-new-refresh")
        );
    }

    #[tokio::test]
    async fn unchanged_browser_uses_persisted_rotated_refresh_token() {
        let source_id = Uuid::new_v4();
        let profile = supported_profile(KimiBrowserKind::Edge, "Default");
        let reader = FakeBrowserValueReader::with_reads(vec![Some(tokens(
            "expired-access",
            "browser-refresh",
        ))]);
        let client = FakeKimiBrowserClient::default();
        client.queue_fetch(FakeFetchResponse::Authentication);
        client.queue_refresh(FakeRefreshResponse::Success(KimiWebTokenPair {
            access_token: "new-access".into(),
            refresh_token: "next-refresh".into(),
        }));
        client.queue_fetch(account("edge-user", 12.5));
        let secrets = FakeSecretStorage::with_secret(BrowserSessionSecret {
            version: 1,
            observed_browser_refresh_fingerprint: refresh_fingerprint("browser-refresh"),
            active_refresh_token: "app-rotated-refresh".into(),
        });

        let status =
            refresh_profile_with(&reader, &client, &secrets, &profile, source_id, || true).await;

        assert!(matches!(
            status,
            BrowserProfileRefreshStatus::Success { quota, .. }
                if (quota.used_percent - 12.5).abs() < f64::EPSILON
        ));
        assert_eq!(client.refresh_request_tokens(), vec!["app-rotated-refresh"]);
        assert_eq!(
            secrets.secret().unwrap().active_refresh_token,
            "next-refresh"
        );
    }

    #[tokio::test]
    async fn changed_browser_token_replaces_the_saved_rotation_start() {
        let source_id = Uuid::new_v4();
        let profile = supported_profile(KimiBrowserKind::Edge, "Default");
        let reader = FakeBrowserValueReader::with_reads(vec![Some(tokens(
            "expired-access",
            "browser-new-refresh",
        ))]);
        let client = FakeKimiBrowserClient::default();
        client.queue_fetch(FakeFetchResponse::Authentication);
        client.queue_refresh(FakeRefreshResponse::Success(KimiWebTokenPair {
            access_token: "new-access".into(),
            refresh_token: "next-refresh".into(),
        }));
        client.queue_fetch(account("edge-user", 12.5));
        let secrets = FakeSecretStorage::with_secret(BrowserSessionSecret {
            version: 1,
            observed_browser_refresh_fingerprint: refresh_fingerprint("browser-old-refresh"),
            active_refresh_token: "app-old-refresh".into(),
        });

        let status =
            refresh_profile_with(&reader, &client, &secrets, &profile, source_id, || true).await;

        assert!(matches!(
            status,
            BrowserProfileRefreshStatus::Success { .. }
        ));
        assert_eq!(client.refresh_request_tokens(), vec!["browser-new-refresh"]);
        assert_eq!(
            secrets
                .secret()
                .unwrap()
                .observed_browser_refresh_fingerprint,
            refresh_fingerprint("browser-new-refresh")
        );
    }

    #[tokio::test]
    async fn refresh_rotation_is_saved_before_quota_is_requested() {
        let profile = supported_profile(KimiBrowserKind::Edge, "Default");
        let reader = FakeBrowserValueReader::with_reads(vec![Some(tokens(
            "expired-access",
            "browser-refresh",
        ))]);
        let client = FakeKimiBrowserClient::default();
        client.queue_fetch(FakeFetchResponse::Authentication);
        client.queue_refresh(FakeRefreshResponse::Success(KimiWebTokenPair {
            access_token: "new-access".into(),
            refresh_token: "next-refresh".into(),
        }));
        client.queue_fetch(account("edge-user", 12.5));
        let secrets = FakeSecretStorage {
            operations: Arc::clone(&client.operations),
            ..FakeSecretStorage::default()
        };

        let status =
            refresh_profile_with(&reader, &client, &secrets, &profile, Uuid::new_v4(), || {
                true
            })
            .await;

        assert!(matches!(
            status,
            BrowserProfileRefreshStatus::Success { .. }
        ));
        assert_eq!(
            *client.operations.lock().unwrap(),
            vec!["refresh", "save-secret", "fetch-account"]
        );
    }

    #[tokio::test]
    async fn cancellation_after_rotation_secret_write_keeps_the_rotated_credential() {
        use std::sync::atomic::{AtomicBool, Ordering};

        struct CancelOnSave {
            inner: FakeSecretStorage,
            current: AtomicBool,
        }

        impl BrowserSecretStorage for CancelOnSave {
            fn load_secret(&self, source_id: Uuid) -> Result<Option<BrowserSessionSecret>, String> {
                self.inner.load_secret(source_id)
            }

            fn save_secret(
                &self,
                source_id: Uuid,
                secret: &BrowserSessionSecret,
            ) -> Result<(), String> {
                self.inner.save_secret(source_id, secret)?;
                self.current.store(false, Ordering::SeqCst);
                Ok(())
            }

            fn delete_secret(&self, source_id: Uuid) -> Result<(), String> {
                self.inner.delete_secret(source_id)
            }
        }

        let source_id = Uuid::new_v4();
        let profile = supported_profile(KimiBrowserKind::Edge, "Default");
        let reader = FakeBrowserValueReader::with_reads(vec![Some(tokens(
            "expired-access",
            "browser-refresh",
        ))]);
        let client = FakeKimiBrowserClient::default();
        client.queue_fetch(FakeFetchResponse::Authentication);
        client.queue_refresh(FakeRefreshResponse::Success(KimiWebTokenPair {
            access_token: "rotated-access".into(),
            refresh_token: "rotated-refresh".into(),
        }));
        let secrets = CancelOnSave {
            inner: FakeSecretStorage::with_secret(BrowserSessionSecret {
                version: BROWSER_SESSION_SECRET_VERSION,
                observed_browser_refresh_fingerprint: refresh_fingerprint("browser-refresh"),
                active_refresh_token: "previous-rotated-refresh".into(),
            }),
            current: AtomicBool::new(true),
        };

        let status = refresh_profile_with(&reader, &client, &secrets, &profile, source_id, || {
            secrets.current.load(Ordering::SeqCst)
        })
        .await;

        assert!(matches!(status, BrowserProfileRefreshStatus::Cancelled));
        assert_eq!(
            secrets.inner.secret().unwrap().active_refresh_token,
            "rotated-refresh",
            "cancellation after saving must not discard the usable rotated credential"
        );
        assert!(
            !secrets
                .inner
                .operations
                .lock()
                .unwrap()
                .contains(&"delete-secret"),
            "the refresh path must not delete a credential merely because its caller cancelled"
        );
    }

    #[tokio::test]
    async fn refresh_rejection_retries_once_only_when_the_browser_token_changed() {
        let profile = supported_profile(KimiBrowserKind::Chrome, "Profile 1");
        let reader = FakeBrowserValueReader::with_reads(vec![
            Some(tokens("expired-access", "browser-old-refresh")),
            Some(tokens("newer-expired-access", "browser-new-refresh")),
        ]);
        let client = FakeKimiBrowserClient::default();
        client.queue_fetch(FakeFetchResponse::Authentication);
        client.queue_refresh(FakeRefreshResponse::Authentication);
        client.queue_refresh(FakeRefreshResponse::Success(KimiWebTokenPair {
            access_token: "new-access".into(),
            refresh_token: "next-refresh".into(),
        }));
        client.queue_fetch(account("chrome-user", 26.75));
        let secrets = FakeSecretStorage::default();

        let status =
            refresh_profile_with(&reader, &client, &secrets, &profile, Uuid::new_v4(), || {
                true
            })
            .await;

        assert!(matches!(
            status,
            BrowserProfileRefreshStatus::Success { .. }
        ));
        assert_eq!(
            client.refresh_request_tokens(),
            vec!["browser-old-refresh", "browser-new-refresh"]
        );
    }

    #[tokio::test]
    async fn browser_logout_deletes_secret_and_never_keeps_old_quota_current() {
        let source_id = Uuid::new_v4();
        let profile = supported_profile(KimiBrowserKind::Edge, "Default");
        let reader = FakeBrowserValueReader::with_reads(vec![None]);
        let client = FakeKimiBrowserClient::default();
        let secrets = FakeSecretStorage::with_secret(BrowserSessionSecret {
            version: 1,
            observed_browser_refresh_fingerprint: refresh_fingerprint("browser-refresh"),
            active_refresh_token: "active-refresh".into(),
        });
        let source = browser_source(source_id, KimiBrowserKind::Edge, "Default");
        let mut store = KimiAccountStore::default();
        let account_id = store
            .upsert_browser_source(
                identity("old-user", "global-old-user"),
                "old-user".into(),
                source.clone(),
            )
            .account_id;
        store.accounts[0].last_quota = Some(KimiStoredQuota {
            used_percent: 18.0,
            resets_at: None,
            updated_at: "2026-08-23T00:00:00Z".into(),
        });

        let status =
            refresh_profile_with(&reader, &client, &secrets, &profile, source_id, || true).await;
        let mut summary = KimiBrowserScanSummary::default();
        apply_profile_outcome(
            &mut store,
            BrowserProfileRefreshOutcome {
                source,
                was_existing: true,
                profile_fingerprint: browser_profile_fingerprint(&profile),
                status,
            },
            false,
            &mut summary,
        );

        assert!(secrets.secret().is_none());
        assert!(
            store
                .accounts
                .iter()
                .find(|account| account.account_id == account_id)
                .unwrap()
                .last_quota
                .is_none()
        );
        assert_eq!(summary.failed, 1);
        assert!(client.refresh_request_tokens().is_empty());
    }

    #[tokio::test]
    async fn cancellation_during_browser_read_never_deletes_an_existing_credential() {
        use std::sync::atomic::{AtomicBool, Ordering};

        struct CancelOnRead {
            current: Arc<AtomicBool>,
        }

        impl BrowserValueReader for CancelOnRead {
            fn read(&self, _profile: &SupportedBrowserProfile) -> BrowserValueReadResult {
                self.current.store(false, Ordering::SeqCst);
                Ok(None)
            }
        }

        let source_id = Uuid::new_v4();
        let profile = supported_profile(KimiBrowserKind::Edge, "Default");
        let current = Arc::new(AtomicBool::new(true));
        let reader = CancelOnRead {
            current: Arc::clone(&current),
        };
        let client = FakeKimiBrowserClient::default();
        let secrets = FakeSecretStorage::with_secret(BrowserSessionSecret {
            version: BROWSER_SESSION_SECRET_VERSION,
            observed_browser_refresh_fingerprint: refresh_fingerprint("browser-refresh"),
            active_refresh_token: "saved-refresh".into(),
        });

        let status = refresh_profile_with(&reader, &client, &secrets, &profile, source_id, || {
            current.load(Ordering::SeqCst)
        })
        .await;

        assert!(matches!(status, BrowserProfileRefreshStatus::Cancelled));
        assert_eq!(
            secrets.secret().unwrap().active_refresh_token,
            "saved-refresh",
            "an invalidated scan must not remove a credential after synchronous browser I/O"
        );
        assert!(
            !secrets
                .operations
                .lock()
                .unwrap()
                .contains(&"delete-secret"),
            "the cancellation check must occur before every deletion"
        );
    }

    #[tokio::test]
    async fn cancellation_during_auth_retry_reread_never_deletes_an_existing_credential() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

        struct CancelOnSecondRead {
            current: Arc<AtomicBool>,
            reads: AtomicUsize,
        }

        impl BrowserValueReader for CancelOnSecondRead {
            fn read(&self, _profile: &SupportedBrowserProfile) -> BrowserValueReadResult {
                if self.reads.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Ok(Some(tokens("expired-access", "browser-refresh")));
                }
                self.current.store(false, Ordering::SeqCst);
                Ok(Some(tokens("expired-access", "browser-refresh")))
            }
        }

        let source_id = Uuid::new_v4();
        let profile = supported_profile(KimiBrowserKind::Chrome, "Default");
        let current = Arc::new(AtomicBool::new(true));
        let reader = CancelOnSecondRead {
            current: Arc::clone(&current),
            reads: AtomicUsize::new(0),
        };
        let client = FakeKimiBrowserClient::default();
        client.queue_fetch(FakeFetchResponse::Authentication);
        client.queue_refresh(FakeRefreshResponse::Authentication);
        let secrets = FakeSecretStorage::with_secret(BrowserSessionSecret {
            version: BROWSER_SESSION_SECRET_VERSION,
            observed_browser_refresh_fingerprint: refresh_fingerprint("browser-refresh"),
            active_refresh_token: "saved-refresh".into(),
        });

        let status = refresh_profile_with(&reader, &client, &secrets, &profile, source_id, || {
            current.load(Ordering::SeqCst)
        })
        .await;

        assert!(matches!(status, BrowserProfileRefreshStatus::Cancelled));
        assert_eq!(
            secrets.secret().unwrap().active_refresh_token,
            "saved-refresh",
            "the retry reread must not delete a valid credential after cancellation"
        );
        assert!(
            !secrets
                .operations
                .lock()
                .unwrap()
                .contains(&"delete-secret")
        );
    }

    #[tokio::test]
    async fn cancelled_authentication_response_does_not_delete_saved_credential() {
        use std::sync::atomic::{AtomicBool, Ordering};

        struct CancelOnRefresh {
            inner: FakeKimiBrowserClient,
            current: AtomicBool,
        }
        impl KimiBrowserClient for CancelOnRefresh {
            fn fetch_account<'a>(&'a self, access_token: &'a str) -> AccountFetchFuture<'a> {
                self.inner.fetch_account(access_token)
            }

            fn refresh<'a>(
                &'a self,
                refresh_token: &'a str,
                context: &'a KimiWebRequestContext,
            ) -> SessionRefreshFuture<'a> {
                Box::pin(async move {
                    let response = self.inner.refresh(refresh_token, context).await;
                    self.current.store(false, Ordering::SeqCst);
                    response
                })
            }
        }

        let client = CancelOnRefresh {
            inner: FakeKimiBrowserClient::default(),
            current: AtomicBool::new(true),
        };
        client.inner.queue_fetch(FakeFetchResponse::Authentication);
        client
            .inner
            .queue_refresh(FakeRefreshResponse::Authentication);
        let reader = FakeBrowserValueReader::with_reads(vec![Some(tokens(
            "expired-access",
            "browser-refresh",
        ))]);
        let profile = supported_profile(KimiBrowserKind::Edge, "Default");
        let secrets = FakeSecretStorage::with_secret(BrowserSessionSecret {
            version: 1,
            observed_browser_refresh_fingerprint: refresh_fingerprint("browser-refresh"),
            active_refresh_token: "saved-refresh".into(),
        });

        let status =
            refresh_profile_with(&reader, &client, &secrets, &profile, Uuid::new_v4(), || {
                client.current.load(Ordering::SeqCst)
            })
            .await;

        assert!(
            secrets.secret().is_some(),
            "a cancelled request deleted the saved credential"
        );
        assert_eq!(
            secrets.secret().unwrap().active_refresh_token,
            "saved-refresh"
        );
        assert!(matches!(status, BrowserProfileRefreshStatus::Cancelled));
    }

    #[tokio::test]
    async fn browser_account_switch_never_keeps_previous_account_quota() {
        let source_id = Uuid::new_v4();
        let profile = supported_profile(KimiBrowserKind::Edge, "Default");
        let source = browser_source(source_id, KimiBrowserKind::Edge, "Default");
        let mut store = KimiAccountStore::default();
        let old_id = store
            .upsert_browser_source(
                identity("old-user", "global-old-user"),
                "old-user".into(),
                source.clone(),
            )
            .account_id;
        store.accounts[0].last_quota = Some(KimiStoredQuota {
            used_percent: 41.0,
            resets_at: None,
            updated_at: "2026-08-23T00:00:00Z".into(),
        });
        let reader =
            FakeBrowserValueReader::with_reads(vec![Some(tokens("new-access", "new-refresh"))]);
        let client = FakeKimiBrowserClient::default();
        client.queue_fetch(account("new-user", 7.0));
        let secrets = FakeSecretStorage::default();

        let status =
            refresh_profile_with(&reader, &client, &secrets, &profile, source_id, || true).await;
        let mut summary = KimiBrowserScanSummary::default();
        apply_profile_outcome(
            &mut store,
            BrowserProfileRefreshOutcome {
                source,
                was_existing: true,
                profile_fingerprint: browser_profile_fingerprint(&profile),
                status,
            },
            false,
            &mut summary,
        );

        assert!(
            store
                .accounts
                .iter()
                .find(|account| account.account_id == old_id)
                .unwrap()
                .last_quota
                .is_none()
        );
        assert_eq!(
            store
                .accounts
                .iter()
                .find_map(|account| account.last_quota.as_ref().map(|quota| quota.used_percent)),
            Some(7.0)
        );
        assert!(
            !store
                .accounts
                .iter()
                .filter_map(|account| account.last_quota.as_ref())
                .any(|quota| (quota.used_percent - 41.0).abs() < f64::EPSILON)
        );
    }

    #[tokio::test]
    async fn one_profile_failure_does_not_block_the_other_profile() {
        let edge_profile = supported_profile(KimiBrowserKind::Edge, "Default");
        let chrome_profile = supported_profile(KimiBrowserKind::Chrome, "Profile 1");
        let edge_source = browser_source(Uuid::new_v4(), KimiBrowserKind::Edge, "Default");
        let chrome_source = browser_source(Uuid::new_v4(), KimiBrowserKind::Chrome, "Profile 1");
        let mut store = KimiAccountStore::default();
        store.upsert_browser_source(
            identity("edge-user", "global-edge-user"),
            "edge-user".into(),
            edge_source.clone(),
        );
        store.upsert_browser_source(
            identity("chrome-user", "global-chrome-user"),
            "chrome-user".into(),
            chrome_source.clone(),
        );
        let edge_reader =
            FakeBrowserValueReader::with_reads(vec![Some(tokens("edge-access", "edge-refresh"))]);
        let chrome_reader = FakeBrowserValueReader::with_reads(vec![Some(tokens(
            "chrome-access",
            "chrome-refresh",
        ))]);
        let edge_client = FakeKimiBrowserClient::default();
        edge_client.queue_fetch(FakeFetchResponse::Temporary);
        let chrome_client = FakeKimiBrowserClient::default();
        chrome_client.queue_fetch(account("chrome-user", 26.75));
        let edge_secrets = FakeSecretStorage::default();
        let chrome_secrets = FakeSecretStorage::default();

        let (edge_status, chrome_status) = tokio::join!(
            refresh_profile_with(
                &edge_reader,
                &edge_client,
                &edge_secrets,
                &edge_profile,
                edge_source.source_id,
                || true,
            ),
            refresh_profile_with(
                &chrome_reader,
                &chrome_client,
                &chrome_secrets,
                &chrome_profile,
                chrome_source.source_id,
                || true,
            )
        );
        let mut summary = KimiBrowserScanSummary::default();
        apply_profile_outcome(
            &mut store,
            BrowserProfileRefreshOutcome {
                source: edge_source,
                was_existing: true,
                profile_fingerprint: browser_profile_fingerprint(&edge_profile),
                status: edge_status,
            },
            false,
            &mut summary,
        );
        apply_profile_outcome(
            &mut store,
            BrowserProfileRefreshOutcome {
                source: chrome_source,
                was_existing: true,
                profile_fingerprint: browser_profile_fingerprint(&chrome_profile),
                status: chrome_status,
            },
            false,
            &mut summary,
        );

        assert_eq!(summary.refreshed, 1);
        assert_eq!(summary.failed, 1);
        assert!(store.accounts.iter().any(|account| {
            account.display_name == "chrome-user"
                && account
                    .last_quota
                    .as_ref()
                    .is_some_and(|quota| (quota.used_percent - 26.75).abs() < f64::EPSILON)
        }));
    }

    #[test]
    fn automatic_scan_skips_removed_profile_until_forced_scan() {
        let profile = supported_profile(KimiBrowserKind::Chrome, "Profile 1");
        let fingerprint = browser_profile_fingerprint(&profile);
        let mut store = KimiAccountStore::default();
        store.suppressed_browser_profiles.push(fingerprint.clone());

        assert!(build_scan_candidates(&store, vec![profile.clone()], false).is_empty());
        let forced = build_scan_candidates(&store, vec![profile.clone()], true);
        assert_eq!(forced.len(), 1);
        let mut summary = KimiBrowserScanSummary::default();
        apply_profile_outcome(
            &mut store,
            BrowserProfileRefreshOutcome {
                source: forced.into_iter().next().unwrap().source,
                was_existing: false,
                profile_fingerprint: fingerprint,
                status: match account("chrome-user", 26.75) {
                    FakeFetchResponse::Account(identity, quota) => {
                        BrowserProfileRefreshStatus::Success { identity, quota }
                    }
                    _ => unreachable!(),
                },
            },
            true,
            &mut summary,
        );

        assert_eq!(summary.discovered, 1);
        assert!(store.suppressed_browser_profiles.is_empty());
    }

    #[test]
    fn suppression_blocks_automatic_rediscovery_without_deleting_secrets() {
        let source_id = Uuid::new_v4();
        let source = browser_source(source_id, KimiBrowserKind::Edge, "Default");
        let profile = profile_from_source(&source);
        let mut store = KimiAccountStore::default();
        store.upsert_browser_source(
            identity("edge-user", "global-edge-user"),
            "edge-user".into(),
            source,
        );
        let record = store.accounts[0].clone();
        let secrets = FakeSecretStorage::with_secret(BrowserSessionSecret {
            version: 1,
            observed_browser_refresh_fingerprint: refresh_fingerprint("browser-refresh"),
            active_refresh_token: "active-refresh".into(),
        });

        suppress_browser_sources(&mut store, &record);

        assert!(secrets.secret().is_some());
        assert_eq!(store.suppressed_browser_profiles.len(), 1);
        assert!(build_scan_candidates(&store, vec![profile], false).is_empty());
    }

    #[test]
    fn secret_roundtrip_uses_source_uuid_and_never_metadata_path() {
        let backend = MemoryCredentialStore::default();
        let secrets = BrowserSessionSecretStore::new(backend.clone());
        let source_id = Uuid::parse_str("00000000-0000-0000-0000-000000000111").unwrap();
        secrets
            .save(
                source_id,
                &BrowserSessionSecret {
                    version: 1,
                    observed_browser_refresh_fingerprint: refresh_fingerprint("browser-refresh"),
                    active_refresh_token: "active-refresh".into(),
                },
            )
            .unwrap();

        let loaded = secrets.load(source_id).unwrap().unwrap();

        assert_eq!(
            loaded.observed_browser_refresh_fingerprint,
            refresh_fingerprint("browser-refresh")
        );
        assert_eq!(loaded.active_refresh_token, "active-refresh");
        assert_eq!(
            backend.keys(),
            vec![(
                "com.codexbar.desktop.kimi-browser-session".to_string(),
                source_id.to_string(),
            )]
        );
        assert!(
            backend
                .keys()
                .iter()
                .all(|(_, key)| !key.contains("Profile 1"))
        );
    }

    #[test]
    fn delete_treats_missing_credential_as_success() {
        let backend = MemoryCredentialStore::default();
        let secrets = BrowserSessionSecretStore::new(backend);
        let source_id = Uuid::parse_str("00000000-0000-0000-0000-000000000222").unwrap();

        assert!(secrets.delete(source_id).is_ok());
        assert!(secrets.delete(source_id).is_ok());
    }

    #[test]
    fn serialized_account_metadata_contains_no_access_or_refresh_token() {
        const ACCESS_MARKER: &str = "TEST_ACCESS_SECRET_DO_NOT_LOG";
        const REFRESH_MARKER: &str = "TEST_REFRESH_SECRET_DO_NOT_LOG";
        let backend = MemoryCredentialStore::default();
        let secrets = BrowserSessionSecretStore::new(backend);
        let source_id = Uuid::parse_str("00000000-0000-0000-0000-000000000333").unwrap();
        secrets
            .save(
                source_id,
                &BrowserSessionSecret {
                    version: 1,
                    observed_browser_refresh_fingerprint: refresh_fingerprint(REFRESH_MARKER),
                    active_refresh_token: format!("{ACCESS_MARKER}-{REFRESH_MARKER}"),
                },
            )
            .unwrap();
        let mut store = KimiAccountStore::default();
        store.upsert_browser_source(
            identity("user-a", "global-a"),
            "账号 A".into(),
            browser_source(source_id, KimiBrowserKind::Edge, "Default"),
        );

        let json = serde_json::to_string(&store).unwrap();

        assert!(json.contains("Default"));
        assert!(!json.contains(ACCESS_MARKER));
        assert!(!json.contains(REFRESH_MARKER));
    }
}
