use std::collections::HashMap;
use std::sync::Mutex;

use super::{
    NamedRateWindowSnapshot, ProviderSummary, ProviderUsageSnapshot, provider_cookie_source_lookup,
    provider_region_lookup, validate_external_url, validate_surface_target,
};
use crate::state::AppState;
use crate::surface::SurfaceMode;
use crate::surface_target::SurfaceTarget;
use codexbar::core::{
    FetchContext, ProviderAccountData, ProviderFetchResult, ProviderId, SourceMode, TokenAccount,
    instantiate_provider,
};
use codexbar::host::session::launch_block_reason;
use codexbar::settings::{ApiKeys, Language, ManualCookies, Settings};

#[test]
fn validate_surface_target_accepts_matching_target() {
    let target = validate_surface_target(
        SurfaceMode::Settings,
        SurfaceTarget::Settings {
            tab: "apiKeys".into(),
        },
    )
    .unwrap();

    assert_eq!(
        target,
        SurfaceTarget::Settings {
            tab: "apiKeys".into()
        }
    );
}

#[test]
fn kimi_partial_quota_bridge_labels_follow_actual_duration() {
    let metadata = instantiate_provider(ProviderId::Kimi).metadata().clone();
    let result = ProviderFetchResult::new(
        codexbar::core::UsageSnapshot::new(codexbar::core::RateWindow::with_details(
            45.0,
            Some(300),
            None,
            None,
        )),
        "code-api",
    );
    let snapshot = ProviderUsageSnapshot::from_fetch_result(ProviderId::Kimi, &metadata, &result);
    assert_eq!(snapshot.primary_label.as_deref(), Some("Rate Limit"));
    assert_eq!(snapshot.primary.window_minutes, Some(300));
    assert!(snapshot.secondary.is_none());
    assert!(snapshot.secondary_label.is_none());
    let result = ProviderFetchResult::new(
        codexbar::core::UsageSnapshot::new(codexbar::core::RateWindow::with_details(
            20.0,
            Some(1440),
            None,
            None,
        )),
        "code-api",
    );
    let snapshot = ProviderUsageSnapshot::from_fetch_result(ProviderId::Kimi, &metadata, &result);
    assert_eq!(snapshot.primary_label.as_deref(), Some("Quota"));
    let cli = ProviderFetchResult::new(
        codexbar::core::UsageSnapshot::new(codexbar::core::RateWindow::with_details(
            20.0,
            Some(1440),
            None,
            None,
        )),
        "code-cli",
    );
    let snapshot = ProviderUsageSnapshot::from_fetch_result(ProviderId::Kimi, &metadata, &cli);
    assert_eq!(snapshot.primary_label.as_deref(), Some("Quota"));
    let web = ProviderFetchResult::new(
        codexbar::core::UsageSnapshot::new(codexbar::core::RateWindow::new(20.0)),
        "web",
    );
    let snapshot = ProviderUsageSnapshot::from_fetch_result(ProviderId::Kimi, &metadata, &web);
    assert_eq!(snapshot.primary_label.as_deref(), Some("Weekly"));
}

#[test]
fn validate_surface_target_rejects_mismatched_target() {
    let error = validate_surface_target(
        SurfaceMode::TrayPanel,
        SurfaceTarget::Settings {
            tab: "apiKeys".into(),
        },
    )
    .unwrap_err();

    assert!(error.contains("not valid for mode 'trayPanel'"));
}

#[test]
fn validate_surface_target_rejects_hidden_mode() {
    let error = validate_surface_target(SurfaceMode::Hidden, SurfaceTarget::Summary).unwrap_err();

    assert!(error.contains("only supports visible surfaces"));
}

#[test]
fn external_url_validation_allows_only_http_urls() {
    assert_eq!(
        validate_external_url(" https://github.com/Finesssee/Win-CodexBar ").unwrap(),
        "https://github.com/Finesssee/Win-CodexBar"
    );
    assert_eq!(
        validate_external_url("http://codexbar.app").unwrap(),
        "http://codexbar.app"
    );

    for invalid in [
        "",
        "file:///etc/passwd",
        "javascript:alert(1)",
        "https://bad\nhost",
    ] {
        assert!(
            validate_external_url(invalid).is_err(),
            "accepted invalid URL: {invalid:?}"
        );
    }
}

#[test]
fn credential_status_labels_do_not_include_error_details() {
    assert_eq!(
        super::credential_file_status_label(codexbar::secure_file::SecureFileStatus::Missing),
        "missing"
    );
    assert_eq!(
        super::credential_file_status_label(codexbar::secure_file::SecureFileStatus::Plaintext),
        "plaintext"
    );
    assert_eq!(
        super::credential_file_status_label(codexbar::secure_file::SecureFileStatus::Protected(
            "windows-dpapi-user".to_string(),
        )),
        "protected:windows-dpapi-user"
    );
    assert_eq!(
        super::credential_file_status_label(codexbar::secure_file::SecureFileStatus::Unreadable(
            "secret path / token".to_string(),
        )),
        "unreadable"
    );
}

#[test]
fn command_inputs_reject_invalid_provider_ids_before_storage_writes() {
    assert!(super::api_key_provider_arg("not-a-provider").is_err());
    assert!(super::validate_manual_cookie_input("not-a-provider", "a=b").is_err());
    assert!(super::api_key_provider_arg("bad\nprovider").is_err());
    assert!(super::canonical_provider_arg("").is_err());
}

#[test]
fn command_inputs_reject_multiline_secrets() {
    assert!(
        super::validate_single_line_secret("sk-test\nnext", "API key", super::MAX_API_KEY_LEN)
            .is_err()
    );
    assert!(super::validate_manual_cookie_input("codex", "a=b\nc=d").is_err());
}

#[test]
fn multi_key_commands_validate_provider_secret_label_and_credential_id() {
    assert!(super::api_key_provider_arg("claude").is_err());
    assert!(super::api_key_provider_arg("alibaba").is_err());
    assert!(super::api_key_provider_arg("grok").is_err());
    assert_eq!(
        super::api_key_provider_arg("ollama").as_deref(),
        Ok("ollama")
    );
    assert!(super::validate_single_line_secret("   ", "API key", super::MAX_API_KEY_LEN).is_err());
    assert!(super::validate_single_line_secret("a\nb", "API key", super::MAX_API_KEY_LEN).is_err());
    assert!(super::sanitize_optional_label(Some("x".repeat(81))).is_err());
    assert!(super::parse_credential_id("not-a-uuid").is_err());
}

#[test]
fn api_key_list_bridge_serialization_contains_no_secret_or_masked_key() {
    let mut keys = ApiKeys::default();
    keys.add("openrouter", "sk-super-secret-value", Some("Primary"))
        .unwrap();

    let payload = keys
        .get_all_for_display()
        .into_iter()
        .map(super::ApiKeyInfoBridge::from)
        .collect::<Vec<_>>();
    let json = serde_json::to_string(&payload).expect("serialize bridge response");

    assert!(json.contains("Primary"));
    assert!(!json.contains("sk-super-secret-value"));
    assert!(!json.contains("maskedKey"));
    assert!(!json.contains("super"));
}

#[test]
fn selected_api_key_secret_requires_matching_provider_and_credential() {
    let mut keys = ApiKeys::default();
    let openrouter_id = keys.add("openrouter", "openrouter-secret", None).unwrap();
    let zai_id = keys.add("zai", "zai-secret", None).unwrap();

    assert_eq!(
        super::selected_api_key_secret(&keys, "openrouter", openrouter_id).as_deref(),
        Ok("openrouter-secret")
    );
    assert!(super::selected_api_key_secret(&keys, "openrouter", zai_id).is_err());
}

#[test]
fn command_inputs_reject_unknown_cookie_source_and_region_values() {
    assert!(super::validate_provider_cookie_source("codex", "browser").is_err());
    assert!(super::validate_provider_region("zai", "moon").is_err());
}

#[test]
fn apply_provider_order_dedupes_and_appends_unknown_canonical() {
    // Request only "codex" and "claude" — remaining canonical ids should
    // be appended after, preserving canonical order.
    let order =
        codexbar::settings::normalize_provider_order(&["codex".to_string(), "claude".to_string()]);
    assert_eq!(order[0], "codex");
    assert_eq!(order[1], "claude");
    // Every canonical id appears exactly once.
    let mut sorted = order.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), order.len());
    // Every canonical id is present.
    let canonical = codexbar::core::ProviderId::all()
        .iter()
        .map(|p| p.cli_name().to_string())
        .collect::<Vec<_>>();
    for id in &canonical {
        assert!(order.contains(id), "missing canonical id: {id}");
    }
}

#[test]
fn apply_provider_order_ignores_unknown_ids() {
    let order = codexbar::settings::normalize_provider_order(&[
        "not-a-provider".to_string(),
        "codex".to_string(),
    ]);
    assert_eq!(order[0], "codex");
    assert!(!order.iter().any(|id| id == "not-a-provider"));
}

#[test]
fn provider_summaries_hide_deprecated_defaults_and_keep_contiguous_order() {
    let active_len = codexbar::core::ProviderId::all()
        .iter()
        .filter(|provider| !provider.is_deprecated())
        .count();
    let s = Settings::default();
    let summaries: Vec<ProviderSummary> = super::build_provider_summaries(&s);
    assert_eq!(summaries.len(), active_len);
    assert!(summaries.iter().all(|summary| {
        ProviderId::from_cli_name(&summary.id).is_some_and(|provider| !provider.is_deprecated())
    }));
    // Index is assigned in emission order.
    for (i, s) in summaries.iter().enumerate() {
        assert_eq!(s.order, i as u32);
    }
}

