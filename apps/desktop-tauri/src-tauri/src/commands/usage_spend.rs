//! Usage & Spend settings tab: 7-day / 30-day local cost aggregates.

use codexbar::cost_scanner::CostScanner;
use serde::Serialize;
use tauri::State;

use super::ProviderUsageSnapshot;
use crate::state::AppState;
use std::sync::Mutex;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSpendRow {
    pub provider_id: String,
    pub display_name: String,
    pub seven_day: Option<f64>,
    pub thirty_day: Option<f64>,
    pub currency: String,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential_count: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSpendSummary {
    pub rows: Vec<UsageSpendRow>,
}

#[tauri::command]
pub async fn get_usage_spend_summary(
    state: State<'_, Mutex<AppState>>,
) -> Result<UsageSpendSummary, String> {
    let (cached, settings) = {
        let guard = state.lock().map_err(|e| e.to_string())?;
        (guard.provider_cache.clone(), guard.settings.clone())
    };

    tauri::async_runtime::spawn_blocking(move || build_usage_spend_summary(&cached, &settings))
        .await
        .map_err(|e| format!("usage spend worker failed: {e}"))
}

fn build_usage_spend_summary(
    cached: &[ProviderUsageSnapshot],
    settings: &codexbar::settings::Settings,
) -> UsageSpendSummary {
    let mut rows = Vec::new();

    // Local JSONL scanners for Codex / Claude (primary spend sources).
    let codex_7 = CostScanner::new(7, settings).scan_codex().total_cost_usd;
    let codex_30 = CostScanner::new(30, settings).scan_codex().total_cost_usd;
    rows.push(UsageSpendRow {
        provider_id: "codex".into(),
        display_name: "Codex".into(),
        seven_day: Some(codex_7),
        thirty_day: Some(codex_30),
        currency: "USD".into(),
        source: "local logs".into(),
        credential_count: None,
    });

    let claude_7 = CostScanner::new(7, settings).scan_claude().total_cost_usd;
    let claude_30 = CostScanner::new(30, settings).scan_claude().total_cost_usd;
    rows.push(UsageSpendRow {
        provider_id: "claude".into(),
        display_name: "Claude".into(),
        seven_day: Some(claude_7),
        thirty_day: Some(claude_30),
        currency: "USD".into(),
        source: "local logs".into(),
        credential_count: None,
    });

    // Surface any other provider cost snapshots from the last refresh (period
    // costs, not calendar 7d/30d — shown under thirty_day only).
    rows.extend(build_cached_provider_rows(cached));

    UsageSpendSummary { rows }
}

fn build_cached_provider_rows(cached: &[ProviderUsageSnapshot]) -> Vec<UsageSpendRow> {
    let mut rows = Vec::new();
    let mut grouped = std::collections::HashMap::<String, Vec<&ProviderUsageSnapshot>>::new();
    let mut provider_order = Vec::new();
    for snapshot in cached {
        if !grouped.contains_key(&snapshot.provider_id) {
            provider_order.push(snapshot.provider_id.clone());
        }
        grouped
            .entry(snapshot.provider_id.clone())
            .or_default()
            .push(snapshot);
    }
    for provider_id in provider_order {
        if provider_id == "codex" || provider_id == "claude" {
            continue;
        }
        let snapshots = &grouped[&provider_id];
        let observed_credential_count = snapshots
            .iter()
            .filter(|snapshot| snapshot.credential_id.is_some())
            .count();
        let credential_count = snapshots
            .iter()
            .fold(observed_credential_count, |count, snapshot| {
                count.max(snapshot.credential_group_size.unwrap_or_default())
            });
        if credential_count > 1 {
            rows.push(UsageSpendRow {
                provider_id: provider_id.clone(),
                display_name: snapshots[0].display_name.clone(),
                seven_day: None,
                thirty_day: None,
                currency: String::new(),
                source: String::new(),
                credential_count: Some(credential_count),
            });
            continue;
        }
        let snapshot = snapshots[0];
        let Some(cost) = &snapshot.cost else {
            continue;
        };
        rows.push(UsageSpendRow {
            provider_id: snapshot.provider_id.clone(),
            display_name: if snapshot.display_name.is_empty() {
                snapshot.provider_id.clone()
            } else {
                snapshot.display_name.clone()
            },
            seven_day: None,
            thirty_day: Some(cost.used),
            currency: cost.currency_code.clone(),
            source: format!("period ({})", cost.period),
            credential_count: None,
        });
    }

    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{CostSnapshotBridge, RateWindowSnapshot};

    fn snapshot(used_cost: f64, credential_id: &str) -> ProviderUsageSnapshot {
        ProviderUsageSnapshot {
            provider_id: "openrouter".to_string(),
            credential_id: Some(uuid::Uuid::parse_str(credential_id).unwrap()),
            credential_display_label: Some("safe label".to_string()),
            credential_display_ordinal: Some(1),
            credential_group_size: None,
            display_name: "OpenRouter".to_string(),
            primary: RateWindowSnapshot {
                used_percent: 50.0,
                remaining_percent: 50.0,
                window_minutes: None,
                resets_at: None,
                reset_description: None,
                is_exhausted: false,
                is_informational: false,
                reserve_percent: None,
                reserve_description: None,
                reserve_eta_seconds: None,
                reserve_will_last_to_reset: false,
            },
            primary_label: None,
            secondary: None,
            secondary_label: None,
            model_specific: None,
            tertiary: None,
            extra_rate_windows: Vec::new(),
            cost: Some(CostSnapshotBridge {
                used: used_cost,
                limit: Some(100.0),
                remaining: Some(100.0 - used_cost),
                currency_code: "USD".to_string(),
                period: "monthly".to_string(),
                resets_at: None,
                formatted_used: format!("${used_cost:.2}"),
                formatted_limit: Some("$100.00".to_string()),
            }),
            plan_name: None,
            account_email: None,
            account_group_id: None,
            account_display_name: None,
            source_label: "api".to_string(),
            updated_at: "2026-07-22T00:00:00Z".to_string(),
            error: None,
            refresh_error: None,
            pace: None,
            account_organization: None,
            tray_status_label: None,
            fetch_duration_ms: None,
            wayfinder_usage: None,
        }
    }

    #[test]
    fn multi_key_provider_emits_one_neutral_row_without_cost_selection_or_aggregation() {
        let provider_rows = build_cached_provider_rows(&[
            snapshot(91.0, "00000000-0000-0000-0000-000000000001"),
            snapshot(12.0, "00000000-0000-0000-0000-000000000002"),
        ]);
        let rows = provider_rows
            .iter()
            .filter(|row| row.provider_id == "openrouter")
            .collect::<Vec<_>>();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].thirty_day, None);
        assert_eq!(rows[0].credential_count, Some(2));
    }
}
