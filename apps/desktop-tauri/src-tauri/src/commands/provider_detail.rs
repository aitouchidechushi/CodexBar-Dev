use super::*;

// ── Provider detail pane (Phase 6b) ──────────────────────────────────

/// DTO for the provider detail pane in the Settings Providers tab.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderDetail {
    pub id: String,
    pub display_name: String,
    pub enabled: bool,

    // Identity
    pub email: Option<String>,
    pub plan: Option<String>,
    pub auth_type: Option<String>,
    pub source_label: Option<String>,
    pub organization: Option<String>,
    pub last_updated: Option<String>,

    // Usage windows — reuse existing RateWindowSnapshot shape.
    pub session: Option<RateWindowSnapshot>,
    pub weekly: Option<RateWindowSnapshot>,
    pub model_specific: Option<RateWindowSnapshot>,
    pub tertiary: Option<RateWindowSnapshot>,
    pub extra_rate_windows: Vec<NamedRateWindowSnapshot>,

    // Cost / pace.
    pub cost: Option<CostSnapshotBridge>,
    pub pace: Option<PaceSnapshot>,

    // Error / state.
    pub last_error: Option<String>,

    // URLs for quick-actions (button visibility).
    pub dashboard_url: Option<String>,
    pub status_page_url: Option<String>,
    pub buy_credits_url: Option<String>,

    // True if the shared backend has produced any snapshot yet.
    pub has_snapshot: bool,
    pub credential_count: usize,
    pub failed_credential_count: usize,

    // Phase 6c — currently-persisted cookie source & region for round-tripping
    // into the settings UI pickers. `None` for providers that do not support
    // one of the pickers.
    pub cookie_source: Option<String>,
    pub region: Option<String>,
}

pub(crate) fn build_provider_detail(
    settings: &Settings,
    provider_id: &str,
) -> Result<ProviderDetail, String> {
    let id = parse_provider_arg(provider_id)?;

    let enabled = settings
        .enabled_providers
        .iter()
        .any(|p| p == id.cli_name());

    let provider = instantiate_provider(id);
    let metadata = provider.metadata();
    let dashboard_url = if id == codexbar::core::ProviderId::MiniMax {
        Some(
            codexbar::providers::MiniMaxProvider::dashboard_url_for_region(Some(
                settings.api_region(id),
            )),
        )
    } else {
        metadata.dashboard_url.map(|s| s.to_string())
    };

    Ok(ProviderDetail {
        id: id.cli_name().to_string(),
        display_name: id.display_name().to_string(),
        enabled,
        email: None,
        plan: None,
        auth_type: None,
        source_label: None,
        organization: None,
        last_updated: None,
        session: None,
        weekly: None,
        model_specific: None,
        tertiary: None,
        extra_rate_windows: Vec::new(),
        cost: None,
        pace: None,
        last_error: None,
        dashboard_url: dashboard_url.clone(),
        status_page_url: metadata.status_page_url.map(|s| s.to_string()),
        // Buy-credits currently mirrors the dashboard URL for providers that
        // support credit top-ups; refine once a dedicated URL lands upstream.
        buy_credits_url: if metadata.supports_credits {
            dashboard_url
        } else {
            None
        },
        has_snapshot: false,
        credential_count: 0,
        failed_credential_count: 0,
        cookie_source: provider_cookie_source_lookup(settings, id.cli_name()),
        region: provider_region_lookup(settings, id.cli_name()),
    })
}

#[tauri::command]
pub fn get_provider_detail(
    app: tauri::AppHandle,
    provider_id: String,
) -> Result<ProviderDetail, String> {
    let settings = crate::storage_services::settings_snapshot_from_app(&app);
    let mut detail = build_provider_detail(&settings, &provider_id)?;

    // Merge the latest cached snapshot, if any.
    let state = app.state::<Mutex<AppState>>();
    if let Ok(guard) = state.lock() {
        let snapshots = guard
            .provider_cache
            .iter()
            .filter(|snapshot| snapshot.provider_id == detail.id)
            .cloned()
            .collect::<Vec<_>>();
        apply_provider_snapshots_to_detail(
            &mut detail,
            &snapshots,
            settings.codex_spark_usage_visible(),
        );
    }

    Ok(detail)
}