#[test]
fn runtime_identity_command_serializes_the_actual_process_identity_only() {
    let state = Mutex::new(AppState::new());
    let payload = super::runtime_identity_from_state(&state).unwrap();
    let value = serde_json::to_value(&payload).unwrap();
    let json = serde_json::to_string(&payload).unwrap();

    assert!(payload.executable_path.is_absolute());
    assert_eq!(payload.pid, std::process::id());
    assert_eq!(payload.build.git_commit.len(), 40);
    assert_eq!(payload.build.short_commit.len(), 12);
    assert!(!payload.build.semantic_version.is_empty());
    assert!(!payload.process_started_at.is_empty());
    assert_eq!(payload.executable_sha256.len(), 64);
    assert!(
        value
            .get("build")
            .and_then(|build| build.get("buildChannel"))
            .is_some()
    );
    for forbidden in ["apiKey", "cookie", "token", "secret", "settings"] {
        assert!(!json.contains(forbidden));
    }
}

#[test]
fn explicitly_enabled_deprecated_providers_remain_visible_for_legacy_settings() {
    let settings = Settings {
        enabled_providers: ["kimik2", "crossmodel"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        ..Settings::default()
    };

    let catalog_ids = super::provider_catalog_for(&settings)
        .into_iter()
        .map(|provider| provider.id)
        .collect::<std::collections::HashSet<_>>();
    let summary_ids = super::build_provider_summaries(&settings)
        .into_iter()
        .map(|provider| provider.id)
        .collect::<std::collections::HashSet<_>>();

    for legacy_id in ["kimik2", "crossmodel"] {
        assert!(catalog_ids.contains(legacy_id));
        assert!(summary_ids.contains(legacy_id));
    }
}

#[test]
fn provider_catalog_preserves_partial_config_order() {
    let settings = Settings {
        provider_order: codexbar::settings::normalize_provider_order(&[
            "gemini".to_string(),
            "claude".to_string(),
            "codex".to_string(),
        ]),
        ..Settings::default()
    };

    let catalog = super::provider_catalog_for(&settings);

    assert_eq!(
        catalog
            .iter()
            .take(3)
            .map(|provider| provider.id.as_str())
            .collect::<Vec<_>>(),
        vec!["gemini", "claude", "codex"]
    );
}

#[test]
fn settings_snapshot_preserves_partial_config_order_for_enabled_providers() {
    let settings = Settings {
        enabled_providers: ["gemini", "claude", "codex"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        provider_order: codexbar::settings::normalize_provider_order(&[
            "gemini".to_string(),
            "claude".to_string(),
            "codex".to_string(),
        ]),
        ..Settings::default()
    };

    let snapshot = serde_json::to_value(super::SettingsSnapshot::from(settings)).unwrap();

    assert_eq!(
        snapshot["providerOrder"]
            .as_array()
            .unwrap()
            .iter()
            .take(3)
            .map(|value| value.as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["gemini", "claude", "codex"],
    );
    assert_eq!(
        snapshot["enabledProviders"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["gemini", "claude", "codex"],
    );
}

#[test]
fn provider_cookie_source_lookup_roundtrips_known_providers() {
    let mut s = Settings::default();
    super::provider_cookie_source_set(&mut s, "codex", "cli-config".to_string()).unwrap();
    assert_eq!(
        provider_cookie_source_lookup(&s, "codex").as_deref(),
        Some("cli-config")
    );
    assert!(provider_cookie_source_lookup(&s, "unknown-provider").is_none());
}

#[test]
fn provider_region_lookup_roundtrips_known_providers() {
    let mut s = Settings::default();
    super::provider_region_set(&mut s, "alibaba", "china".to_string()).unwrap();
    assert_eq!(
        provider_region_lookup(&s, "alibaba").as_deref(),
        Some("china")
    );
    // Non-regional providers return None.
    assert!(provider_region_lookup(&s, "claude").is_none());
}

#[test]
fn glm_region_options_use_values_understood_by_the_quota_parser() {
    let values = super::region_options_for("zai")
        .into_iter()
        .map(|option| option.value)
        .collect::<Vec<_>>();

    assert_eq!(values, vec!["global", "cn"]);
}

#[test]
fn minimax_region_lookup_normalizes_legacy_china_value() {
    let mut s = Settings::default();
    super::provider_region_set(&mut s, "minimax", "china".to_string()).unwrap();
    assert_eq!(provider_region_lookup(&s, "minimax").as_deref(), Some("cn"));
}

#[test]
fn minimax_cookie_domain_follows_selected_region() {
    let mut s = Settings::default();
    assert_eq!(
        super::provider_cookie_domain(ProviderId::MiniMax, &s),
        Some("platform.minimaxi.com")
    );

    s.set_api_region(ProviderId::MiniMax, "global");
    assert_eq!(
        super::provider_cookie_domain(ProviderId::MiniMax, &s),
        Some("platform.minimax.io")
    );
}

#[test]
fn provider_cookie_source_set_rejects_unknown_provider() {
    let mut s = Settings::default();
    let err = super::provider_cookie_source_set(&mut s, "nope", "x".into()).unwrap_err();
    assert!(err.contains("nope"));
}

#[test]
fn fetch_context_defaults_to_manual_cookies_without_browser_import() {
    let settings = Settings::default();
    let cookies = ManualCookies::default();
    let api_keys = ApiKeys::default();
    let token_accounts = HashMap::new();

    let ctx = super::build_fetch_context(
        ProviderId::Cursor,
        &settings,
        &cookies,
        &api_keys,
        &token_accounts,
    );

    // Cursor does not support Cli; empty manual cookie remaps to Web (browser attempt).
    assert_eq!(ctx.source_mode, SourceMode::Web);
}

#[test]
fn fetch_context_cursor_cookie_off_stays_cli() {
    let mut settings = Settings::default();
    settings.set_cookie_source(ProviderId::Cursor, "off");
    let cookies = ManualCookies::default();
    let api_keys = ApiKeys::default();
    let token_accounts = HashMap::new();

    let ctx = super::build_fetch_context(
        ProviderId::Cursor,
        &settings,
        &cookies,
        &api_keys,
        &token_accounts,
    );

    // Explicit cookie-off keeps Cli (no browser scrape).
    assert_eq!(ctx.source_mode, SourceMode::Cli);
    assert!(ctx.manual_cookie_header.is_none());
}

#[test]
fn fetch_context_opencode_empty_manual_remaps_to_web() {
    let settings = Settings::default();
    let cookies = ManualCookies::default();
    let api_keys = ApiKeys::default();
    let token_accounts = HashMap::new();

    let ctx = super::build_fetch_context(
        ProviderId::OpenCode,
        &settings,
        &cookies,
        &api_keys,
        &token_accounts,
    );

    assert_eq!(ctx.source_mode, SourceMode::Web);
}

#[test]
fn fetch_context_claude_uses_oauth_without_manual_cookie() {
    let settings = Settings::default();
    let cookies = ManualCookies::default();
    let api_keys = ApiKeys::default();
    let token_accounts = HashMap::new();

    let ctx = super::build_fetch_context(
        ProviderId::Claude,
        &settings,
        &cookies,
        &api_keys,
        &token_accounts,
    );

    assert_eq!(ctx.source_mode, SourceMode::OAuth);
    assert!(ctx.manual_cookie_header.is_none());
}

#[test]
fn fetch_context_claude_explicit_cli_source_still_uses_cli() {
    let mut settings = Settings::default();
    settings.set_usage_source(ProviderId::Claude, "cli");
    let cookies = ManualCookies::default();
    let api_keys = ApiKeys::default();
    let token_accounts = HashMap::new();

    let ctx = super::build_fetch_context(
        ProviderId::Claude,
        &settings,
        &cookies,
        &api_keys,
        &token_accounts,
    );

    assert_eq!(ctx.source_mode, SourceMode::Cli);
    assert!(ctx.manual_cookie_header.is_none());
}

#[test]
fn fetch_context_manual_cookie_uses_web_without_browser_import() {
    let settings = Settings::default();
    let mut cookies = ManualCookies::default();
    cookies.set("cursor", "session=abc123");
    let api_keys = ApiKeys::default();
    let token_accounts = HashMap::new();

    let ctx = super::build_fetch_context(
        ProviderId::Cursor,
        &settings,
        &cookies,
        &api_keys,
        &token_accounts,
    );

    assert_eq!(ctx.source_mode, SourceMode::Web);
    assert_eq!(ctx.manual_cookie_header.as_deref(), Some("session=abc123"));
}

#[test]
fn fetch_context_api_key_provider_uses_auto_without_cookie_import() {
    let settings = Settings::default();
    let cookies = ManualCookies::default();
    let mut api_keys = ApiKeys::default();
    api_keys.set("deepseek", "sk-test", None);
    let token_accounts = HashMap::new();

    let ctx = super::build_fetch_context(
        ProviderId::DeepSeek,
        &settings,
        &cookies,
        &api_keys,
        &token_accounts,
    );

    assert_eq!(ctx.source_mode, SourceMode::Auto);
    assert!(ctx.manual_cookie_header.is_none());
    assert_eq!(ctx.api_key.as_deref(), Some("sk-test"));
}

#[test]
fn fetch_context_kimi_api_key_preserves_auto_for_web_fallback() {
    let settings = Settings::default();
    let cookies = ManualCookies::default();
    let mut api_keys = ApiKeys::default();
    api_keys.set("kimi", "sk-kimi-test", None);
    let token_accounts = HashMap::new();

    let ctx = super::build_fetch_context(
        ProviderId::Kimi,
        &settings,
        &cookies,
        &api_keys,
        &token_accounts,
    );

    assert_eq!(ctx.source_mode, SourceMode::Auto);
    assert!(ctx.manual_cookie_header.is_none());
    assert_eq!(ctx.api_key.as_deref(), Some("sk-kimi-test"));
}

#[test]
fn fetch_context_includes_minimax_region() {
    let mut settings = Settings::default();
    settings.set_api_region(ProviderId::MiniMax, "cn");
    let cookies = ManualCookies::default();
    let api_keys = ApiKeys::default();
    let token_accounts = HashMap::new();

    let ctx = super::build_fetch_context(
        ProviderId::MiniMax,
        &settings,
        &cookies,
        &api_keys,
        &token_accounts,
    );

    assert_eq!(ctx.api_region.as_deref(), Some("cn"));
}

#[test]
fn fetch_context_token_account_uses_web_cookie_header() {
    let settings = Settings::default();
    let cookies = ManualCookies::default();
    let api_keys = ApiKeys::default();
    let mut token_accounts = HashMap::new();
    let mut data = ProviderAccountData::new();
    data.add_account(TokenAccount::new("Work", "abc123"));
    token_accounts.insert(ProviderId::Ollama, data);

    let ctx = super::build_fetch_context(
        ProviderId::Ollama,
        &settings,
        &cookies,
        &api_keys,
        &token_accounts,
    );

    assert_eq!(ctx.source_mode, SourceMode::Web);
    assert_eq!(
        ctx.manual_cookie_header.as_deref(),
        Some("__Secure-session=abc123")
    );
}

#[test]
fn fetch_context_claude_oauth_token_account_uses_oauth() {
    let settings = Settings::default();
    let cookies = ManualCookies::default();
    let api_keys = ApiKeys::default();
    let mut token_accounts = HashMap::new();
    let mut data = ProviderAccountData::new();
    data.add_account(TokenAccount::new("Claude OAuth", "sk-ant-oat01-abc123"));
    token_accounts.insert(ProviderId::Claude, data);

    let ctx = super::build_fetch_context(
        ProviderId::Claude,
        &settings,
        &cookies,
        &api_keys,
        &token_accounts,
    );

    assert_eq!(ctx.source_mode, SourceMode::OAuth);
    assert!(ctx.manual_cookie_header.is_none());
    assert_eq!(ctx.api_key.as_deref(), Some("sk-ant-oat01-abc123"));
}

#[test]
fn fetch_context_copilot_token_account_uses_oauth_api_key() {
    let settings = Settings::default();
    let cookies = ManualCookies::default();
    let api_keys = ApiKeys::default();
    let mut token_accounts = HashMap::new();
    let mut data = ProviderAccountData::new();
    data.add_account(TokenAccount::new("GitHub", "gho_testtoken"));
    token_accounts.insert(ProviderId::Copilot, data);

    let ctx = super::build_fetch_context(
        ProviderId::Copilot,
        &settings,
        &cookies,
        &api_keys,
        &token_accounts,
    );

    assert_eq!(ctx.source_mode, SourceMode::OAuth);
    assert!(ctx.manual_cookie_header.is_none());
    assert_eq!(ctx.api_key.as_deref(), Some("gho_testtoken"));
}

#[test]
fn fetch_context_claude_session_token_account_uses_web_cookie() {
    let settings = Settings::default();
    let cookies = ManualCookies::default();
    let api_keys = ApiKeys::default();
    let mut token_accounts = HashMap::new();
    let mut data = ProviderAccountData::new();
    data.add_account(TokenAccount::new("Claude Web", "sessionKey=abc123"));
    token_accounts.insert(ProviderId::Claude, data);

    let ctx = super::build_fetch_context(
        ProviderId::Claude,
        &settings,
        &cookies,
        &api_keys,
        &token_accounts,
    );

    assert_eq!(ctx.source_mode, SourceMode::Web);
    assert_eq!(
        ctx.manual_cookie_header.as_deref(),
        Some("sessionKey=abc123")
    );
    assert!(ctx.api_key.is_none());
}

#[test]
fn fetch_context_token_account_takes_precedence_over_manual_cookie() {
    let settings = Settings::default();
    let mut cookies = ManualCookies::default();
    cookies.set("cursor", "manual=old");
    let api_keys = ApiKeys::default();
    let mut token_accounts = HashMap::new();
    let mut data = ProviderAccountData::new();
    data.add_account(TokenAccount::new("Work", "WorkosCursorSessionToken=new"));
    token_accounts.insert(ProviderId::Cursor, data);

    let ctx = super::build_fetch_context(
        ProviderId::Cursor,
        &settings,
        &cookies,
        &api_keys,
        &token_accounts,
    );

    assert_eq!(ctx.source_mode, SourceMode::Web);
    assert_eq!(
        ctx.manual_cookie_header.as_deref(),
        Some("WorkosCursorSessionToken=new")
    );
}

#[test]
fn fetch_context_openrouter_token_account_overrides_stored_api_key() {
    let settings = Settings::default();
    let cookies = ManualCookies::default();
    let mut api_keys = ApiKeys::default();
    api_keys.set("openrouter", "sk-or-v1-stored-decoy", None);
    let mut token_accounts = HashMap::new();
    let mut data = ProviderAccountData::new();
    data.add_account(TokenAccount::new("Personal", "sk-or-v1-personal"));
    data.add_account(TokenAccount::new("Work", "sk-or-v1-work"));
    data.set_active(1);
    token_accounts.insert(ProviderId::OpenRouter, data);

    let ctx = super::build_fetch_context(
        ProviderId::OpenRouter,
        &settings,
        &cookies,
        &api_keys,
        &token_accounts,
    );

    assert_eq!(ctx.source_mode, SourceMode::OAuth);
    assert!(ctx.manual_cookie_header.is_none());
    assert_eq!(ctx.api_key.as_deref(), Some("sk-or-v1-work"));
}

#[test]
fn fetch_context_openrouter_falls_back_to_stored_api_key_without_token_accounts() {
    let settings = Settings::default();
    let cookies = ManualCookies::default();
    let mut api_keys = ApiKeys::default();
    api_keys.set("openrouter", "sk-or-v1-stored", None);
    let token_accounts = HashMap::new();

    let ctx = super::build_fetch_context(
        ProviderId::OpenRouter,
        &settings,
        &cookies,
        &api_keys,
        &token_accounts,
    );

    assert_eq!(ctx.api_key.as_deref(), Some("sk-or-v1-stored"));
}

#[test]
fn refresh_jobs_pair_each_stored_credential_with_its_exact_secret_without_default_duplicate() {
    let settings = Settings::default();
    let cookies = ManualCookies::default();
    let mut api_keys = ApiKeys::default();
    let first_id = api_keys
        .add("openrouter", "sk-first-exact", Some("Personal"))
        .unwrap();
    let second_id = api_keys.add("openrouter", "sk-second-exact", None).unwrap();
    let mut token_accounts = HashMap::new();
    let mut accounts = ProviderAccountData::new();
    accounts.add_account(TokenAccount::new("Decoy", "sk-token-decoy"));
    token_accounts.insert(ProviderId::OpenRouter, accounts);

    let jobs = super::plan_provider_refresh_jobs(
        &[ProviderId::OpenRouter],
        &settings,
        &cookies,
        &api_keys,
        &token_accounts,
    );

    assert_eq!(jobs.len(), 2);
    assert_eq!(jobs[0].provider_id, ProviderId::OpenRouter);
    assert_eq!(jobs[0].ctx.api_key.as_deref(), Some("sk-first-exact"));
    assert_eq!(jobs[0].credential.as_ref().unwrap().id, first_id);
    assert_eq!(
        jobs[0].credential.as_ref().unwrap().display_label,
        "Personal"
    );
    assert_eq!(jobs[0].credential.as_ref().unwrap().display_ordinal, 1);
    assert_eq!(jobs[1].ctx.api_key.as_deref(), Some("sk-second-exact"));
    assert_eq!(jobs[1].credential.as_ref().unwrap().id, second_id);
    assert_eq!(jobs[1].credential.as_ref().unwrap().display_label, "Key 2");
    assert_eq!(jobs[1].credential.as_ref().unwrap().display_ordinal, 2);
    assert!(jobs.iter().all(|job| job.credential.is_some()));
}

#[test]
fn refresh_jobs_never_schedule_inactive_api_keys() {
    let settings = Settings::default();
    let cookies = ManualCookies::default();
    let mut api_keys = ApiKeys::default();
    let inactive_id = api_keys
        .add("openrouter", "sk-inactive-must-not-run", None)
        .unwrap();
    let active_id = api_keys
        .add("openrouter", "sk-active-may-run", None)
        .unwrap();
    api_keys
        .set_active("openrouter", inactive_id, false)
        .unwrap();

    let jobs = super::plan_provider_refresh_jobs(
        &[ProviderId::OpenRouter],
        &settings,
        &cookies,
        &api_keys,
        &HashMap::new(),
    );

    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].credential.as_ref().unwrap().id, active_id);
    assert_eq!(jobs[0].ctx.api_key.as_deref(), Some("sk-active-may-run"));

    api_keys.set_active("openrouter", active_id, false).unwrap();
    let all_inactive_jobs = super::plan_provider_refresh_jobs(
        &[ProviderId::OpenRouter],
        &settings,
        &cookies,
        &api_keys,
        &HashMap::new(),
    );

    assert_eq!(all_inactive_jobs.len(), 1);
    assert!(all_inactive_jobs[0].credential.is_none());
    assert!(all_inactive_jobs[0].ctx.api_key.is_none());
}

#[test]
fn refresh_jobs_preserve_one_default_job_when_no_stored_credentials_exist() {
    let settings = Settings::default();
    let cookies = ManualCookies::default();
    let api_keys = ApiKeys::default();
    let mut token_accounts = HashMap::new();
    let mut accounts = ProviderAccountData::new();
    accounts.add_account(TokenAccount::new("Work", "sk-token-default"));
    token_accounts.insert(ProviderId::OpenRouter, accounts);

    let jobs = super::plan_provider_refresh_jobs(
        &[ProviderId::OpenRouter],
        &settings,
        &cookies,
        &api_keys,
        &token_accounts,
    );

    assert_eq!(jobs.len(), 1);
    assert!(jobs[0].credential.is_none());
    assert_eq!(jobs[0].ctx.api_key.as_deref(), Some("sk-token-default"));
}

#[test]
fn fallback_capable_credential_jobs_select_strict_api_sources() {
    let settings = Settings::default();
    let cookies = ManualCookies::default();
    let mut api_keys = ApiKeys::default();
    api_keys.add("kimi", "kimi-selected", None).unwrap();
    api_keys.add("factory", "factory-selected", None).unwrap();
    api_keys.add("ollama", "ollama-selected", None).unwrap();
    api_keys.add("amp", "amp-selected", None).unwrap();

    let jobs = super::plan_provider_refresh_jobs(
        &[
            ProviderId::Kimi,
            ProviderId::Factory,
            ProviderId::Ollama,
            ProviderId::Amp,
        ],
        &settings,
        &cookies,
        &api_keys,
        &HashMap::new(),
    );

    assert_eq!(jobs.len(), 4);
    assert!(
        jobs[..3]
            .iter()
            .all(|job| job.ctx.source_mode == SourceMode::OAuth)
    );
    assert_eq!(jobs[3].ctx.source_mode, SourceMode::Web);
    assert!(
        jobs.iter()
            .all(|job| job.ctx.manual_cookie_header.is_none())
    );
}

#[test]
fn provider_region_set_rejects_non_regional_provider() {
    let mut s = Settings::default();
    let err = super::provider_region_set(&mut s, "claude", "global".into()).unwrap_err();
    assert!(err.contains("claude"));
}

#[test]
fn launch_block_reason_helper_returns_none_when_not_blocked() {
    assert!(launch_block_reason(false, false).is_none());
}

#[test]
fn launch_block_reason_helper_prefers_ssh() {
    let msg = launch_block_reason(true, true).unwrap();
    assert!(msg.contains("SSH"));
}

// ── Phase 6b — provider detail pane ────────────────────────────

#[test]
fn build_provider_detail_populates_identity_urls() {
    let detail =
        super::build_provider_detail(&Settings::default(), "claude").expect("known provider");
    assert_eq!(detail.id, "claude");
    assert_eq!(detail.display_name, "Claude");
    // Claude advertises a status page URL in its metadata.
    assert!(detail.status_page_url.is_some());
    // No snapshot yet — empty usage bars and no error.
    assert!(detail.session.is_none());
    assert!(detail.last_error.is_none());
    assert!(!detail.has_snapshot);
}

#[test]
fn build_provider_detail_rejects_unknown_provider() {
    let err = super::build_provider_detail(&Settings::default(), "not-a-provider").unwrap_err();
    assert!(err.contains("not-a-provider"));
}

#[test]
fn provider_detail_roundtrips_through_serde() {
    let detail =
        super::build_provider_detail(&Settings::default(), "codex").expect("known provider");
    let json = serde_json::to_string(&detail).expect("serialize");
    // camelCase rename survives the round-trip.
    assert!(json.contains("\"displayName\""));
    assert!(json.contains("\"hasSnapshot\""));
    assert!(json.contains("\"statusPageUrl\""));
}

#[test]
fn pace_stage_serializes_to_snake_case_string() {
    use codexbar::core::PaceStage;
    assert_eq!(super::pace_stage_str(PaceStage::OnTrack), "on_track");
    assert_eq!(
        super::pace_stage_str(PaceStage::SlightlyAhead),
        "slightly_ahead"
    );
    assert_eq!(super::pace_stage_str(PaceStage::FarAhead), "far_ahead");
    assert_eq!(
        super::pace_stage_str(PaceStage::SlightlyBehind),
        "slightly_behind"
    );
    assert_eq!(super::pace_stage_str(PaceStage::Behind), "behind");
    assert_eq!(super::pace_stage_str(PaceStage::FarBehind), "far_behind");
}

#[test]
fn provider_cache_is_fresh_inside_stale_window() {
    assert!(super::is_provider_cache_fresh(
        Some(std::time::Instant::now()),
        std::time::Duration::from_secs(30),
    ));
}

#[test]
fn provider_cache_is_stale_when_missing_timestamp() {
    assert!(!super::is_provider_cache_fresh(
        None,
        std::time::Duration::from_secs(30),
    ));
}

#[test]
fn provider_cache_is_stale_after_window() {
    assert!(!super::is_provider_cache_fresh(
        Some(std::time::Instant::now() - std::time::Duration::from_secs(31)),
        std::time::Duration::from_secs(30),
    ));
}

#[test]
fn provider_fetch_timeout_allows_slower_authenticated_providers() {
    let ctx = FetchContext {
        web_timeout: 30,
        ..FetchContext::default()
    };
    assert_eq!(
        super::provider_fetch_timeout(ProviderId::Claude, &ctx),
        std::time::Duration::from_secs(75)
    );
    assert_eq!(
        super::provider_fetch_timeout(ProviderId::Codex, &ctx),
        std::time::Duration::from_secs(75)
    );
    assert_eq!(
        super::provider_fetch_timeout(ProviderId::Copilot, &ctx),
        std::time::Duration::from_secs(75)
    );
    assert_eq!(
        super::provider_fetch_timeout(ProviderId::DeepSeek, &ctx),
        std::time::Duration::from_secs(35)
    );
}

#[test]
fn provider_fetch_timeout_respects_context_web_timeout_with_cap() {
    let ctx = FetchContext {
        web_timeout: 60,
        ..FetchContext::default()
    };
    assert_eq!(
        super::provider_fetch_timeout(ProviderId::T3Chat, &ctx),
        std::time::Duration::from_secs(65)
    );

    let ctx = FetchContext {
        web_timeout: 120,
        ..FetchContext::default()
    };
    assert_eq!(
        super::provider_fetch_timeout(ProviderId::AzureOpenAI, &ctx),
        std::time::Duration::from_secs(65)
    );
}

#[test]
fn provider_cache_upsert_replaces_only_the_same_composite_identity() {
    let metadata = instantiate_provider(ProviderId::Codex).metadata().clone();
    let result = ProviderFetchResult {
        usage: codexbar::core::UsageSnapshot::new(codexbar::core::RateWindow::new(10.0)),
        cost: None,
        wayfinder_usage: None,
        source_label: "CLI".to_string(),
    };
    let first_id = uuid::Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap();
    let second_id = uuid::Uuid::parse_str("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb").unwrap();
    let credential = super::CredentialSnapshotMeta::new(first_id, "Key 1".into(), 1);
    let sibling = super::CredentialSnapshotMeta::new(second_id, "Key 2".into(), 2);
    let mut first = ProviderUsageSnapshot::from_fetch_result(ProviderId::Codex, &metadata, &result)
        .decorate_for_credential(&credential);
    let mut second = first.clone();
    let sibling = ProviderUsageSnapshot::from_fetch_result(ProviderId::Codex, &metadata, &result)
        .decorate_for_credential(&sibling);
    first.error = Some("old".to_string());
    second.error = Some("new".to_string());

    let mut cache = vec![first, sibling];
    super::upsert_provider_cache(&mut cache, second);

    assert_eq!(cache.len(), 2);
    assert_eq!(cache[0].error.as_deref(), Some("new"));
    assert_eq!(cache[1].credential_id, Some(second_id));
}

#[test]
fn provider_cache_reconciliation_preserves_siblings_and_prunes_stale_identities() {
    let metadata = instantiate_provider(ProviderId::Codex).metadata().clone();
    let result = ProviderFetchResult {
        usage: codexbar::core::UsageSnapshot::new(codexbar::core::RateWindow::new(10.0)),
        cost: None,
        wayfinder_usage: None,
        source_label: "CLI".to_string(),
    };
    let kept_id = uuid::Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap();
    let stale_id = uuid::Uuid::parse_str("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb").unwrap();
    let kept = ProviderUsageSnapshot::from_fetch_result(ProviderId::Codex, &metadata, &result)
        .decorate_for_credential(&super::CredentialSnapshotMeta::new(
            kept_id,
            "Key 1".into(),
            1,
        ));
    let stale = ProviderUsageSnapshot::from_fetch_result(ProviderId::Codex, &metadata, &result)
        .decorate_for_credential(&super::CredentialSnapshotMeta::new(
            stale_id,
            "Key 2".into(),
            2,
        ));
    let obsolete_default =
        ProviderUsageSnapshot::from_fetch_result(ProviderId::Codex, &metadata, &result);

    let expected = vec![super::ProviderSnapshotIdentity::new(
        "codex".into(),
        Some(kept_id),
    )];
    let mut cache = vec![kept, stale, obsolete_default];
    super::reconcile_provider_cache(&mut cache, &expected);

    assert_eq!(cache.len(), 1);
    assert_eq!(cache[0].credential_id, Some(kept_id));
}

fn cached_openrouter_snapshot(
    credential_id: Option<uuid::Uuid>,
    label: Option<&str>,
    ordinal: Option<u64>,
) -> ProviderUsageSnapshot {
    let metadata = instantiate_provider(ProviderId::OpenRouter)
        .metadata()
        .clone();
    let result = ProviderFetchResult {
        usage: codexbar::core::UsageSnapshot::new(codexbar::core::RateWindow::new(10.0)),
        cost: None,
        wayfinder_usage: None,
        source_label: "API".to_string(),
    };
    let snapshot =
        ProviderUsageSnapshot::from_fetch_result(ProviderId::OpenRouter, &metadata, &result);
    match credential_id {
        Some(id) => snapshot.decorate_for_credential(&super::CredentialSnapshotMeta::new(
            id,
            label.unwrap_or("Key").to_string(),
            ordinal.unwrap_or(1),
        )),
        None => snapshot,
    }
}

#[test]
fn credential_add_removes_default_lane_and_preserves_sibling_cache() {
    let mut keys = ApiKeys::default();
    let sibling_id = keys.add("openrouter", "first", Some("First")).unwrap();
    let added_id = keys.add("openrouter", "second", Some("Second")).unwrap();
    let mut state = AppState::new();
    state.provider_cache = vec![
        cached_openrouter_snapshot(None, None, None),
        cached_openrouter_snapshot(Some(sibling_id), Some("First"), Some(1)),
    ];

    let should_refresh = super::apply_credential_mutation_result(
        &mut state,
        &[ProviderId::OpenRouter],
        &keys,
        "openrouter",
        Ok(super::CredentialCacheMutation::Added(added_id)),
    )
    .unwrap();

    assert!(should_refresh);
    assert_eq!(state.provider_cache.len(), 1);
    assert_eq!(state.provider_cache[0].credential_id, Some(sibling_id));
    assert_eq!(state.provider_cache[0].credential_group_size, Some(2));
}

#[test]
fn credential_secret_replacement_prunes_only_the_changed_uuid() {
    let mut keys = ApiKeys::default();
    let changed_id = keys.add("openrouter", "changed", Some("Changed")).unwrap();
    let sibling_id = keys.add("openrouter", "sibling", Some("Sibling")).unwrap();
    let mut state = AppState::new();
    state.provider_cache = vec![
        cached_openrouter_snapshot(Some(changed_id), Some("Changed"), Some(1)),
        cached_openrouter_snapshot(Some(sibling_id), Some("Sibling"), Some(2)),
    ];

    super::apply_credential_mutation_result(
        &mut state,
        &[ProviderId::OpenRouter],
        &keys,
        "openrouter",
        Ok(super::CredentialCacheMutation::SecretReplaced(changed_id)),
    )
    .unwrap();

    assert_eq!(state.provider_cache.len(), 1);
    assert_eq!(state.provider_cache[0].credential_id, Some(sibling_id));
    assert_eq!(state.provider_cache[0].credential_group_size, Some(2));
}

#[test]
fn credential_delete_prunes_target_and_preserves_sibling() {
    let deleted_id = uuid::Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap();
    let mut keys = ApiKeys::default();
    let sibling_id = keys.add("openrouter", "sibling", Some("Sibling")).unwrap();
    let mut state = AppState::new();
    state.provider_cache = vec![
        cached_openrouter_snapshot(Some(deleted_id), Some("Deleted"), Some(1)),
        cached_openrouter_snapshot(Some(sibling_id), Some("Sibling"), Some(2)),
    ];

    super::apply_credential_mutation_result(
        &mut state,
        &[ProviderId::OpenRouter],
        &keys,
        "openrouter",
        Ok(super::CredentialCacheMutation::Deleted(deleted_id)),
    )
    .unwrap();

    assert_eq!(state.provider_cache.len(), 1);
    assert_eq!(state.provider_cache[0].credential_id, Some(sibling_id));
}

#[test]
fn deleting_last_credential_restores_default_refresh_identity() {
    let deleted_id = uuid::Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap();
    let keys = ApiKeys::default();
    let mut state = AppState::new();
    state.provider_cache = vec![cached_openrouter_snapshot(
        Some(deleted_id),
        Some("Deleted"),
        Some(1),
    )];

    super::apply_credential_mutation_result(
        &mut state,
        &[ProviderId::OpenRouter],
        &keys,
        "openrouter",
        Ok(super::CredentialCacheMutation::Deleted(deleted_id)),
    )
    .unwrap();
    let identities = super::expected_provider_snapshot_identities(&[ProviderId::OpenRouter], &keys);

    assert!(state.provider_cache.is_empty());
    assert_eq!(identities.len(), 1);
    assert_eq!(identities[0].credential_id, None);
}

#[test]
fn credential_label_update_patches_cached_label_without_dropping_quota() {
    let mut keys = ApiKeys::default();
    let credential_id = keys.add("openrouter", "secret", Some("Renamed")).unwrap();
    let mut state = AppState::new();
    state.provider_cache = vec![cached_openrouter_snapshot(
        Some(credential_id),
        Some("Old label"),
        Some(1),
    )];

    super::apply_credential_mutation_result(
        &mut state,
        &[ProviderId::OpenRouter],
        &keys,
        "openrouter",
        Ok(super::CredentialCacheMutation::Relabeled(credential_id)),
    )
    .unwrap();

    assert_eq!(state.provider_cache.len(), 1);
    assert_eq!(
        state.provider_cache[0].credential_display_label.as_deref(),
        Some("Renamed")
    );
    assert_eq!(state.provider_cache[0].primary.used_percent, 10.0);
}

#[test]
fn credential_reorder_updates_cached_ordinals_without_refreshing_quota() {
    let mut keys = ApiKeys::default();
    let first = keys.add("openrouter", "first", None).unwrap();
    let second = keys.add("openrouter", "second", Some("Second")).unwrap();
    keys.reorder("openrouter", &[second, first]).unwrap();
    let mut state = AppState::new();
    state.provider_cache = vec![
        cached_openrouter_snapshot(Some(first), Some("Key 1"), Some(1)),
        cached_openrouter_snapshot(Some(second), Some("Second"), Some(2)),
    ];

    let should_refresh = super::apply_credential_mutation_result(
        &mut state,
        &[ProviderId::OpenRouter],
        &keys,
        "openrouter",
        Ok(super::CredentialCacheMutation::Reordered),
    )
    .unwrap();

    assert!(!should_refresh);
    assert_eq!(state.provider_cache[0].credential_id, Some(second));
    assert_eq!(state.provider_cache[0].credential_display_ordinal, Some(1));
    assert_eq!(state.provider_cache[1].credential_id, Some(first));
    assert_eq!(state.provider_cache[1].credential_display_ordinal, Some(2));
    assert_eq!(state.provider_cache[0].primary.used_percent, 10.0);
}

#[test]
fn provider_revoke_removes_every_provider_snapshot() {
    let credential_id = uuid::Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap();
    let keys = ApiKeys::default();
    let mut state = AppState::new();
    state.provider_cache = vec![
        cached_openrouter_snapshot(None, None, None),
        cached_openrouter_snapshot(Some(credential_id), Some("Key 1"), Some(1)),
    ];

    super::apply_credential_mutation_result(
        &mut state,
        &[ProviderId::OpenRouter],
        &keys,
        "openrouter",
        Ok(super::CredentialCacheMutation::ProviderRevoked),
    )
    .unwrap();

    assert!(state.provider_cache.is_empty());
}

#[test]
fn credential_mutation_advances_generation_releases_lock_and_rejects_old_publish() {
    let keys = ApiKeys::default();
    let mut state = AppState::new();
    state.provider_refresh_generation = 41;
    state.is_refreshing = true;
    state.provider_refresh_started_at = Some(std::time::Instant::now());

    super::apply_credential_mutation_result(
        &mut state,
        &[ProviderId::OpenRouter],
        &keys,
        "openrouter",
        Ok(super::CredentialCacheMutation::ProviderRevoked),
    )
    .unwrap();

    assert_eq!(state.provider_refresh_generation, 42);
    assert!(!state.is_refreshing);
    assert!(state.provider_refresh_started_at.is_none());
    assert!(!super::is_current_provider_refresh_generation(&state, 41));
}

#[test]
fn disabled_provider_mutation_does_not_request_refresh() {
    let keys = ApiKeys::default();
    let mut state = AppState::new();

    let should_refresh = super::apply_credential_mutation_result(
        &mut state,
        &[ProviderId::Codex],
        &keys,
        "openrouter",
        Ok(super::CredentialCacheMutation::ProviderRevoked),
    )
    .unwrap();

    assert!(!should_refresh);
    assert_eq!(state.provider_refresh_generation, 1);
}

#[test]
fn failed_credential_save_does_not_invalidate_state_or_request_refresh() {
    let keys = ApiKeys::default();
    let mut state = AppState::new();
    state.provider_refresh_generation = 9;
    state.is_refreshing = true;
    state.provider_cache = vec![cached_openrouter_snapshot(None, None, None)];

    let result = super::apply_credential_mutation_result(
        &mut state,
        &[ProviderId::OpenRouter],
        &keys,
        "openrouter",
        Err("save failed".to_string()),
    );

    assert_eq!(result.unwrap_err(), "save failed");
    assert_eq!(state.provider_refresh_generation, 9);
    assert!(state.is_refreshing);
    assert_eq!(state.provider_cache.len(), 1);
}

#[test]
fn provider_revoke_invalidates_saved_key_even_when_later_credential_save_fails() {
    let mut invalidated = false;

    let result = super::finalize_provider_revoke_after_key_save(
        Err("cookie save failed".to_string()),
        || {
            invalidated = true;
            Ok(())
        },
    );

    assert!(invalidated);
    assert_eq!(result.unwrap_err(), "cookie save failed");
}

#[test]
fn provider_cache_sort_is_deterministic_by_provider_then_credential_ordinal() {
    let metadata = instantiate_provider(ProviderId::Codex).metadata().clone();
    let result = ProviderFetchResult {
        usage: codexbar::core::UsageSnapshot::new(codexbar::core::RateWindow::new(10.0)),
        cost: None,
        wayfinder_usage: None,
        source_label: "API".to_string(),
    };
    let make = |provider, id, ordinal| {
        ProviderUsageSnapshot::from_fetch_result(provider, &metadata, &result)
            .decorate_for_credential(&super::CredentialSnapshotMeta::new(
                id,
                format!("Key {ordinal}"),
                ordinal,
            ))
    };
    let first_id = uuid::Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap();
    let second_id = uuid::Uuid::parse_str("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb").unwrap();
    let mut cache = vec![
        make(ProviderId::Codex, second_id, 2),
        make(ProviderId::Claude, first_id, 1),
        make(ProviderId::Codex, first_id, 1),
    ];

    super::sort_provider_snapshots(&mut cache, &[ProviderId::Codex, ProviderId::Claude]);

    assert_eq!(cache[0].provider_id, "codex");
    assert_eq!(cache[0].credential_display_ordinal, Some(1));
    assert_eq!(cache[1].provider_id, "codex");
    assert_eq!(cache[1].credential_display_ordinal, Some(2));
    assert_eq!(cache[2].provider_id, "claude");
}

#[test]
fn credential_snapshot_decoration_preserves_safe_account_metadata_without_secrets() {
    let metadata = instantiate_provider(ProviderId::OpenRouter)
        .metadata()
        .clone();
    let result = ProviderFetchResult {
        usage: codexbar::core::UsageSnapshot::new(codexbar::core::RateWindow::new(10.0))
            .with_email("secret@example.com")
            .with_organization("Secret Org")
            .with_account_group_id("openrouter:account-42")
            .with_account_display_name("Ada")
            .with_login_method("Secret Plan"),
        cost: None,
        wayfinder_usage: None,
        source_label: "API".to_string(),
    };
    let credential_id = uuid::Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap();
    let credential = super::CredentialSnapshotMeta::new(credential_id, "Personal".into(), 7);

    let success =
        ProviderUsageSnapshot::from_fetch_result(ProviderId::OpenRouter, &metadata, &result)
            .decorate_for_credential(&credential);
    let error = ProviderUsageSnapshot::from_error(
        ProviderId::OpenRouter,
        &metadata,
        "bad selected key".into(),
    )
    .decorate_for_credential(&credential);
    let json = serde_json::to_string(&success).unwrap();

    assert_eq!(success.credential_id, Some(credential_id));
    assert_eq!(
        success.credential_display_label.as_deref(),
        Some("Personal")
    );
    assert_eq!(success.credential_display_ordinal, Some(7));
    assert_eq!(success.account_email.as_deref(), Some("secret@example.com"));
    assert_eq!(success.account_organization.as_deref(), Some("Secret Org"));
    assert_eq!(
        success.account_group_id.as_deref(),
        Some("openrouter:account-42")
    );
    assert_eq!(success.account_display_name.as_deref(), Some("Ada"));
    assert_eq!(success.plan_name.as_deref(), Some("Secret Plan"));
    assert_eq!(error.credential_id, Some(credential_id));
    assert!(error.account_group_id.is_none());
    assert!(error.account_display_name.is_none());
    let object = serde_json::from_str::<serde_json::Value>(&json)
        .unwrap()
        .as_object()
        .unwrap()
        .clone();
    assert_eq!(object["accountGroupId"], "openrouter:account-42");
    assert_eq!(object["accountDisplayName"], "Ada");
    for forbidden in ["credential", "apiKey", "token", "cookie", "secret"] {
        assert!(
            !object.contains_key(forbidden),
            "serialized forbidden field: {forbidden}"
        );
    }
}

#[test]
fn credential_snapshot_rejects_audited_providers_that_ignore_explicit_api_keys() {
    let credential_id = uuid::Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap();
    let credential = super::CredentialSnapshotMeta::new(credential_id, "Key 1".into(), 1);
    for provider in [ProviderId::Alibaba, ProviderId::Grok] {
        let metadata = instantiate_provider(provider).metadata().clone();
        let result = ProviderFetchResult {
            usage: codexbar::core::UsageSnapshot::new(codexbar::core::RateWindow::new(10.0)),
            cost: None,
            wayfinder_usage: None,
            source_label: "api".to_string(),
        };
        let snapshot = ProviderUsageSnapshot::from_fetch_result(provider, &metadata, &result);
        let finalized =
            super::finalize_credential_snapshot(provider, &metadata, snapshot, &credential);

        assert!(
            finalized
                .error
                .as_deref()
                .is_some_and(|error| error.contains("does not support app-managed API keys"))
        );
        assert_eq!(finalized.credential_id, Some(credential_id));
    }
}

#[test]
fn credential_snapshot_allows_amp_key_backed_web_source() {
    let metadata = instantiate_provider(ProviderId::Amp).metadata().clone();
    let result = ProviderFetchResult {
        usage: codexbar::core::UsageSnapshot::new(codexbar::core::RateWindow::new(10.0)),
        cost: None,
        wayfinder_usage: None,
        source_label: "web".to_string(),
    };
    let credential_id = uuid::Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap();
    let credential = super::CredentialSnapshotMeta::new(credential_id, "Key 1".into(), 1);
    let snapshot = ProviderUsageSnapshot::from_fetch_result(ProviderId::Amp, &metadata, &result);
    let finalized =
        super::finalize_credential_snapshot(ProviderId::Amp, &metadata, snapshot, &credential);

    assert!(finalized.error.is_none());
    assert_eq!(finalized.source_label, "web");
    assert_eq!(finalized.credential_id, Some(credential_id));
}

#[test]
fn refresh_outcomes_treat_partial_credential_failure_as_degraded_not_provider_error() {
    let first = uuid::Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap();
    let second = uuid::Uuid::parse_str("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb").unwrap();
    let summary = super::summarize_provider_refresh_outcomes(&[
        super::ProviderRefreshOutcome::new(ProviderId::OpenRouter, Some(first), true),
        super::ProviderRefreshOutcome::new(ProviderId::OpenRouter, Some(second), false),
    ]);

    assert_eq!(summary.error_count, 0);
    assert_eq!(summary.degraded_credential_count, 1);
}

#[test]
fn refresh_outcomes_count_one_provider_error_when_all_credentials_fail() {
    let first = uuid::Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap();
    let second = uuid::Uuid::parse_str("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb").unwrap();
    let summary = super::summarize_provider_refresh_outcomes(&[
        super::ProviderRefreshOutcome::new(ProviderId::OpenRouter, Some(first), false),
        super::ProviderRefreshOutcome::new(ProviderId::OpenRouter, Some(second), false),
        super::ProviderRefreshOutcome::new(ProviderId::Codex, None, true),
    ]);

    assert_eq!(summary.error_count, 1);
    assert_eq!(summary.degraded_credential_count, 2);
}

#[tokio::test]
async fn overlapping_refresh_generations_share_one_eight_permit_semaphore() {
    let first = super::provider_fetch_permits();
    let second = super::provider_fetch_permits();
    assert!(std::sync::Arc::ptr_eq(&first, &second));

    let mut permits = Vec::new();
    for _ in 0..8 {
        permits.push(first.clone().try_acquire_owned().unwrap());
    }
    assert!(second.clone().try_acquire_owned().is_err());
    drop(permits);
    assert!(second.try_acquire_owned().is_ok());
}

#[test]
fn superseded_refresh_generation_is_not_current() {
    let mut state = AppState::new();
    state.provider_refresh_generation = 3;
    assert!(super::is_current_provider_refresh_generation(&state, 3));
    assert!(!super::is_current_provider_refresh_generation(&state, 2));
}

#[test]
fn provider_input_load_failure_releases_only_its_current_refresh_generation() {
    let mut state = AppState::new();
    state.provider_refresh_generation = 7;
    state.is_refreshing = true;
    state.provider_refresh_started_at = Some(std::time::Instant::now());

    super::abort_provider_refresh_generation(&mut state, 6);
    assert!(
        state.is_refreshing,
        "a stale failure must not stop newer work"
    );

    super::abort_provider_refresh_generation(&mut state, 7);
    assert!(!state.is_refreshing);
    assert!(state.provider_refresh_started_at.is_none());
}

#[test]
fn unreadable_secret_stores_abort_refresh_input_loading_without_overwrite() {
    use codexbar::storage::{StoragePaths, StoreError, StoreKind, UserScopeId};

    for store in [StoreKind::ManualCookies, StoreKind::TokenAccounts] {
        let root = std::env::temp_dir().join(format!(
            "codexbar-refresh-input-storage-test-{}",
            uuid::Uuid::new_v4()
        ));
        let paths = StoragePaths::from_roots(root.join("roaming"), root.join("local"));
        paths.ensure_directories().unwrap();
        let path = paths.store_path(store).unwrap().to_path_buf();
        let corrupt = b"{not-valid-json";
        std::fs::write(&path, corrupt).unwrap();
        let storage = crate::storage_services::StorageServices::new(
            paths,
            UserScopeId::from_stable_identifier(b"refresh-input-storage-test-user"),
        );

        let error = match super::ProviderRefreshInputs::load(Settings::default(), &storage) {
            Ok(_) => panic!("{store} must stop refresh input loading"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            StoreError::InvalidPayload {
                store: failed_store,
                ..
            } if failed_store == store
        ));
        assert_eq!(std::fs::read(&path).unwrap(), corrupt);

        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn hiding_codex_spark_rows_preserves_other_extra_usage() {
    let metadata = instantiate_provider(ProviderId::Codex).metadata().clone();
    let result = ProviderFetchResult {
        usage: codexbar::core::UsageSnapshot::new(codexbar::core::RateWindow::new(10.0)),
        cost: None,
        wayfinder_usage: None,
        source_label: "CLI".to_string(),
    };
    let mut snapshot =
        ProviderUsageSnapshot::from_fetch_result(ProviderId::Codex, &metadata, &result);
    snapshot.extra_rate_windows = vec![
        NamedRateWindowSnapshot {
            id: "codex-spark".to_string(),
            title: "Codex Spark 5-hour".to_string(),
            window: snapshot.primary.clone(),
        },
        NamedRateWindowSnapshot {
            id: "credits".to_string(),
            title: "Credits".to_string(),
            window: snapshot.primary.clone(),
        },
    ];

    super::filter_hidden_codex_spark_rows(&mut snapshot, false);

    assert_eq!(snapshot.extra_rate_windows.len(), 1);
    assert_eq!(snapshot.extra_rate_windows[0].id, "credits");
}

#[test]
fn claude_transient_auth_failure_preserves_first_last_good_snapshot() {
    let metadata = instantiate_provider(ProviderId::Claude).metadata().clone();
    let result = ProviderFetchResult {
        usage: codexbar::core::UsageSnapshot::new(codexbar::core::RateWindow::new(42.0)),
        cost: None,
        wayfinder_usage: None,
        source_label: "OAuth".to_string(),
    };
    let good = ProviderUsageSnapshot::from_fetch_result(ProviderId::Claude, &metadata, &result);
    let error = ProviderUsageSnapshot::from_error(
        ProviderId::Claude,
        &metadata,
        "Unauthorized".to_string(),
    );
    let mut state = crate::state::AppState::new();
    state.provider_cache.push(good.clone());

    let preserved = super::providers::preserve_last_good_transient_failure(
        &mut state,
        ProviderId::Claude,
        error,
    );

    assert_eq!(preserved.error, None);
    assert_eq!(preserved.primary.used_percent, 42.0);
}

#[test]
fn claude_repeated_auth_failure_surfaces_error() {
    let metadata = instantiate_provider(ProviderId::Claude).metadata().clone();
    let result = ProviderFetchResult {
        usage: codexbar::core::UsageSnapshot::new(codexbar::core::RateWindow::new(42.0)),
        cost: None,
        wayfinder_usage: None,
        source_label: "OAuth".to_string(),
    };
    let good = ProviderUsageSnapshot::from_fetch_result(ProviderId::Claude, &metadata, &result);
    let first_error = ProviderUsageSnapshot::from_error(
        ProviderId::Claude,
        &metadata,
        "Unauthorized".to_string(),
    );
    let second_error = first_error.clone();
    let mut state = crate::state::AppState::new();
    state.provider_cache.push(good);

    let _ = super::providers::preserve_last_good_transient_failure(
        &mut state,
        ProviderId::Claude,
        first_error,
    );
    let surfaced = super::providers::preserve_last_good_transient_failure(
        &mut state,
        ProviderId::Claude,
        second_error,
    );

    assert!(surfaced.error.is_some());
}

#[test]
fn claude_cli_parse_failure_keeps_last_good_every_time() {
    let metadata = instantiate_provider(ProviderId::Claude).metadata().clone();
    let result = ProviderFetchResult {
        usage: codexbar::core::UsageSnapshot::new(codexbar::core::RateWindow::new(17.0)),
        cost: None,
        wayfinder_usage: None,
        source_label: "CLI".to_string(),
    };
    let good = ProviderUsageSnapshot::from_fetch_result(ProviderId::Claude, &metadata, &result);
    let err = ProviderUsageSnapshot::from_error(
        ProviderId::Claude,
        &metadata,
        "Parse error: Empty output from Claude CLI".to_string(),
    );
    let mut state = crate::state::AppState::new();
    state.provider_cache.push(good.clone());

    let first = super::providers::preserve_last_good_transient_failure(
        &mut state,
        ProviderId::Claude,
        err.clone(),
    );
    let second =
        super::providers::preserve_last_good_transient_failure(&mut state, ProviderId::Claude, err);

    assert_eq!(first.error, None);
    assert_eq!(first.primary.used_percent, 17.0);
    // Parse failures keep last-good on every refresh (upstream #2247), unlike one-shot auth.
    assert_eq!(second.error, None);
    assert_eq!(second.primary.used_percent, 17.0);
}

#[test]
fn claude_hard_credentials_missing_does_not_preserve_stale() {
    let metadata = instantiate_provider(ProviderId::Claude).metadata().clone();
    let result = ProviderFetchResult {
        usage: codexbar::core::UsageSnapshot::new(codexbar::core::RateWindow::new(17.0)),
        cost: None,
        wayfinder_usage: None,
        source_label: "OAuth".to_string(),
    };
    let good = ProviderUsageSnapshot::from_fetch_result(ProviderId::Claude, &metadata, &result);
    let err = ProviderUsageSnapshot::from_error(
        ProviderId::Claude,
        &metadata,
        "OAuth error: Claude OAuth credentials not found. Run `claude` to authenticate."
            .to_string(),
    );
    let mut state = crate::state::AppState::new();
    state.provider_cache.push(good);

    let out =
        super::providers::preserve_last_good_transient_failure(&mut state, ProviderId::Claude, err);
    assert!(out.error.is_some());
}

#[test]
fn claude_error_message_removes_upstream_swift_cancellation() {
    let message = super::friendly_provider_error(
        ProviderId::Claude,
        "The operation couldn't be completed. (Swift.CancellationError error 1.)",
    );

    assert!(!message.contains("Swift"));
    assert!(message.contains("Claude usage fetch was cancelled"));
    assert!(message.contains("Refresh Claude"));
}

#[test]
fn claude_error_message_explains_missing_sign_in() {
    let message = super::friendly_provider_error(
        ProviderId::Claude,
        "OAuth error: Claude OAuth credentials not found. Run `claude` to authenticate.",
    );

    assert_eq!(
        message,
        "Claude sign-in was not found. Run `claude` once to authenticate, then refresh Claude in Win-CodexBar."
    );
}

#[test]
fn non_claude_error_message_is_preserved() {
    let message = super::friendly_provider_error(
        ProviderId::Codex,
        "OAuth error: Claude OAuth credentials not found. Run `claude` to authenticate.",
    );

    assert_eq!(
        message,
        "OAuth error: Claude OAuth credentials not found. Run `claude` to authenticate."
    );
}

#[test]
fn chart_data_serde_roundtrip_preserves_fields() {
    use super::{DailyCostPoint, DailyUsageBreakdown, ProviderChartData, ServiceUsagePoint};

    let original = ProviderChartData {
        provider_id: "codex".into(),
        cost_history: vec![
            DailyCostPoint {
                date: "2025-01-01".into(),
                value: 1.25,
            },
            DailyCostPoint {
                date: "2025-01-02".into(),
                value: 0.0,
            },
        ],
        credits_history: vec![DailyCostPoint {
            date: "2025-01-01".into(),
            value: 42.0,
        }],
        usage_breakdown: vec![DailyUsageBreakdown {
            day: "2025-01-01".into(),
            services: vec![
                ServiceUsagePoint {
                    service: "gpt-4o".into(),
                    credits_used: 10.0,
                },
                ServiceUsagePoint {
                    service: "gpt-4o-mini".into(),
                    credits_used: 3.5,
                },
            ],
            total_credits_used: 13.5,
        }],
        local_usage: None,
    };

    let json = serde_json::to_string(&original).expect("serialize");
    assert!(
        json.contains("\"providerId\":\"codex\""),
        "camelCase providerId: {json}"
    );
    assert!(json.contains("\"costHistory\""));
    assert!(json.contains("\"creditsHistory\""));
    assert!(json.contains("\"usageBreakdown\""));
    assert!(json.contains("\"localUsage\":null"));
    assert!(json.contains("\"creditsUsed\":10.0"));
    assert!(json.contains("\"totalCreditsUsed\":13.5"));

    let back: ProviderChartData = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back.provider_id, "codex");
    assert_eq!(back.cost_history.len(), 2);
    assert_eq!(back.cost_history[0].date, "2025-01-01");
    assert_eq!(back.credits_history[0].value, 42.0);
    assert_eq!(back.usage_breakdown[0].services.len(), 2);
    assert_eq!(back.usage_breakdown[0].total_credits_used, 13.5);
}

#[test]
fn chart_data_for_unknown_provider_is_empty() {
    let data =
        super::build_provider_chart_data("this-provider-definitely-does-not-exist".into(), None);
    assert_eq!(data.provider_id, "this-provider-definitely-does-not-exist");
    assert!(data.credits_history.is_empty());
    assert!(data.usage_breakdown.is_empty());
}

#[test]
fn japanese_provider_snapshot_localizes_weekly_label() {
    let metadata = instantiate_provider(ProviderId::Claude).metadata().clone();
    let usage = codexbar::core::UsageSnapshot::new(codexbar::core::RateWindow::new(10.0))
        .with_secondary(codexbar::core::RateWindow::new(20.0));
    let result = ProviderFetchResult {
        usage,
        cost: None,
        wayfinder_usage: None,
        source_label: "OAuth".to_string(),
    };

    let snapshot = ProviderUsageSnapshot::from_fetch_result(ProviderId::Claude, &metadata, &result);

    // Secondary label stays raw; localization happens at render time.
    assert_eq!(snapshot.secondary_label, Some("Weekly".to_string()));
}

#[test]
fn kimi_legacy_monthly_window_is_not_exposed_as_key_quota() {
    let metadata = instantiate_provider(ProviderId::Kimi).metadata().clone();
    let usage = codexbar::core::UsageSnapshot::new(codexbar::core::RateWindow::new(10.0))
        .with_extra_rate_window(
            "kimi-monthly",
            "Monthly",
            codexbar::core::RateWindow::new(35.0),
        );
    let result = ProviderFetchResult {
        usage,
        cost: None,
        wayfinder_usage: None,
        source_label: "web".to_string(),
    };

    let snapshot = ProviderUsageSnapshot::from_fetch_result(ProviderId::Kimi, &metadata, &result);
    assert!(snapshot.extra_rate_windows.is_empty());
}

#[test]
fn minimax_token_plan_labels_do_not_change_legacy_billing_labels() {
    let metadata = instantiate_provider(ProviderId::MiniMax).metadata().clone();
    let usage = codexbar::core::UsageSnapshot::new(codexbar::core::RateWindow::new(10.0))
        .with_secondary(codexbar::core::RateWindow::new(20.0));
    let mut result = ProviderFetchResult {
        usage,
        cost: None,
        wayfinder_usage: None,
        source_label: "token-plan".to_string(),
    };

    let token_plan =
        ProviderUsageSnapshot::from_fetch_result(ProviderId::MiniMax, &metadata, &result);
    assert_eq!(token_plan.primary_label, Some("Rate Limit".to_string()));
    assert_eq!(token_plan.secondary_label, Some("Weekly".to_string()));

    result.source_label = "web".to_string();
    let legacy = ProviderUsageSnapshot::from_fetch_result(ProviderId::MiniMax, &metadata, &result);
    assert_eq!(legacy.primary_label, Some("Usage".to_string()));
    assert_eq!(legacy.secondary_label, Some("Monthly".to_string()));
}

#[test]
fn japanese_provider_snapshot_localizes_pace_reserve_description() {
    use chrono::{Duration, Utc};

    let metadata = instantiate_provider(ProviderId::Claude).metadata().clone();
    let now = Utc::now();
    // 7-day window, half elapsed, 40% used → 10% ahead of pace, will last to reset.
    let secondary = codexbar::core::RateWindow::with_details(
        40.0,
        Some(7 * 24 * 60),
        Some(now + Duration::minutes(7 * 24 * 60 / 2)),
        None,
    );
    let usage = codexbar::core::UsageSnapshot::new(codexbar::core::RateWindow::new(10.0))
        .with_secondary(secondary);
    let result = ProviderFetchResult {
        usage,
        cost: None,
        wayfinder_usage: None,
        source_label: "OAuth".to_string(),
    };

    let snapshot = ProviderUsageSnapshot::from_fetch_result(ProviderId::Claude, &metadata, &result);

    // Reserve data stays raw; localization happens at render time.
    let secondary = snapshot.secondary.as_ref().expect("secondary window");
    assert!(secondary.reserve_percent.is_some());
    assert!(secondary.reserve_will_last_to_reset);
    assert!(secondary.reserve_description.is_none());
}

#[test]
fn chart_data_does_not_read_legacy_dashboard_history() {
    let data = super::build_provider_chart_data("codex".into(), Some("legacy@example.test".into()));
    assert_eq!(data.provider_id, "codex");
    assert!(data.credits_history.is_empty());
    assert!(data.usage_breakdown.is_empty());
}

#[test]
fn cookie_options_for_cookie_supporting_provider() {
    let opts = super::cookie_source_options_for("codex", Language::English);
    let values: Vec<_> = opts.iter().map(|o| o.value.as_str()).collect();
    assert_eq!(values, vec!["auto", "manual", "off"]);
    assert!(opts.iter().any(|o| o.label == "Automatic"));
    assert!(opts.iter().any(|o| o.label == "Manual"));
    assert!(opts.iter().any(|o| o.label == "Disabled"));
}

#[test]
fn cookie_options_empty_for_providers_without_picker() {
    assert!(super::cookie_source_options_for("anthropic", Language::English).is_empty());
    assert!(super::cookie_source_options_for("unknown", Language::English).is_empty());
}

#[test]
fn region_options_for_regional_provider() {
    let opts = super::region_options_for("alibaba");
    let values: Vec<_> = opts.iter().map(|o| o.value.as_str()).collect();
    assert_eq!(values, vec!["singapore", "us", "germany", "hongkong", "cn"]);
}

#[test]
fn minimax_region_options_match_upstream_hosts() {
    let opts = super::region_options_for("minimax");
    let values: Vec<_> = opts.iter().map(|o| o.value.as_str()).collect();
    let labels: Vec<_> = opts.iter().map(|o| o.label.as_str()).collect();
    assert_eq!(values, vec!["global", "cn"]);
    assert_eq!(
        labels,
        vec![
            "Global (platform.minimax.io)",
            "China mainland (platform.minimaxi.com)"
        ]
    );
}

#[test]
fn region_options_empty_for_non_regional_provider() {
    assert!(super::region_options_for("claude").is_empty());
    assert!(super::region_options_for("codex").is_empty());
}

#[test]
fn cookie_source_option_roundtrips_serde() {
    let opt = super::CookieSourceOption {
        value: "auto".to_string(),
        label: "Automatic".to_string(),
        description: Some("Imports browser cookies.".to_string()),
    };
    let json = serde_json::to_string(&opt).unwrap();
    let back: super::CookieSourceOption = serde_json::from_str(&json).unwrap();
    assert_eq!(opt, back);
}

#[test]
fn region_option_roundtrips_serde() {
    let opt = super::RegionOption {
        value: "intl".to_string(),
        label: "International".to_string(),
    };
    let json = serde_json::to_string(&opt).unwrap();
    let back: super::RegionOption = serde_json::from_str(&json).unwrap();
    assert_eq!(opt, back);
}

// ── Phase 6d — credential detection UIs ────────────────────────

#[test]
fn open_path_rejects_empty_path() {
    let err = super::open_path(String::new()).unwrap_err();
    assert!(err.to_lowercase().contains("empty"));
}

#[test]
fn open_path_rejects_relative_path() {
    let err = super::open_path("relative/path".into()).unwrap_err();
    assert!(err.contains("absolute"));
}

#[test]
fn open_path_rejects_missing_path() {
    let missing = std::env::temp_dir()
        .join(format!("codexbar-phase6d-missing-{}", std::process::id()))
        .join("does-not-exist");
    let err = super::open_path(missing.to_string_lossy().into_owned()).unwrap_err();
    assert!(err.contains("not found"));
}

#[test]
fn external_url_validator_accepts_http_and_https() {
    assert_eq!(
        super::validate_external_url(" https://github.com/Finesssee/Win-CodexBar "),
        Ok("https://github.com/Finesssee/Win-CodexBar")
    );
    assert_eq!(
        super::validate_external_url("http://localhost:1420"),
        Ok("http://localhost:1420")
    );
}

#[test]
fn external_url_validator_rejects_non_web_and_control_urls() {
    assert!(super::validate_external_url("file:///C:/Windows/win.ini").is_err());
    assert!(super::validate_external_url("javascript:alert(1)").is_err());
    assert!(super::validate_external_url("https://example.com/\nmalicious").is_err());
}

// ── Phase 13 — E2E IPC harness ─────────────────────────────────
//
// Build the full bootstrap payload and prove that every active shared
// `ProviderId` variant ends up in the provider catalog with a non-empty
// id + display name. Soft-removed variants remain CLI-compatible but are
// not part of a fresh user's public catalog.

#[test]
fn bootstrap_payload_exposes_every_active_provider_variant() {
    let payload = super::bootstrap_state_for(Settings::default());

    let catalog_ids: std::collections::HashSet<String> = payload
        .providers
        .iter()
        .map(|entry| entry.id.clone())
        .collect();

    for entry in &payload.providers {
        assert!(!entry.id.is_empty(), "provider entry has empty id");
        assert!(
            !entry.display_name.is_empty(),
            "provider {} has empty display_name",
            entry.id
        );
    }

    for provider in ProviderId::all()
        .iter()
        .filter(|provider| !provider.is_deprecated())
    {
        let expected = provider.cli_name().to_string();
        assert!(
            catalog_ids.contains(&expected),
            "missing provider in bootstrap catalog: {expected}"
        );
    }

    assert_eq!(
        catalog_ids.len(),
        ProviderId::all()
            .iter()
            .filter(|provider| !provider.is_deprecated())
            .count(),
        "bootstrap catalog size drifted from the active provider set"
    );
    assert!(!catalog_ids.contains("kimik2"));
    assert!(!catalog_ids.contains("crossmodel"));

    // Sanity — payload must also round-trip through JSON cleanly so
    // the TypeScript bridge never sees a partially-populated record.
    let encoded = serde_json::to_string(&payload).expect("serialize bootstrap");
    assert!(encoded.contains("contractVersion"));
    assert!(encoded.contains("\"providers\""));
    assert!(encoded.contains("\"settings\""));
}
