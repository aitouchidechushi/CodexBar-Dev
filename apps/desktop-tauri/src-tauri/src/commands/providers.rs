use super::*;
use std::sync::{Arc, OnceLock};

const MAX_CONCURRENT_PROVIDER_FETCHES: usize = 8;
const PROVIDER_FAILURE_RETRY_AFTER: std::time::Duration = std::time::Duration::from_secs(10);
static PROVIDER_FETCH_PERMITS: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();

pub(crate) fn provider_fetch_permits() -> Arc<tokio::sync::Semaphore> {
    Arc::clone(
        PROVIDER_FETCH_PERMITS
            .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_PROVIDER_FETCHES))),
    )
}

// ── Provider refresh commands ────────────────────────────────────────

/// Build a `FetchContext` for a provider using persisted cookies/keys.
pub(crate) fn build_fetch_context(
    id: ProviderId,
    settings: &Settings,
    cookies: &ManualCookies,
    api_keys: &ApiKeys,
    token_accounts: &HashMap<ProviderId, ProviderAccountData>,
) -> FetchContext {
    let cookie_source = settings.cookie_source(id);
    let stored_cookie = cookies.get(id.cli_name()).map(|s| s.to_string());
    let stored_api_key = api_keys.get(id.cli_name()).map(|s| s.to_string());
    let token_override = token_accounts
        .get(&id)
        .and_then(|data| data.active_account())
        .cloned()
        .map(|account| TokenAccountOverride::from_account(id, account));
    let active_token_cookie = token_override
        .as_ref()
        .and_then(|override_data| override_data.cookie_header.clone());
    let active_token_env = token_override
        .as_ref()
        .and_then(|override_data| override_data.env_override.as_ref());
    let active_token_api_key = active_token_env.and_then(|env| env.values().next().cloned());
    let usage_source = SourceMode::parse(settings.usage_source(id)).unwrap_or_default();
    // Selected token-account key overrides a stored provider apiKey (upstream #2271 / #1183).
    let api_key = active_token_api_key.or(stored_api_key);
    let has_kimi_code_api_key =
        id == ProviderId::Kimi && api_key.as_deref().is_some_and(|key| !key.trim().is_empty());

    let (mut source_mode, mut cookie_header) = if id.cookie_domain().is_none() {
        let source_mode = if active_token_env.is_some() {
            SourceMode::OAuth
        } else {
            usage_source
        };
        (source_mode, None)
    } else {
        match cookie_source {
            _ if active_token_env.is_some() => (SourceMode::OAuth, None),
            "off" if id == ProviderId::Claude && usage_source != SourceMode::Cli => {
                (SourceMode::OAuth, None)
            }
            "off" if has_kimi_code_api_key && usage_source == SourceMode::Auto => {
                (SourceMode::Auto, None)
            }
            // Droid/Factory: cookie-off must never scrape browser cookies. Map to
            // Cli (API-only in the provider) so Auto does not fall through to web.
            "off" if id == ProviderId::Factory => (SourceMode::Cli, None),
            "off" => (SourceMode::Cli, None),
            "manual" => {
                let cookie_header = active_token_cookie.or(stored_cookie);
                let source_mode = if has_kimi_code_api_key && usage_source == SourceMode::Auto {
                    SourceMode::Auto
                } else if cookie_header.is_some() {
                    SourceMode::Web
                } else if id == ProviderId::Claude && usage_source != SourceMode::Cli {
                    SourceMode::OAuth
                } else {
                    SourceMode::Cli
                };
                (source_mode, cookie_header)
            }
            // `browser` is accepted as a legacy alias from older settings.
            "auto" | "browser" | "web" => {
                // Try browser cookie extraction as fallback when no manual cookie is set.
                // On non-Windows this is a harmless no-op that returns an error.
                let cookie_header = active_token_cookie.or(stored_cookie).or_else(|| {
                    provider_cookie_domain(id, settings).and_then(|domain| {
                        codexbar::browser::cookies::get_cookie_header(domain)
                            .ok()
                            .filter(|h| !h.is_empty())
                    })
                });
                (usage_source, cookie_header)
            }
            _ => (usage_source, stored_cookie),
        }
    };

    // Cookie-web providers (Cursor, OpenCode, …) reject SourceMode::Cli. The shell
    // historically mapped "manual + no cookie" to Cli, which surfaces as
    // "Source mode 'Cli' not supported". Remap to Web and try browser cookies
    // unless the user explicitly disabled cookies ("off").
    if source_mode == SourceMode::Cli
        && cookie_source != "off"
        && !instantiate_provider(id).supports_cli()
    {
        if cookie_header
            .as_deref()
            .map(str::trim)
            .is_none_or(|s| s.is_empty())
        {
            cookie_header = provider_cookie_domain(id, settings).and_then(|domain| {
                codexbar::browser::cookies::get_cookie_header(domain)
                    .ok()
                    .filter(|h| !h.is_empty())
            });
        }
        source_mode = SourceMode::Web;
    }

    let workspace_id = settings.workspace_id(id).trim().to_string();
    let api_region = settings.api_region(id).trim().to_string();
    let gateway_url = (id == ProviderId::Wayfinder && !settings.gateway_url(id).is_empty())
        .then(|| settings.gateway_url(id).to_string());
    // Local-first Auto providers (OpenCode Go) flip to web-first when a
    // token account or manual cookie source scopes the session to web creds.
    let auto_prefer_web = token_override.is_some() || cookie_source == "manual";

    FetchContext {
        source_mode,
        manual_cookie_header: cookie_header,
        api_key,
        workspace_id: (!workspace_id.is_empty()).then_some(workspace_id),
        api_region: (!api_region.is_empty()).then_some(api_region),
        gateway_url,
        auto_prefer_web,
        ..FetchContext::default()
    }
}

pub(crate) fn provider_cookie_domain(id: ProviderId, settings: &Settings) -> Option<&'static str> {
    if id == ProviderId::MiniMax {
        return Some(
            codexbar::providers::MiniMaxProvider::cookie_domain_for_region(Some(
                settings.api_region(id),
            )),
        );
    }
    if id == ProviderId::Alibaba {
        return Some(
            codexbar::providers::AlibabaProvider::cookie_domain_for_region(Some(
                settings.api_region(id),
            )),
        );
    }
    id.cookie_domain()
}

const DEFAULT_PROVIDER_FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(35);
const SLOW_PROVIDER_FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(75);
const MAX_CONTEXT_FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(65);

pub(crate) fn provider_fetch_timeout(id: ProviderId, ctx: &FetchContext) -> std::time::Duration {
    let provider_timeout = match id {
        ProviderId::Claude | ProviderId::Codex | ProviderId::Copilot => SLOW_PROVIDER_FETCH_TIMEOUT,
        _ => DEFAULT_PROVIDER_FETCH_TIMEOUT,
    };
    let context_timeout = std::time::Duration::from_secs(ctx.web_timeout.saturating_add(5));
    provider_timeout.max(context_timeout.min(MAX_CONTEXT_FETCH_TIMEOUT))
}

pub(crate) fn is_provider_cache_fresh(
    updated_at: Option<std::time::Instant>,
    stale_after: std::time::Duration,
) -> bool {
    updated_at
        .map(|updated| updated.elapsed() <= stale_after)
        .unwrap_or(false)
}