fn apply_provider_snapshots_to_detail(
    detail: &mut ProviderDetail,
    snapshots: &[ProviderUsageSnapshot],
    spark_usage_visible: bool,
) {
    if snapshots.is_empty() {
        return;
    }
    let observed_credential_count = snapshots
        .iter()
        .filter(|snapshot| snapshot.credential_id.is_some())
        .count();
    detail.credential_count = snapshots
        .iter()
        .fold(observed_credential_count, |count, snapshot| {
            count.max(snapshot.credential_group_size.unwrap_or_default())
        });
    detail.failed_credential_count = snapshots
        .iter()
        .filter(|snapshot| snapshot.credential_id.is_some() && snapshot.error.is_some())
        .count();
    detail.has_snapshot = true;
    detail.last_updated = snapshots
        .iter()
        .map(|snapshot| snapshot.updated_at.clone())
        .max();
    if detail.credential_count > 1 {
        detail.email = None;
        detail.plan = None;
        detail.organization = None;
        detail.source_label = Some("api".to_string());
        detail.session = None;
        detail.weekly = None;
        detail.model_specific = None;
        detail.tertiary = None;
        detail.extra_rate_windows.clear();
        detail.cost = None;
        detail.pace = None;
        detail.last_error = (detail.failed_credential_count == detail.credential_count)
            .then(|| "error".to_string());
        return;
    }
    if let Some(snap) = snapshots.first() {
        let mut snapshot = snap.clone();
        super::filter_hidden_codex_spark_rows(&mut snapshot, spark_usage_visible);
        detail.email = snapshot.account_email.clone();
        detail.plan = snapshot.plan_name.clone();
        detail.organization = snapshot.account_organization.clone();
        detail.source_label = if snapshot.source_label.is_empty() {
            None
        } else {
            Some(snapshot.source_label.clone())
        };
        detail.last_updated = Some(snapshot.updated_at.clone());
        if snapshot.error.is_none() {
            detail.session = Some(snapshot.primary.clone());
            detail.weekly = snapshot.secondary.clone();
            detail.model_specific = snapshot.model_specific.clone();
            detail.tertiary = snapshot.tertiary.clone();
            detail.extra_rate_windows = snapshot.extra_rate_windows.clone();
            detail.cost = snapshot.cost.clone();
            detail.pace = snapshot.pace.clone();
        }
        detail.last_error = snapshot.error.clone();
    }
}

#[tauri::command]
pub fn revoke_provider_credentials(
    app: tauri::AppHandle,
    provider_id: String,
) -> Result<(), String> {
    // Best-effort: drop every app-managed credential for this provider so the
    // caller can follow up with a fresh login or import. Missing entries are
    // silently ignored; only I/O errors propagate.
    let id = parse_provider_arg(&provider_id)?;
    let provider_id = id.cli_name();
    let storage = crate::storage_services::storage_snapshot_from_app(&app);

    let api_keys = storage
        .api_keys
        .mutate(|keys| {
            keys.remove(provider_id);
            Ok(keys.clone())
        })
        .map_err(crate::storage_services::command_store_error)?;

    let remaining_credential_result = (|| {
        storage
            .manual_cookies
            .mutate(|cookies| cookies.remove(provider_id))
            .map_err(crate::storage_services::command_store_error)?;
        storage
            .token_accounts
            .remove_provider(id)
            .map_err(crate::storage_services::command_store_error)?;
        Ok(())
    })();

    finalize_provider_revoke_after_key_save(remaining_credential_result, || {
        finish_credential_mutation(
            &app,
            provider_id,
            &api_keys,
            CredentialCacheMutation::ProviderRevoked,
        )
    })
}

#[cfg(test)]
mod multi_key_tests {
    use super::*;

    fn rate_window(used_percent: f64) -> RateWindowSnapshot {
        RateWindowSnapshot {
            used_percent,
            remaining_percent: 100.0 - used_percent,
            window_minutes: None,
            resets_at: Some("2026-07-23T00:00:00Z".to_string()),
            reset_description: Some("1h".to_string()),
            is_exhausted: false,
            is_informational: false,
            reserve_percent: None,
            reserve_description: None,
            reserve_eta_seconds: None,
            reserve_will_last_to_reset: false,
        }
    }

