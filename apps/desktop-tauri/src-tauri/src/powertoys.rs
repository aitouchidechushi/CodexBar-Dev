use std::sync::Mutex;
use std::time::Duration;

use chrono::Utc;
use serde::Serialize;
use tauri::Manager;

use crate::commands::{
    ProviderLocalUsageSummary, ProviderUsageSnapshot, RateWindowSnapshot,
    cached_provider_local_usage_summary,
};
use crate::state::AppState;
use crate::tray_bridge::{ProviderPresentationSummary, provider_presentation_summaries};

pub const STATUS_PIPE_NAME: &str = r"\\.\pipe\WinCodexBar.Status";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PowerToysSnapshot {
    version: u32,
    updated_at: String,
    providers: Vec<PowerToysProviderSnapshot>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PowerToysProviderSnapshot {
    id: String,
    name: String,
    status_text: String,
    subtitle: Option<String>,
    primary_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    primary: Option<RateWindowSnapshot>,
    secondary_label: Option<String>,
    secondary: Option<RateWindowSnapshot>,
    today_cost: Option<f64>,
    thirty_day_cost: Option<f64>,
    latest_tokens: Option<u64>,
    thirty_day_tokens: Option<u64>,
    top_model: Option<String>,
    updated_at: String,
    error: Option<String>,
}

#[cfg(not(windows))]
pub fn install(_app: tauri::AppHandle) {}

#[cfg(windows)]
pub fn install(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        run_status_pipe(app).await;
    });
}

pub fn snapshot(app: &tauri::AppHandle) -> PowerToysSnapshot {
    let (snapshots, language) = app
        .state::<Mutex<AppState>>()
        .lock()
        .map(|guard| (guard.provider_cache.clone(), guard.settings.ui_language))
        .unwrap_or_else(|poisoned| {
            let guard = poisoned.into_inner();
            (guard.provider_cache.clone(), guard.settings.ui_language)
        });
    let providers = provider_snapshots(snapshots, language);

    PowerToysSnapshot {
        version: 1,
        updated_at: Utc::now().to_rfc3339(),
        providers,
    }
}

fn provider_snapshot(provider: ProviderUsageSnapshot) -> PowerToysProviderSnapshot {
    let local_usage = cached_local_usage(&provider.provider_id);
    let status_text = if provider.error.is_some() {
        "error".to_string()
    } else {
        format!("{}%", provider.primary.used_percent.round())
    };
    let subtitle = provider_subtitle(&provider, local_usage.as_ref());

    PowerToysProviderSnapshot {
        id: provider.provider_id,
        name: provider.display_name,
        status_text,
        subtitle,
        primary_label: provider.primary_label,
        primary: Some(provider.primary),
        secondary_label: provider.secondary_label,
        secondary: provider.secondary,
        today_cost: local_usage.as_ref().and_then(|summary| summary.today_cost),
        thirty_day_cost: local_usage
            .as_ref()
            .and_then(|summary| summary.thirty_day_cost),
        latest_tokens: local_usage
            .as_ref()
            .and_then(|summary| summary.latest_tokens),
        thirty_day_tokens: local_usage
            .as_ref()
            .and_then(|summary| summary.thirty_day_tokens),
        top_model: local_usage.and_then(|summary| summary.top_model),
        updated_at: provider.updated_at,
        error: provider.error,
    }
}

fn provider_subtitle(
    provider: &ProviderUsageSnapshot,
    local_usage: Option<&ProviderLocalUsageSummary>,
) -> Option<String> {
    let mut parts = Vec::new();
    if let (Some(label), Some(secondary)) = (&provider.secondary_label, &provider.secondary) {
        parts.push(format!("{} {}%", label, secondary.used_percent.round()));
    }
    if let Some(cost) = local_usage.and_then(|summary| summary.today_cost) {
        parts.push(format!("Today ${cost:.2}"));
    }
    if parts.is_empty() {
        provider
            .primary
            .reset_description
            .as_ref()
            .map(|reset| reset.to_string())
    } else {
        Some(parts.join(" · "))
    }
}

fn cached_local_usage(provider_id: &str) -> Option<ProviderLocalUsageSummary> {
    cached_provider_local_usage_summary(provider_id)
}

#[cfg(windows)]
async fn run_status_pipe(app: tauri::AppHandle) {
    use tokio::io::AsyncWriteExt;
    use tokio::net::windows::named_pipe::ServerOptions;

    loop {
        let server = match ServerOptions::new().create(STATUS_PIPE_NAME) {
            Ok(server) => server,
            Err(err) => {
                tracing::warn!("failed to create PowerToys status pipe: {err}");
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };

        if let Err(err) = server.connect().await {
            tracing::warn!("PowerToys status pipe connection failed: {err}");
            continue;
        }

        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            let mut server = server;
            let payload = serde_json::to_vec(&snapshot(&app)).unwrap_or_else(|err| {
                tracing::warn!("failed to serialize PowerToys snapshot: {err}");
                b"{\"version\":1,\"providers\":[]}".to_vec()
            });
            let _ = server.write_all(&payload).await;
            let _ = server.write_all(b"\n").await;
            let _ = server.flush().await;
        });
    }
}