pub(crate) fn upsert_provider_cache(
    cache: &mut Vec<ProviderUsageSnapshot>,
    snapshot: ProviderUsageSnapshot,
) {
    let identity = ProviderSnapshotIdentity::from_snapshot(&snapshot);
    if let Some(existing) = cache
        .iter_mut()
        .find(|existing| ProviderSnapshotIdentity::from_snapshot(existing) == identity)
    {
        *existing = snapshot;
    } else {
        cache.push(snapshot);
    }
}

pub(crate) fn reconcile_provider_cache(
    cache: &mut Vec<ProviderUsageSnapshot>,
    expected: &[ProviderSnapshotIdentity],
) {
    cache.retain(|snapshot| expected.contains(&ProviderSnapshotIdentity::from_snapshot(snapshot)));
}

pub(crate) fn sort_provider_snapshots(
    snapshots: &mut [ProviderUsageSnapshot],
    provider_order: &[ProviderId],
) {
    snapshots.sort_by(|left, right| {
        let left_provider = provider_order
            .iter()
            .position(|id| id.cli_name() == left.provider_id)
            .unwrap_or(usize::MAX);
        let right_provider = provider_order
            .iter()
            .position(|id| id.cli_name() == right.provider_id)
            .unwrap_or(usize::MAX);
        left_provider
            .cmp(&right_provider)
            .then_with(|| {
                left.credential_display_ordinal
                    .unwrap_or_default()
                    .cmp(&right.credential_display_ordinal.unwrap_or_default())
            })
            .then_with(|| left.credential_id.cmp(&right.credential_id))
    });
}

/// Drop cached snapshots for providers that are no longer enabled.
pub(crate) fn prune_provider_cache_to_enabled(
    cache: &mut Vec<ProviderUsageSnapshot>,
    enabled_ids: &[ProviderId],
) {
    cache.retain(|snapshot| {
        enabled_ids
            .iter()
            .any(|id| id.cli_name() == snapshot.provider_id)
    });
}