    fn snapshot(used: f64, credential_id: &str, error: Option<&str>) -> ProviderUsageSnapshot {
        ProviderUsageSnapshot {
            provider_id: "openrouter".to_string(),
            credential_id: Some(uuid::Uuid::parse_str(credential_id).unwrap()),
            credential_display_label: Some("safe label".to_string()),
            credential_display_ordinal: Some(1),
            credential_group_size: None,
            display_name: "OpenRouter".to_string(),
            primary: rate_window(used),
            primary_label: None,
            secondary: Some(rate_window(used + 1.0)),
            secondary_label: None,
            model_specific: None,
            tertiary: None,
            extra_rate_windows: Vec::new(),
            cost: Some(CostSnapshotBridge {
                used: 9.0,
                limit: Some(10.0),
                remaining: Some(1.0),
                currency_code: "USD".to_string(),
                period: "monthly".to_string(),
                resets_at: None,
                formatted_used: "$9.00".to_string(),
                formatted_limit: Some("$10.00".to_string()),
            }),
            plan_name: Some("secret plan".to_string()),
            account_email: Some("secret@example.com".to_string()),
            account_group_id: None,
            account_display_name: None,
            source_label: "api".to_string(),
            updated_at: "2026-07-22T00:00:00Z".to_string(),
            error: error.map(str::to_string),
            refresh_error: None,
            pace: None,
            account_organization: Some("secret org".to_string()),
            tray_status_label: None,
            fetch_duration_ms: None,
            wayfinder_usage: None,
        }
    }

    #[test]
    fn multi_key_detail_is_neutral_for_success_and_partial_failure() {
        let mut detail = build_provider_detail(&Settings::default(), "openrouter").unwrap();
        let snapshots = vec![
            snapshot(91.0, "00000000-0000-0000-0000-000000000001", None),
            snapshot(
                12.0,
                "00000000-0000-0000-0000-000000000002",
                Some("secret sibling failure"),
            ),
        ];

        apply_provider_snapshots_to_detail(&mut detail, &snapshots, true);

        assert_eq!(detail.credential_count, 2);
        assert_eq!(detail.failed_credential_count, 1);
        assert!(detail.session.is_none());
        assert!(detail.weekly.is_none());
        assert!(detail.cost.is_none());
        assert!(detail.email.is_none());
        assert!(detail.plan.is_none());
        assert!(detail.organization.is_none());
        assert!(detail.last_error.is_none());
        assert!(detail.has_snapshot);
    }

    #[test]
    fn all_failed_multi_key_detail_has_one_safe_provider_error() {
        let mut detail = build_provider_detail(&Settings::default(), "openrouter").unwrap();
        let snapshots = vec![
            snapshot(
                0.0,
                "00000000-0000-0000-0000-000000000001",
                Some("first raw failure"),
            ),
            snapshot(
                0.0,
                "00000000-0000-0000-0000-000000000002",
                Some("second raw failure"),
            ),
        ];

        apply_provider_snapshots_to_detail(&mut detail, &snapshots, true);

        assert_eq!(detail.credential_count, 2);
        assert_eq!(detail.failed_credential_count, 2);
        assert_eq!(detail.last_error.as_deref(), Some("error"));
        assert!(!format!("{:?}", detail.last_error).contains("raw failure"));
    }
}

pub(crate) fn finalize_provider_revoke_after_key_save(
    remaining_credential_result: Result<(), String>,
    invalidate_saved_key: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let invalidation_result = invalidate_saved_key();
    match remaining_credential_result {
        Err(error) => Err(error),
        Ok(()) => invalidation_result,
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialStorageStatusBridge {
    pub manual_cookies: String,
    pub api_keys: String,
    pub token_accounts: String,
}

pub(crate) fn credential_file_status_label(status: SecureFileStatus) -> String {
    match status {
        SecureFileStatus::Missing => "missing".to_string(),
        SecureFileStatus::Plaintext => "plaintext".to_string(),
        SecureFileStatus::Protected(protection) => format!("protected:{protection}"),
        SecureFileStatus::Unreadable(_) => "unreadable".to_string(),
    }
}

#[tauri::command]
pub fn get_credential_storage_status(
    state: tauri::State<'_, Mutex<AppState>>,
) -> CredentialStorageStatusBridge {
    let storage = crate::storage_services::storage_snapshot(&state);
    CredentialStorageStatusBridge {
        manual_cookies: credential_file_status_label(secure_file::status(
            storage.manual_cookies.path(),
        )),
        api_keys: credential_file_status_label(secure_file::status(storage.api_keys.path())),
        token_accounts: credential_file_status_label(secure_file::status(
            storage.token_accounts.path(),
        )),
    }
}