fn provider_snapshots(
    snapshots: Vec<ProviderUsageSnapshot>,
    lang: codexbar::settings::Language,
) -> Vec<PowerToysProviderSnapshot> {
    provider_presentation_summaries(&snapshots)
        .into_iter()
        .map(|summary| {
            if summary.is_multi_key() {
                multi_key_provider_snapshot(summary, lang)
            } else {
                provider_snapshot(
                    summary
                        .snapshots
                        .into_iter()
                        .next()
                        .expect("non-empty summary"),
                )
            }
        })
        .collect()
}

fn multi_key_provider_snapshot(
    summary: ProviderPresentationSummary,
    lang: codexbar::settings::Language,
) -> PowerToysProviderSnapshot {
    use codexbar::locale::{LocaleKey, format_locale};

    let local_usage = cached_local_usage(&summary.provider_id);
    let all_failed = summary.is_all_failed();
    let status_text = if all_failed {
        "error".to_string()
    } else {
        format_locale(
            lang,
            LocaleKey::ApiKeyCount,
            &[&summary.credential_count.to_string()],
        )
    };
    let updated_at = summary.updated_at();

    PowerToysProviderSnapshot {
        id: summary.provider_id,
        name: summary.display_name,
        status_text,
        subtitle: None,
        primary_label: None,
        primary: None,
        secondary_label: None,
        secondary: None,
        today_cost: local_usage.as_ref().and_then(|usage| usage.today_cost),
        thirty_day_cost: local_usage.as_ref().and_then(|usage| usage.thirty_day_cost),
        latest_tokens: local_usage.as_ref().and_then(|usage| usage.latest_tokens),
        thirty_day_tokens: local_usage
            .as_ref()
            .and_then(|usage| usage.thirty_day_tokens),
        top_model: local_usage.and_then(|usage| usage.top_model),
        updated_at,
        error: all_failed.then(|| "error".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rate_window(used_percent: f64) -> RateWindowSnapshot {
        RateWindowSnapshot {
            used_percent,
            remaining_percent: 100.0 - used_percent,
            window_minutes: None,
            resets_at: None,
            reset_description: None,
            is_exhausted: false,
            is_informational: false,
            reserve_percent: None,
            reserve_description: None,
            reserve_eta_seconds: None,
            reserve_will_last_to_reset: false,
        }
    }

    fn credential_snapshot(
        used_percent: f64,
        credential_id: &str,
        error: Option<&str>,
    ) -> ProviderUsageSnapshot {
        ProviderUsageSnapshot {
            provider_id: "test-provider".to_string(),
            credential_id: Some(uuid::Uuid::parse_str(credential_id).unwrap()),
            credential_display_label: Some("safe label".to_string()),
            credential_display_ordinal: Some(1),
            credential_group_size: None,
            display_name: "Test Provider".to_string(),
            primary: rate_window(used_percent),
            primary_label: Some("Session".to_string()),
            secondary: None,
            secondary_label: None,
            model_specific: None,
            tertiary: None,
            extra_rate_windows: Vec::new(),
            cost: None,
            plan_name: None,
            account_email: None,
            account_group_id: None,
            account_display_name: None,
            source_label: "api".to_string(),
            updated_at: "2026-07-09T00:00:00Z".to_string(),
            error: error.map(str::to_string),
            refresh_error: None,
            pace: None,
            account_organization: None,
            tray_status_label: None,
            fetch_duration_ms: None,
            wayfinder_usage: None,
        }
    }

    #[test]
    fn provider_snapshot_omits_account_identity_fields() {
        let snapshot = provider_snapshot(ProviderUsageSnapshot {
            provider_id: "test-provider".to_string(),
            credential_id: None,
            credential_display_label: None,
            credential_display_ordinal: None,
            credential_group_size: None,
            display_name: "Test Provider".to_string(),
            primary: rate_window(42.0),
            primary_label: Some("Session".to_string()),
            secondary: None,
            secondary_label: None,
            model_specific: None,
            tertiary: None,
            extra_rate_windows: Vec::new(),
            cost: None,
            plan_name: Some("Team".to_string()),
            account_email: Some("dev@example.com".to_string()),
            account_group_id: None,
            account_display_name: None,
            source_label: "web".to_string(),
            updated_at: "2026-07-09T00:00:00Z".to_string(),
            error: None,
            refresh_error: None,
            pace: None,
            account_organization: Some("Example Org".to_string()),
            tray_status_label: None,
            fetch_duration_ms: None,
            wayfinder_usage: None,
        });
        let value = serde_json::to_value(snapshot).unwrap();

        assert!(value.get("planName").is_none());
        assert!(value.get("accountEmail").is_none());
    }

    #[test]
    fn provider_snapshot_includes_cached_local_usage_fields() {
        crate::commands::cache_provider_local_usage_summary_for_test(
            "test-provider",
            Some(ProviderLocalUsageSummary {
                today_cost: Some(1.25),
                thirty_day_cost: Some(12.5),
                thirty_day_tokens: Some(42_000),
                latest_tokens: Some(1_200),
                top_model: Some("gpt-5".to_string()),
                estimate_note: "cached".to_string(),
                token_cost_updated_at_ms: 1234,
            }),
        );

        let snapshot = provider_snapshot(ProviderUsageSnapshot {
            provider_id: "test-provider".to_string(),
            credential_id: None,
            credential_display_label: None,
            credential_display_ordinal: None,
            credential_group_size: None,
            display_name: "Test Provider".to_string(),
            primary: rate_window(42.0),
            primary_label: Some("Session".to_string()),
            secondary: None,
            secondary_label: None,
            model_specific: None,
            tertiary: None,
            extra_rate_windows: Vec::new(),
            cost: None,
            plan_name: None,
            account_email: None,
            account_group_id: None,
            account_display_name: None,
            source_label: "web".to_string(),
            updated_at: "2026-07-09T00:00:00Z".to_string(),
            error: None,
            refresh_error: None,
            pace: None,
            account_organization: None,
            tray_status_label: None,
            fetch_duration_ms: None,
            wayfinder_usage: None,
        });
        let value = serde_json::to_value(snapshot).unwrap();

        assert_eq!(value.get("todayCost").and_then(|v| v.as_f64()), Some(1.25));
        assert_eq!(
            value.get("thirtyDayCost").and_then(|v| v.as_f64()),
            Some(12.5)
        );
        assert_eq!(
            value.get("latestTokens").and_then(|v| v.as_u64()),
            Some(1_200)
        );
        assert_eq!(
            value.get("thirtyDayTokens").and_then(|v| v.as_u64()),
            Some(42_000)
        );
        assert_eq!(
            value.get("topModel").and_then(|v| v.as_str()),
            Some("gpt-5")
        );
    }

    #[test]
    fn multi_key_provider_emits_one_neutral_entry_without_quota_or_reset() {
        let providers = provider_snapshots(
            vec![
                credential_snapshot(91.0, "00000000-0000-0000-0000-000000000001", None),
                credential_snapshot(12.0, "00000000-0000-0000-0000-000000000002", None),
            ],
            codexbar::settings::Language::English,
        );

        assert_eq!(providers.len(), 1);
        let value = serde_json::to_value(&providers[0]).unwrap();
        assert_eq!(
            value.get("id").and_then(|v| v.as_str()),
            Some("test-provider")
        );
        assert_eq!(
            value.get("statusText").and_then(|v| v.as_str()),
            Some("2 keys")
        );
        assert!(value.get("primary").is_none() || value.get("primary").unwrap().is_null());
        assert!(value.get("secondary").is_none() || value.get("secondary").unwrap().is_null());
        assert!(value.get("subtitle").is_none() || value.get("subtitle").unwrap().is_null());
        assert!(!serde_json::to_string(&value).unwrap().contains("91"));
    }

    #[test]
    fn retained_sibling_stays_neutral_while_changed_key_refreshes() {
        let mut sibling = credential_snapshot(91.0, "00000000-0000-0000-0000-000000000001", None);
        sibling.credential_group_size = Some(2);
        let providers = provider_snapshots(vec![sibling], codexbar::settings::Language::English);

        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].status_text, "2 keys");
        assert!(providers[0].primary.is_none());
        assert!(providers[0].subtitle.is_none());
    }

    #[test]
    fn partial_failure_remains_neutral_and_all_failure_is_one_safe_error_entry() {
        let partial = provider_snapshots(
            vec![
                credential_snapshot(84.0, "00000000-0000-0000-0000-000000000001", None),
                credential_snapshot(
                    0.0,
                    "00000000-0000-0000-0000-000000000002",
                    Some("secret backend failure"),
                ),
            ],
            codexbar::settings::Language::English,
        );
        assert_eq!(partial.len(), 1);
        assert_eq!(partial[0].status_text, "2 keys");
        assert!(partial[0].error.is_none());

        let failed = provider_snapshots(
            vec![
                credential_snapshot(
                    0.0,
                    "00000000-0000-0000-0000-000000000001",
                    Some("first secret failure"),
                ),
                credential_snapshot(
                    0.0,
                    "00000000-0000-0000-0000-000000000002",
                    Some("second secret failure"),
                ),
            ],
            codexbar::settings::Language::English,
        );
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].status_text, "error");
        assert_eq!(failed[0].error.as_deref(), Some("error"));
    }
}