/// Invalidate in-flight publish work and remove disabled providers from cache.
///
/// Also clears the refresh lock so a follow-up force refresh can start immediately
/// (otherwise `begin_provider_refresh` no-ops while a superseded batch still holds
/// `is_refreshing`, and newly enabled providers never get a replacement run).
pub(crate) fn invalidate_provider_refresh_and_prune_disabled(
    state: &tauri::State<'_, Mutex<AppState>>,
    enabled_ids: &[ProviderId],
) -> Result<(), String> {
    let mut guard = state.lock().map_err(|e| e.to_string())?;
    guard.provider_refresh_generation = guard.provider_refresh_generation.wrapping_add(1);
    guard.is_refreshing = false;
    guard.provider_refresh_started_at = None;
    prune_provider_cache_to_enabled(&mut guard.provider_cache, enabled_ids);
    // Drop transient-failure counters for providers that left the enabled set.
    guard
        .transient_provider_failure_counts
        .retain(|identity, _| {
            enabled_ids
                .iter()
                .any(|id| id.cli_name() == identity.provider_id)
        });
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CredentialCacheMutation {
    Added(uuid::Uuid),
    SecretReplaced(uuid::Uuid),
    Deleted(uuid::Uuid),
    Relabeled(uuid::Uuid),
    Reordered,
    ProviderRevoked,
}

pub(crate) fn expected_provider_snapshot_identities(
    enabled_ids: &[ProviderId],
    api_keys: &ApiKeys,
) -> Vec<ProviderSnapshotIdentity> {
    enabled_ids
        .iter()
        .flat_map(|id| {
            let provider_id = id.cli_name().to_string();
            let entries = api_keys.active_entries(id.cli_name()).collect::<Vec<_>>();
            if entries.is_empty() {
                vec![ProviderSnapshotIdentity::new(provider_id, None)]
            } else {
                entries
                    .into_iter()
                    .map(|entry| ProviderSnapshotIdentity::new(provider_id.clone(), Some(entry.id)))
                    .collect()
            }
        })
        .collect()
}

/// Apply the synchronous half of a credential mutation only after persistence.
///
/// The `mutation` error path deliberately returns before touching state. The
/// success path supersedes old publish work, reconciles cache against the
/// current store, and reports whether the caller should start a forced refresh.
pub(crate) fn apply_credential_mutation_result(
    state: &mut AppState,
    enabled_ids: &[ProviderId],
    api_keys: &ApiKeys,
    provider_id: &str,
    mutation: Result<CredentialCacheMutation, String>,
) -> Result<bool, String> {
    let mutation = mutation?;
    let expected = expected_provider_snapshot_identities(enabled_ids, api_keys);
    let should_refresh = !matches!(mutation, CredentialCacheMutation::Reordered);

    state.provider_refresh_generation = state.provider_refresh_generation.wrapping_add(1);
    state.is_refreshing = false;
    state.provider_refresh_started_at = None;
    reconcile_provider_cache(&mut state.provider_cache, &expected);
    state
        .transient_provider_failure_counts
        .retain(|identity, _| expected.contains(identity));

    let changed_credential_id = match mutation {
        CredentialCacheMutation::SecretReplaced(id) | CredentialCacheMutation::Deleted(id) => {
            Some(id)
        }
        CredentialCacheMutation::Added(id) => {
            debug_assert!(
                api_keys
                    .entries(provider_id)
                    .iter()
                    .any(|entry| entry.id == id)
            );
            None
        }
        CredentialCacheMutation::Relabeled(id) => {
            if let Some(entry) = api_keys
                .entries(provider_id)
                .iter()
                .find(|entry| entry.id == id)
            {
                let label = entry
                    .label
                    .clone()
                    .unwrap_or_else(|| format!("Key {}", entry.ordinal));
                for snapshot in state.provider_cache.iter_mut().filter(|snapshot| {
                    snapshot.provider_id == provider_id && snapshot.credential_id == Some(id)
                }) {
                    snapshot.credential_display_label = Some(label.clone());
                    snapshot.credential_display_ordinal = Some(entry.ordinal);
                }
            } else {
                state.provider_cache.retain(|snapshot| {
                    snapshot.provider_id != provider_id || snapshot.credential_id != Some(id)
                });
            }
            None
        }
        CredentialCacheMutation::Reordered => {
            for snapshot in state.provider_cache.iter_mut().filter(|snapshot| {
                snapshot.provider_id == provider_id && snapshot.credential_id.is_some()
            }) {
                let Some(entry) = snapshot.credential_id.and_then(|id| {
                    api_keys
                        .entries(provider_id)
                        .iter()
                        .find(|entry| entry.id == id)
                }) else {
                    continue;
                };
                snapshot.credential_display_label = Some(
                    entry
                        .label
                        .clone()
                        .unwrap_or_else(|| format!("Key {}", entry.ordinal)),
                );
                snapshot.credential_display_ordinal = Some(entry.ordinal);
            }
            None
        }
        CredentialCacheMutation::ProviderRevoked => {
            state
                .provider_cache
                .retain(|snapshot| snapshot.provider_id != provider_id);
            state
                .transient_provider_failure_counts
                .retain(|identity, _| identity.provider_id != provider_id);
            None
        }
    };

    if let Some(credential_id) = changed_credential_id {
        state.provider_cache.retain(|snapshot| {
            snapshot.provider_id != provider_id || snapshot.credential_id != Some(credential_id)
        });
        state
            .transient_provider_failure_counts
            .retain(|identity, _| {
                identity.provider_id != provider_id || identity.credential_id != Some(credential_id)
            });
    }

    let credential_group_size = api_keys.active_entries(provider_id).count();
    for snapshot in state
        .provider_cache
        .iter_mut()
        .filter(|snapshot| snapshot.provider_id == provider_id && snapshot.credential_id.is_some())
    {
        snapshot.credential_group_size = Some(credential_group_size);
    }

    sort_provider_snapshots(&mut state.provider_cache, enabled_ids);
    Ok(should_refresh && enabled_ids.iter().any(|id| id.cli_name() == provider_id))
}

pub(crate) fn is_current_provider_refresh_generation(guard: &AppState, generation: u64) -> bool {
    guard.provider_refresh_generation == generation
}

pub(crate) fn abort_provider_refresh_generation(guard: &mut AppState, generation: u64) {
    if is_current_provider_refresh_generation(guard, generation) {
        guard.is_refreshing = false;
        guard.provider_refresh_started_at = None;
    }
}

/// Core refresh logic, usable from both the Tauri command and tray menu actions.
pub(crate) async fn do_refresh_providers(app: &tauri::AppHandle) -> Result<(), String> {
    do_refresh_providers_with_policy(app, true).await
}

pub(crate) async fn do_refresh_providers_if_stale(app: &tauri::AppHandle) -> Result<(), String> {
    do_refresh_providers_with_policy(app, false).await
}

async fn do_refresh_providers_with_policy(
    app: &tauri::AppHandle,
    force: bool,
) -> Result<(), String> {
    let state = app.state::<Mutex<AppState>>();

    let Some(generation) = begin_provider_refresh(&state, force)? else {
        return Ok(());
    };

    let settings = crate::storage_services::settings_snapshot(&state);
    let storage = crate::storage_services::storage_snapshot(&state);
    let inputs = match ProviderRefreshInputs::load(settings, &storage) {
        Ok(inputs) => inputs,
        Err(error) => {
            let mut guard = state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            abort_provider_refresh_generation(&mut guard, generation);
            return Err(crate::storage_services::command_store_error(error));
        }
    };
    let jobs = plan_provider_refresh_jobs(
        &inputs.enabled_ids,
        &inputs.settings,
        &inputs.manual_cookies,
        &inputs.api_keys,
        &inputs.token_accounts,
    );
    let expected_identities = jobs
        .iter()
        .map(ProviderRefreshJob::identity)
        .collect::<Vec<_>>();
    // Reconcile deleted credentials, obsolete default lanes, and disabled providers.
    if let Ok(mut guard) = state.lock()
        && is_current_provider_refresh_generation(&guard, generation)
    {
        reconcile_provider_cache(&mut guard.provider_cache, &expected_identities);
        sort_provider_snapshots(&mut guard.provider_cache, &inputs.enabled_ids);
    }

    events::emit_refresh_started(
        app,
        inputs
            .enabled_ids
            .iter()
            .map(|id| id.cli_name().to_string())
            .collect(),
    );
    let enabled_count = inputs.enabled_ids.len();

    let handles = spawn_provider_refreshes(app, jobs, generation);
    let outcomes = await_provider_refreshes(handles).await;

    let Some(summary) =
        finish_provider_refresh(&state, generation, &outcomes, &inputs.enabled_ids)?
    else {
        // Superseded by a newer generation (or invalidate). Do not clear UI
        // "refreshing" for a dead batch or stamp tray from incomplete work.
        return Ok(());
    };
    update_tray_and_notifications(app, &state, &inputs.settings, &inputs.token_accounts)?;

    events::emit_refresh_complete(
        app,
        enabled_count,
        summary.error_count,
        summary.degraded_credential_count,
    );
    let account_app = app.clone();
    tauri::async_runtime::spawn(async move {
        crate::kimi_accounts::refresh_kimi_accounts(&account_app).await;
    });
    crate::auto_refresh::schedule_refresh_enrichment(&inputs.settings);

    Ok(())
}

fn begin_provider_refresh(
    state: &tauri::State<'_, Mutex<AppState>>,
    force: bool,
) -> Result<Option<u64>, String> {
    let mut guard = state.lock().map_err(|e| e.to_string())?;
    if guard.is_refreshing {
        return Ok(None);
    }
    if provider_cache_can_skip_refresh(&guard, force) {
        return Ok(None);
    }

    guard.provider_refresh_generation = guard.provider_refresh_generation.wrapping_add(1);
    let generation = guard.provider_refresh_generation;
    guard.is_refreshing = true;
    guard.provider_refresh_started_at = Some(std::time::Instant::now());
    Ok(Some(generation))
}

fn provider_cache_can_skip_refresh(guard: &AppState, force: bool) -> bool {
    if force || guard.provider_cache.is_empty() {
        return false;
    }

    let stale_after = if guard
        .provider_cache
        .iter()
        .any(|snapshot| snapshot.error.is_some() || snapshot.refresh_error.is_some())
    {
        PROVIDER_FAILURE_RETRY_AFTER
    } else {
        PROVIDER_CACHE_STALE_AFTER
    };
    is_provider_cache_fresh(guard.provider_cache_updated_at, stale_after)
}

pub(crate) struct ProviderRefreshInputs {
    settings: Settings,
    enabled_ids: Vec<ProviderId>,
    manual_cookies: ManualCookies,
    api_keys: ApiKeys,
    token_accounts: TokenAccounts,
}

impl ProviderRefreshInputs {
    pub(crate) fn load(
        settings: Settings,
        storage: &crate::storage_services::StorageServices,
    ) -> Result<Self, codexbar::storage::StoreError> {
        let enabled_ids = settings.get_enabled_provider_ids();
        let manual_cookies = storage
            .manual_cookies
            .load()?
            .into_writable_for(StoreKind::ManualCookies)?;
        let api_keys = storage
            .api_keys
            .load()?
            .into_writable_for(StoreKind::ApiKeys)?;
        let token_accounts = storage
            .token_accounts
            .load()?
            .into_writable_for(StoreKind::TokenAccounts)?;

        Ok(Self {
            settings,
            enabled_ids,
            manual_cookies,
            api_keys,
            token_accounts,
        })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ProviderRefreshJob {
    pub provider_id: ProviderId,
    pub credential: Option<CredentialSnapshotMeta>,
    pub ctx: FetchContext,
}

impl ProviderRefreshJob {
    fn identity(&self) -> ProviderSnapshotIdentity {
        ProviderSnapshotIdentity::new(
            self.provider_id.cli_name().to_string(),
            self.credential.as_ref().map(|credential| credential.id),
        )
    }
}

fn credential_source_mode(id: ProviderId, fallback: SourceMode) -> SourceMode {
    match id {
        ProviderId::Kimi | ProviderId::Factory | ProviderId::Ollama => SourceMode::OAuth,
        ProviderId::Amp => SourceMode::Web,
        _ => fallback,
    }
}

fn build_credential_fetch_context(
    id: ProviderId,
    secret: &str,
    settings: &Settings,
    cookies: &ManualCookies,
    api_keys: &ApiKeys,
    token_accounts: &HashMap<ProviderId, ProviderAccountData>,
) -> FetchContext {
    let mut ctx = build_fetch_context(id, settings, cookies, api_keys, token_accounts);
    ctx.source_mode = credential_source_mode(id, ctx.source_mode);
    ctx.api_key = Some(secret.to_string());
    ctx.manual_cookie_header = None;
    ctx.auto_prefer_web = false;
    ctx
}

pub(crate) fn plan_provider_refresh_jobs(
    enabled_ids: &[ProviderId],
    settings: &Settings,
    cookies: &ManualCookies,
    api_keys: &ApiKeys,
    token_accounts: &HashMap<ProviderId, ProviderAccountData>,
) -> Vec<ProviderRefreshJob> {
    let mut jobs = Vec::new();
    for &provider_id in enabled_ids {
        let entries = api_keys
            .active_entries(provider_id.cli_name())
            .collect::<Vec<_>>();
        if entries.is_empty() {
            jobs.push(ProviderRefreshJob {
                provider_id,
                credential: None,
                ctx: build_fetch_context(provider_id, settings, cookies, api_keys, token_accounts),
            });
            continue;
        }

        jobs.extend(entries.iter().map(|entry| {
            let display_label = entry
                .label
                .clone()
                .unwrap_or_else(|| format!("Key {}", entry.ordinal));
            ProviderRefreshJob {
                provider_id,
                credential: Some(CredentialSnapshotMeta::in_group(
                    entry.id,
                    display_label,
                    entry.ordinal,
                    entries.len(),
                )),
                ctx: build_credential_fetch_context(
                    provider_id,
                    &entry.secret,
                    settings,
                    cookies,
                    api_keys,
                    token_accounts,
                ),
            }
        }));
    }
    jobs
}

#[derive(Debug, Clone)]
pub(crate) struct ProviderRefreshOutcome {
    provider_id: ProviderId,
    credential_id: Option<uuid::Uuid>,
    succeeded: bool,
}

impl ProviderRefreshOutcome {
    pub(crate) fn new(
        provider_id: ProviderId,
        credential_id: Option<uuid::Uuid>,
        succeeded: bool,
    ) -> Self {
        Self {
            provider_id,
            credential_id,
            succeeded,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProviderRefreshSummary {
    pub error_count: usize,
    pub degraded_credential_count: usize,
}

pub(crate) fn summarize_provider_refresh_outcomes(
    outcomes: &[ProviderRefreshOutcome],
) -> ProviderRefreshSummary {
    let mut provider_success = HashMap::<ProviderId, bool>::new();
    let mut degraded_credential_count = 0;
    for outcome in outcomes {
        provider_success
            .entry(outcome.provider_id)
            .and_modify(|succeeded| *succeeded |= outcome.succeeded)
            .or_insert(outcome.succeeded);
        if outcome.credential_id.is_some() && !outcome.succeeded {
            degraded_credential_count += 1;
        }
    }
    ProviderRefreshSummary {
        error_count: provider_success
            .values()
            .filter(|succeeded| !**succeeded)
            .count(),
        degraded_credential_count,
    }
}

struct ProviderRefreshHandle {
    provider_id: ProviderId,
    credential_id: Option<uuid::Uuid>,
    handle: tokio::task::JoinHandle<ProviderRefreshOutcome>,
}

fn spawn_provider_refreshes(
    app: &tauri::AppHandle,
    jobs: Vec<ProviderRefreshJob>,
    generation: u64,
) -> Vec<ProviderRefreshHandle> {
    let mut handles = Vec::with_capacity(jobs.len());
    let fetch_permits = provider_fetch_permits();

    for job in jobs {
        let provider_id = job.provider_id;
        let credential_id = job.credential.as_ref().map(|credential| credential.id);
        let app_handle = app.clone();
        let fetch_permits = Arc::clone(&fetch_permits);
        let handle = tokio::spawn(async move {
            let Ok(_permit) = fetch_permits.acquire_owned().await else {
                return ProviderRefreshOutcome::new(provider_id, credential_id, false);
            };
            refresh_provider(app_handle, job, generation).await
        });
        handles.push(ProviderRefreshHandle {
            provider_id,
            credential_id,
            handle,
        });
    }

    handles
}

async fn refresh_provider(
    app: tauri::AppHandle,
    job: ProviderRefreshJob,
    generation: u64,
) -> ProviderRefreshOutcome {
    let id = job.provider_id;
    let credential_id = job.credential.as_ref().map(|credential| credential.id);
    let snapshot = fetch_provider_snapshot(id, job.ctx, job.credential.as_ref()).await;
    let fetched_succeeded = snapshot.error.is_none();

    let state = app.state::<Mutex<AppState>>();
    let published = if let Ok(mut guard) = state.lock() {
        if !is_current_provider_refresh_generation(&guard, generation) {
            tracing::debug!(
                provider = id.cli_name(),
                generation,
                current = guard.provider_refresh_generation,
                "dropping superseded provider refresh result"
            );
            None
        } else {
            let snapshot = preserve_last_good_transient_failure_for_identity(
                &mut guard,
                id,
                credential_id,
                snapshot,
            );
            upsert_provider_cache(&mut guard.provider_cache, snapshot.clone());
            Some(snapshot)
        }
    } else {
        None
    };

    if let Some(snapshot) = published {
        events::emit_provider_updated(&app, &snapshot);
    }
    ProviderRefreshOutcome::new(id, credential_id, fetched_succeeded)
}

#[cfg(test)]
pub(super) fn preserve_last_good_transient_failure(
    guard: &mut AppState,
    id: ProviderId,
    snapshot: ProviderUsageSnapshot,
) -> ProviderUsageSnapshot {
    preserve_last_good_transient_failure_for_identity(guard, id, None, snapshot)
}

fn preserve_last_good_transient_failure_for_identity(
    guard: &mut AppState,
    id: ProviderId,
    credential_id: Option<uuid::Uuid>,
    snapshot: ProviderUsageSnapshot,
) -> ProviderUsageSnapshot {
    let identity = ProviderSnapshotIdentity::new(id.cli_name().to_string(), credential_id);
    if snapshot.error.is_none() {
        guard.transient_provider_failure_counts.remove(&identity);
        return snapshot;
    }

    let error = snapshot.error.as_deref();
    if is_retryable_provider_refresh_error(error) {
        let Some(previous) = guard
            .provider_cache
            .iter()
            .find(|cached| {
                ProviderSnapshotIdentity::from_snapshot(cached) == identity && cached.error.is_none()
            })
            .cloned()
        else {
            return snapshot;
        };
        tracing::warn!(
            provider = id.cli_name(),
            error = error.unwrap_or(""),
            "preserving last good provider snapshot after retryable refresh failure"
        );
        return mark_snapshot_as_stale(previous, error);
    }

    if id != ProviderId::Claude {
        guard.transient_provider_failure_counts.remove(&identity);
        return snapshot;
    }

    // Hard auth loss / subscription-unavailable answers should not keep stale bars.
    if is_hard_claude_auth_loss(error) {
        guard.transient_provider_failure_counts.remove(&identity);
        return snapshot;
    }

    let preservable = is_transient_claude_auth_error(error)
        || is_claude_cli_usage_parse_failure(error)
        || is_claude_cli_rate_limit_failure(error)
        || is_timeout_failure(error);
    if !preservable {
        guard.transient_provider_failure_counts.remove(&identity);
        return snapshot;
    }

    let Some(previous) = guard
        .provider_cache
        .iter()
        .find(|cached| {
            ProviderSnapshotIdentity::from_snapshot(cached) == identity && cached.error.is_none()
        })
        .cloned()
    else {
        return snapshot;
    };

    // Parse / rate-limit / timeout: keep last-good every time (upstream #2247).
    // Transient auth (unauthorized-ish) still only preserves once so real logout surfaces.
    let parse_or_rate = is_claude_cli_usage_parse_failure(error)
        || is_claude_cli_rate_limit_failure(error)
        || is_timeout_failure(error);

    let count = guard
        .transient_provider_failure_counts
        .entry(identity)
        .or_insert(0);
    if parse_or_rate || *count == 0 {
        if !parse_or_rate {
            *count = 1;
        }
        tracing::warn!(
            provider = id.cli_name(),
            error = error.unwrap_or(""),
            "preserving last good Claude snapshot after transient failure"
        );
        mark_snapshot_as_stale(previous, error)
    } else {
        *count = count.saturating_add(1);
        snapshot
    }
}

fn mark_snapshot_as_stale(
    mut snapshot: ProviderUsageSnapshot,
    refresh_error: Option<&str>,
) -> ProviderUsageSnapshot {
    snapshot.refresh_error = refresh_error.map(str::to_string);
    snapshot
}

fn is_retryable_provider_refresh_error(error: Option<&str>) -> bool {
    is_timeout_failure(error) || error.map(str::trim).is_some_and(|message| {
        let normalized = message.to_ascii_lowercase();
        normalized.starts_with("network error:")
            || normalized.contains("rate limit")
            || normalized.contains("rate_limit")
            || normalized.contains("ratelimited")
            || is_retryable_http_status_failure(&normalized)
    })
}

fn is_retryable_http_status_failure(message: &str) -> bool {
    ["status", "http"].into_iter().any(|marker| {
        let Some((_, suffix)) = message.split_once(marker) else {
            return false;
        };
        let digits = suffix
            .trim_start_matches(|character: char| {
                character.is_ascii_whitespace() || character == ':'
            })
            .chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>();
        let Ok(status) = digits.parse::<u16>() else {
            return false;
        };
        status == 429 || (500..=599).contains(&status)
    })
}

fn is_transient_claude_auth_error(error: Option<&str>) -> bool {
    let Some(error) = error else {
        return false;
    };
    let lower = error.to_ascii_lowercase();
    lower.contains("unauthorized")
        || lower.contains("authentication required")
        || lower.contains("auth required")
}

fn is_hard_claude_auth_loss(error: Option<&str>) -> bool {
    let Some(error) = error else {
        return false;
    };
    let lower = error.to_ascii_lowercase();
    // Credentials truly missing / login required — clear stale usage.
    lower.contains("credentials not found")
        || lower.contains("run `claude` to authenticate")
        || (lower.contains("not installed") && lower.contains("claude"))
        || (lower.contains("subscription") && lower.contains("unavailable"))
}

fn is_claude_cli_usage_parse_failure(error: Option<&str>) -> bool {
    let Some(error) = error else {
        return false;
    };
    let lower = error.to_ascii_lowercase();
    lower.contains("parse error")
        || lower.contains("empty output")
        || lower.contains("missing current session")
        || lower.contains("treated /usage as a normal prompt")
        || lower.contains("local activity stats")
        || lower.contains("could not parse")
}

fn is_claude_cli_rate_limit_failure(error: Option<&str>) -> bool {
    let Some(error) = error else {
        return false;
    };
    let lower = error.to_ascii_lowercase();
    lower.contains("rate limit") || lower.contains("rate_limit") || lower.contains("ratelimited")
}

fn is_timeout_failure(error: Option<&str>) -> bool {
    let Some(error) = error else {
        return false;
    };
    error.eq_ignore_ascii_case("timeout") || error.to_ascii_lowercase().contains("timed out")
}

async fn fetch_provider_snapshot(
    id: ProviderId,
    ctx: FetchContext,
    credential: Option<&CredentialSnapshotMeta>,
) -> ProviderUsageSnapshot {
    let provider = instantiate_provider(id);
    let metadata = provider.metadata().clone();
    let started = std::time::Instant::now();

    if let Some(credential) = credential
        && credential_fetch_unsupported_reason(id).is_some()
    {
        let mut snapshot = unsupported_credential_snapshot(id, &metadata, credential);
        record_provider_fetch_duration(id, &mut snapshot, started);
        return snapshot;
    }

    let mut snapshot =
        match tokio::time::timeout(provider_fetch_timeout(id, &ctx), provider.fetch_usage(&ctx))
            .await
        {
            Ok(Ok(result)) => ProviderUsageSnapshot::from_fetch_result(id, &metadata, &result),
            Ok(Err(e)) => ProviderUsageSnapshot::from_error(
                id,
                &metadata,
                codexbar::logging::safe_error_message(e),
            ),
            Err(_) => ProviderUsageSnapshot::from_error(id, &metadata, "Timeout".to_string()),
        };

    record_provider_fetch_duration(id, &mut snapshot, started);
    if let Some(credential) = credential {
        finalize_credential_snapshot(id, &metadata, snapshot, credential)
    } else {
        snapshot
    }
}

fn credential_fetch_unsupported_reason(id: ProviderId) -> Option<&'static str> {
    match id {
        ProviderId::Alibaba => Some("the Alibaba fetcher is web-cookie only"),
        ProviderId::Grok => Some("the Grok fetcher is web-cookie only"),
        _ => None,
    }
}

fn unsupported_credential_snapshot(
    id: ProviderId,
    metadata: &ProviderMetadata,
    credential: &CredentialSnapshotMeta,
) -> ProviderUsageSnapshot {
    let reason =
        credential_fetch_unsupported_reason(id).unwrap_or("the fetcher ignores explicit API keys");
    ProviderUsageSnapshot::from_error(
        id,
        metadata,
        format!("This provider does not support app-managed API keys: {reason}"),
    )
    .decorate_for_credential(credential)
}

pub(crate) fn finalize_credential_snapshot(
    id: ProviderId,
    metadata: &ProviderMetadata,
    snapshot: ProviderUsageSnapshot,
    credential: &CredentialSnapshotMeta,
) -> ProviderUsageSnapshot {
    if credential_fetch_unsupported_reason(id).is_some() {
        let mut error = unsupported_credential_snapshot(id, metadata, credential);
        error.fetch_duration_ms = snapshot.fetch_duration_ms;
        error
    } else {
        snapshot.decorate_for_credential(credential)
    }
}

fn record_provider_fetch_duration(
    id: ProviderId,
    snapshot: &mut ProviderUsageSnapshot,
    started: std::time::Instant,
) {
    let fetch_duration_ms = started.elapsed().as_millis();
    snapshot.fetch_duration_ms = Some(fetch_duration_ms);
    if fetch_duration_ms > 5_000 {
        tracing::warn!(
            provider = id.cli_name(),
            fetch_duration_ms,
            "slow provider refresh"
        );
    }
}

async fn await_provider_refreshes(
    handles: Vec<ProviderRefreshHandle>,
) -> Vec<ProviderRefreshOutcome> {
    let mut outcomes = Vec::with_capacity(handles.len());
    for refresh in handles {
        let outcome = refresh.handle.await.unwrap_or_else(|error| {
            tracing::warn!(
                provider = refresh.provider_id.cli_name(),
                credential_scoped = refresh.credential_id.is_some(),
                error = %error,
                "provider refresh task failed"
            );
            ProviderRefreshOutcome::new(refresh.provider_id, refresh.credential_id, false)
        });
        outcomes.push(outcome);
    }
    outcomes
}

/// Finish a refresh batch. Returns `None` when `generation` was superseded
/// (do not emit complete / tray updates for dead work). Returns `Some(error_count)`
/// when this batch still owns the generation and the lock was released.
fn finish_provider_refresh(
    state: &tauri::State<'_, Mutex<AppState>>,
    generation: u64,
    outcomes: &[ProviderRefreshOutcome],
    provider_order: &[ProviderId],
) -> Result<Option<ProviderRefreshSummary>, String> {
    let mut guard = state.lock().map_err(|e| e.to_string())?;
    if !is_current_provider_refresh_generation(&guard, generation) {
        // A newer begin or invalidate owns the lock/generation. Do not clear
        // is_refreshing — that would race a live successor batch.
        return Ok(None);
    }
    guard.is_refreshing = false;
    guard.provider_refresh_started_at = None;
    sort_provider_snapshots(&mut guard.provider_cache, provider_order);
    guard.provider_cache_updated_at = Some(std::time::Instant::now());
    Ok(Some(summarize_provider_refresh_outcomes(outcomes)))
}

fn update_tray_and_notifications(
    app: &tauri::AppHandle,
    state: &tauri::State<'_, Mutex<AppState>>,
    settings: &Settings,
    token_accounts: &HashMap<ProviderId, ProviderAccountData>,
) -> Result<(), String> {
    let cached = {
        let guard = state.lock().map_err(|e| e.to_string())?;
        guard.provider_cache.clone()
    };
    crate::tray_bridge::update_tray_status_items(app, &cached);
    crate::tray_bridge::update_tray_icon_and_tooltip(app, &cached);
    notify_usage_thresholds(state, settings, token_accounts, &cached);
    Ok(())
}

fn notify_usage_thresholds(
    state: &tauri::State<'_, Mutex<AppState>>,
    settings: &Settings,
    token_accounts: &HashMap<ProviderId, ProviderAccountData>,
    cached: &[ProviderUsageSnapshot],
) {
    let cli_map = codexbar::core::cli_name_map();
    if let Ok(mut guard) = state.lock() {
        for snapshot in cached {
            if snapshot.error.is_none()
                && let Some(&provider) = cli_map.get(snapshot.provider_id.as_str())
            {
                let token_account_id = token_accounts
                    .get(&provider)
                    .and_then(ProviderAccountData::active_account)
                    .map(|account| account.id);
                let account = quota_notification_account_identity(snapshot, token_account_id);
                // Skip session notifications for synthetic/no-session placeholders
                // (e.g. Claude web five_hour: null → informational 5h 0%).
                if !snapshot.primary.is_informational {
                    guard.notification_manager.check_and_notify(
                        provider,
                        &account,
                        "session",
                        snapshot.primary.used_percent,
                        settings,
                    );
                    guard.notification_manager.check_session_transition(
                        provider,
                        &account,
                        snapshot.primary.used_percent,
                        settings,
                    );
                    dispatch_quota_hooks(
                        settings,
                        provider,
                        &account,
                        "session",
                        snapshot.primary.used_percent,
                    );
                }
                if let Some(weekly) = &snapshot.secondary
                    && !weekly.is_informational
                {
                    guard.notification_manager.check_and_notify(
                        provider,
                        &account,
                        "weekly",
                        weekly.used_percent,
                        settings,
                    );
                    dispatch_quota_hooks(
                        settings,
                        provider,
                        &account,
                        "weekly",
                        weekly.used_percent,
                    );
                }
                notify_predictive_pace(
                    &mut guard.notification_manager,
                    provider,
                    snapshot,
                    token_accounts,
                    settings,
                );
            }
        }
    }
}

fn dispatch_quota_hooks(
    settings: &Settings,
    provider: ProviderId,
    account: &str,
    window: &str,
    used_percent: f64,
) {
    if !settings.hooks_enabled {
        return;
    }
    let thresholds = settings.usage_thresholds(provider, window);
    let account = (!settings.hide_personal_info && !account.is_empty()).then_some(account);
    codexbar::core::emit_quota_threshold_hooks(
        true,
        provider.cli_name(),
        window,
        used_percent,
        thresholds.high,
        thresholds.critical,
        account,
    );
}

/// Stable account discriminator for threshold/session toast dedupe.
/// Prefer app-managed credential id, then token-account id, email, and org.
fn quota_notification_account_identity(
    snapshot: &ProviderUsageSnapshot,
    token_account_id: Option<uuid::Uuid>,
) -> String {
    if let Some(id) = snapshot.credential_id {
        return format!("api-key:{}", id.as_hyphenated());
    }
    if let Some(id) = token_account_id {
        return format!("token-account:{}", id.as_hyphenated());
    }
    if let Some(email) = snapshot
        .account_email
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return email.to_ascii_lowercase();
    }
    if let Some(org) = snapshot
        .account_organization
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return format!("org:{}", org.to_ascii_lowercase());
    }
    // Do not fall back to plan_name/login_method — those are display tiers and
    // flicker across refreshes, re-arming still-hot windows for a new identity.
    String::new()
}

fn notify_predictive_pace(
    manager: &mut codexbar::notifications::NotificationManager,
    provider: ProviderId,
    snapshot: &ProviderUsageSnapshot,
    token_accounts: &HashMap<ProviderId, ProviderAccountData>,
    settings: &Settings,
) {
    let enabled = settings.show_notifications && settings.predictive_pace_warning_enabled;
    manager.set_predictive_warnings_enabled(provider, enabled);
    if !enabled || !matches!(provider, ProviderId::Claude | ProviderId::Codex) {
        return;
    }

    let token_account_id = token_accounts
        .get(&provider)
        .and_then(ProviderAccountData::active_account)
        .map(|account| account.id);
    let Some(identity) = predictive_warning_identity(
        provider,
        &snapshot.source_label,
        snapshot.account_email.as_deref(),
        token_account_id,
    ) else {
        return;
    };
    let observed_at = chrono::DateTime::parse_from_rfc3339(&snapshot.updated_at)
        .ok()
        .map(|date| date.with_timezone(&chrono::Utc));

    for (warning_window, window, default_window_minutes) in [
        (
            codexbar::notifications::PredictiveWarningWindow::Session,
            Some(&snapshot.primary),
            300,
        ),
        (
            codexbar::notifications::PredictiveWarningWindow::Weekly,
            snapshot.secondary.as_ref(),
            10080,
        ),
    ] {
        let Some(window) = window else {
            continue;
        };
        if window.is_informational {
            continue;
        }
        let rate_window = RateWindow::with_details(
            window.used_percent,
            window.window_minutes,
            window
                .resets_at
                .as_deref()
                .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                .map(|date| date.with_timezone(&chrono::Utc)),
            window.reset_description.clone(),
        );
        let Some(pace) =
            codexbar::core::UsagePace::weekly(&rate_window, observed_at, default_window_minutes)
        else {
            continue;
        };
        manager.check_predictive_pace(
            provider,
            &identity,
            warning_window,
            &rate_window,
            &pace,
            settings,
        );
    }
}

fn predictive_warning_identity(
    provider: ProviderId,
    source_label: &str,
    account_email: Option<&str>,
    token_account_id: Option<uuid::Uuid>,
) -> Option<String> {
    if !matches!(provider, ProviderId::Claude | ProviderId::Codex) {
        return None;
    }
    if let Some(id) = token_account_id {
        return Some(format!("token-account:{}", id.as_hyphenated()));
    }
    let source = source_label.trim().to_ascii_lowercase();
    let account = account_email?.trim().to_ascii_lowercase();
    if source.is_empty() || account.is_empty() {
        return None;
    }
    Some(format!("{source}:{account}"))
}

#[tauri::command]
pub async fn refresh_providers(app: tauri::AppHandle) -> Result<(), String> {
    do_refresh_providers(&app).await
}

#[tauri::command]
pub async fn refresh_providers_if_stale(app: tauri::AppHandle) -> Result<(), String> {
    do_refresh_providers_if_stale(&app).await
}

#[tauri::command]
pub fn get_cached_providers(
    state: tauri::State<'_, Mutex<AppState>>,
) -> Vec<ProviderUsageSnapshot> {
    let (mut snapshots, settings) = state
        .lock()
        .map(|guard| (guard.provider_cache.clone(), guard.settings.clone()))
        .unwrap_or_else(|poisoned| {
            let guard = poisoned.into_inner();
            (guard.provider_cache.clone(), guard.settings.clone())
        });
    let spark_usage_visible = settings.codex_spark_usage_visible();
    for snapshot in &mut snapshots {
        super::filter_hidden_codex_spark_rows(snapshot, spark_usage_visible);
    }
    sort_provider_snapshots(&mut snapshots, &settings.get_enabled_provider_ids());

    snapshots
}

#[cfg(test)]
mod predictive_warning_tests {
    use super::*;

    #[test]
    fn predictive_warning_identity_keeps_claude_sources_and_token_accounts_separate() {
        let account_id = uuid::Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap();

        assert_eq!(
            predictive_warning_identity(
                ProviderId::Claude,
                "cli",
                Some("Person@Example.com"),
                None,
            )
            .as_deref(),
            Some("cli:person@example.com")
        );
        assert_eq!(
            predictive_warning_identity(
                ProviderId::Claude,
                "oauth",
                Some("Person@Example.com"),
                None,
            )
            .as_deref(),
            Some("oauth:person@example.com")
        );
        assert_eq!(
            predictive_warning_identity(
                ProviderId::Claude,
                "oauth",
                Some("Person@Example.com"),
                Some(account_id),
            )
            .as_deref(),
            Some("token-account:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa")
        );
    }

    #[test]
    fn predictive_warning_identity_skips_unidentified_accounts() {
        assert_eq!(
            predictive_warning_identity(ProviderId::Claude, "oauth", None, None),
            None
        );
        assert_eq!(
            predictive_warning_identity(ProviderId::Codex, "cli", Some("  "), None),
            None
        );
    }

    fn empty_snapshot() -> ProviderUsageSnapshot {
        let metadata = codexbar::core::instantiate_provider(ProviderId::Claude)
            .metadata()
            .clone();
        ProviderUsageSnapshot::from_error(ProviderId::Claude, &metadata, "unused".to_string())
    }

    #[test]
    fn quota_notification_account_identity_prefers_token_then_email() {
        let account_id = uuid::Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap();
        let mut snapshot = empty_snapshot();
        snapshot.account_email = Some("Person@Example.com".to_string());
        snapshot.account_organization = Some("Acme Org".to_string());
        snapshot.plan_name = Some("Pro".to_string());

        assert_eq!(
            quota_notification_account_identity(&snapshot, Some(account_id)),
            "token-account:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa"
        );
        assert_eq!(
            quota_notification_account_identity(&snapshot, None),
            "person@example.com"
        );

        snapshot.account_email = None;
        assert_eq!(
            quota_notification_account_identity(&snapshot, None),
            "org:acme org"
        );

        snapshot.account_organization = None;
        // plan_name/login_method is not a stable ownership key — fall through to "".
        assert_eq!(quota_notification_account_identity(&snapshot, None), "");

        snapshot.plan_name = None;
        assert_eq!(quota_notification_account_identity(&snapshot, None), "");
    }

    #[test]
    fn quota_notification_identity_prefers_app_credential_uuid() {
        let credential_id = uuid::Uuid::parse_str("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb").unwrap();
        let token_id = uuid::Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap();
        let mut snapshot = empty_snapshot();
        snapshot.credential_id = Some(credential_id);

        assert_eq!(
            quota_notification_account_identity(&snapshot, Some(token_id)),
            "api-key:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"
        );
    }
}

#[cfg(test)]
mod transient_refresh_tests {
    use super::*;

    #[test]
    fn retryable_api_key_timeout_keeps_its_own_last_good_quota() {
        let metadata = codexbar::core::instantiate_provider(ProviderId::Kimi)
            .metadata()
            .clone();
        let credential_id = uuid::Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap();
        let result = codexbar::core::ProviderFetchResult {
            usage: codexbar::core::UsageSnapshot::new(codexbar::core::RateWindow::new(42.0)),
            cost: None,
            wayfinder_usage: None,
            source_label: "Code API".to_string(),
        };
        let mut last_good =
            ProviderUsageSnapshot::from_fetch_result(ProviderId::Kimi, &metadata, &result);
        last_good.credential_id = Some(credential_id);
        last_good.credential_display_label = Some("Work".to_string());

        let mut timeout =
            ProviderUsageSnapshot::from_error(ProviderId::Kimi, &metadata, "Timeout".to_string());
        timeout.credential_id = Some(credential_id);
        timeout.credential_display_label = Some("Work".to_string());

        let mut state = AppState::new();
        state.provider_cache.push(last_good);

        let published = preserve_last_good_transient_failure_for_identity(
            &mut state,
            ProviderId::Kimi,
            Some(credential_id),
            timeout,
        );

        assert_eq!(published.credential_id, Some(credential_id));
        assert_eq!(published.error, None);
        assert_eq!(published.primary.used_percent, 42.0);
    }

    #[test]
    fn retryable_api_key_timeout_marks_the_preserved_quota_as_stale() {
        let metadata = codexbar::core::instantiate_provider(ProviderId::Kimi)
            .metadata()
            .clone();
        let credential_id = uuid::Uuid::parse_str("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb").unwrap();
        let result = codexbar::core::ProviderFetchResult {
            usage: codexbar::core::UsageSnapshot::new(codexbar::core::RateWindow::new(42.0)),
            cost: None,
            wayfinder_usage: None,
            source_label: "Code API".to_string(),
        };
        let mut last_good =
            ProviderUsageSnapshot::from_fetch_result(ProviderId::Kimi, &metadata, &result);
        last_good.credential_id = Some(credential_id);

        let mut timeout =
            ProviderUsageSnapshot::from_error(ProviderId::Kimi, &metadata, "Timeout".to_string());
        timeout.credential_id = Some(credential_id);

        let mut state = AppState::new();
        state.provider_cache.push(last_good);
        let published = preserve_last_good_transient_failure_for_identity(
            &mut state,
            ProviderId::Kimi,
            Some(credential_id),
            timeout,
        );

        let bridge = serde_json::to_value(published).expect("serialize provider bridge");
        assert_eq!(
            bridge.get("refreshError").and_then(serde_json::Value::as_str),
            Some("Timeout")
        );
    }

    #[test]
    fn retryable_api_key_network_failure_keeps_its_last_good_quota() {
        let metadata = codexbar::core::instantiate_provider(ProviderId::Kimi)
            .metadata()
            .clone();
        let credential_id = uuid::Uuid::parse_str("cccccccc-cccc-cccc-cccc-cccccccccccc").unwrap();
        let result = codexbar::core::ProviderFetchResult {
            usage: codexbar::core::UsageSnapshot::new(codexbar::core::RateWindow::new(42.0)),
            cost: None,
            wayfinder_usage: None,
            source_label: "Code API".to_string(),
        };
        let mut last_good =
            ProviderUsageSnapshot::from_fetch_result(ProviderId::Kimi, &metadata, &result);
        last_good.credential_id = Some(credential_id);

        let mut failure = ProviderUsageSnapshot::from_error(
            ProviderId::Kimi,
            &metadata,
            "Network error: connection reset by peer".to_string(),
        );
        failure.credential_id = Some(credential_id);

        let mut state = AppState::new();
        state.provider_cache.push(last_good);
        let published = preserve_last_good_transient_failure_for_identity(
            &mut state,
            ProviderId::Kimi,
            Some(credential_id),
            failure,
        );

        assert_eq!(published.error, None);
        assert_eq!(published.primary.used_percent, 42.0);
    }

    #[test]
    fn retryable_api_key_http_status_failures_keep_the_last_good_quota() {
        let metadata = codexbar::core::instantiate_provider(ProviderId::Kimi)
            .metadata()
            .clone();

        for (credential_id, failure_message) in [
            (
                "dddddddd-dddd-dddd-dddd-dddddddddddd",
                "Kimi Code API returned status 429",
            ),
            (
                "eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee",
                "Kimi Code API returned status 503",
            ),
        ] {
            let credential_id = uuid::Uuid::parse_str(credential_id).unwrap();
            let result = codexbar::core::ProviderFetchResult {
                usage: codexbar::core::UsageSnapshot::new(codexbar::core::RateWindow::new(42.0)),
                cost: None,
                wayfinder_usage: None,
                source_label: "Code API".to_string(),
            };
            let mut last_good =
                ProviderUsageSnapshot::from_fetch_result(ProviderId::Kimi, &metadata, &result);
            last_good.credential_id = Some(credential_id);

            let mut failure = ProviderUsageSnapshot::from_error(
                ProviderId::Kimi,
                &metadata,
                failure_message.to_string(),
            );
            failure.credential_id = Some(credential_id);

            let mut state = AppState::new();
            state.provider_cache.push(last_good);
            let published = preserve_last_good_transient_failure_for_identity(
                &mut state,
                ProviderId::Kimi,
                Some(credential_id),
                failure,
            );

            assert_eq!(published.error, None, "{failure_message}");
            assert_eq!(published.primary.used_percent, 42.0, "{failure_message}");
        }
    }

    #[test]
    fn preserved_quota_uses_a_short_failure_retry_backoff() {
        let metadata = codexbar::core::instantiate_provider(ProviderId::Kimi)
            .metadata()
            .clone();
        let mut stale =
            ProviderUsageSnapshot::from_error(ProviderId::Kimi, &metadata, "Timeout".to_string());
        stale.error = None;
        stale.refresh_error = Some("Timeout".to_string());

        let mut state = AppState::new();
        state.provider_cache.push(stale);
        state.provider_cache_updated_at = Some(
            std::time::Instant::now() - std::time::Duration::from_secs(11),
        );

        assert!(
            !provider_cache_can_skip_refresh(&state, false),
            "a preserved transient failure must retry before the normal success cache window"
        );

        state.provider_cache_updated_at = Some(std::time::Instant::now());
        assert!(
            provider_cache_can_skip_refresh(&state, false),
            "failure retries must still be bounded when the user opens the UI repeatedly"
        );
    }

    #[test]
    fn retryable_api_key_rate_limit_message_keeps_the_last_good_quota() {
        let metadata = codexbar::core::instantiate_provider(ProviderId::Kimi)
            .metadata()
            .clone();
        let credential_id = uuid::Uuid::parse_str("ffffffff-ffff-ffff-ffff-ffffffffffff").unwrap();
        let result = codexbar::core::ProviderFetchResult {
            usage: codexbar::core::UsageSnapshot::new(codexbar::core::RateWindow::new(42.0)),
            cost: None,
            wayfinder_usage: None,
            source_label: "Code API".to_string(),
        };
        let mut last_good =
            ProviderUsageSnapshot::from_fetch_result(ProviderId::Kimi, &metadata, &result);
        last_good.credential_id = Some(credential_id);

        let mut failure = ProviderUsageSnapshot::from_error(
            ProviderId::Kimi,
            &metadata,
            "Rate limit exceeded; retry later".to_string(),
        );
        failure.credential_id = Some(credential_id);

        let mut state = AppState::new();
        state.provider_cache.push(last_good);
        let published = preserve_last_good_transient_failure_for_identity(
            &mut state,
            ProviderId::Kimi,
            Some(credential_id),
            failure,
        );

        assert_eq!(published.error, None);
        assert_eq!(published.primary.used_percent, 42.0);
    }

    #[test]
    fn authentication_failure_does_not_keep_a_stale_api_key_quota() {
        let metadata = codexbar::core::instantiate_provider(ProviderId::Kimi)
            .metadata()
            .clone();
        let credential_id = uuid::Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
        let result = codexbar::core::ProviderFetchResult {
            usage: codexbar::core::UsageSnapshot::new(codexbar::core::RateWindow::new(42.0)),
            cost: None,
            wayfinder_usage: None,
            source_label: "Code API".to_string(),
        };
        let mut last_good =
            ProviderUsageSnapshot::from_fetch_result(ProviderId::Kimi, &metadata, &result);
        last_good.credential_id = Some(credential_id);

        let mut authentication_failure = ProviderUsageSnapshot::from_error(
            ProviderId::Kimi,
            &metadata,
            "Authentication required".to_string(),
        );
        authentication_failure.credential_id = Some(credential_id);

        let mut state = AppState::new();
        state.provider_cache.push(last_good);
        let published = preserve_last_good_transient_failure_for_identity(
            &mut state,
            ProviderId::Kimi,
            Some(credential_id),
            authentication_failure,
        );

        assert_eq!(published.error.as_deref(), Some("Authentication required"));
        assert!(published.refresh_error.is_none());
    }
}
