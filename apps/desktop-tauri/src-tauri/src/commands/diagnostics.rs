use std::collections::HashMap;

use codexbar::core::ProviderId;
use codexbar::settings::{ApiKeys, ManualCookies, Settings};
use codexbar::storage::StoreKind;
use serde::Serialize;
use std::sync::Mutex;

use crate::state::AppState;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SafeDiagnostics {
    pub app_version: String,
    pub platform: String,
    pub enabled_providers: Vec<String>,
    pub provider_cookie_sources: HashMap<String, String>,
    pub has_manual_cookies: Vec<String>,
    pub has_api_keys: Vec<String>,
    pub api_key_counts: HashMap<String, usize>,
    pub hide_personal_info: bool,
    pub refresh_interval_secs: u64,
}

fn safe_diagnostics_from(
    settings: Settings,
    cookies: ManualCookies,
    api_keys: ApiKeys,
) -> SafeDiagnostics {
    let mut enabled_providers = settings
        .get_enabled_provider_ids()
        .into_iter()
        .map(|id| id.cli_name().to_string())
        .collect::<Vec<_>>();
    enabled_providers.sort();

    let provider_cookie_sources = ProviderId::all()
        .iter()
        .map(|id| {
            (
                id.cli_name().to_string(),
                settings.cookie_source(*id).to_string(),
            )
        })
        .collect::<HashMap<_, _>>();

    let mut has_manual_cookies = cookies
        .get_all_for_display()
        .into_iter()
        .map(|entry| entry.provider_id)
        .collect::<Vec<_>>();
    has_manual_cookies.sort();

    let mut api_key_counts = HashMap::new();
    for entry in api_keys.get_all_for_display() {
        *api_key_counts.entry(entry.provider_id).or_insert(0) += 1;
    }
    let mut has_api_keys = api_key_counts.keys().cloned().collect::<Vec<_>>();
    has_api_keys.sort();

    SafeDiagnostics {
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        platform: std::env::consts::OS.to_string(),
        enabled_providers,
        provider_cookie_sources,
        has_manual_cookies,
        has_api_keys,
        api_key_counts,
        hide_personal_info: settings.hide_personal_info,
        refresh_interval_secs: settings.refresh_interval_secs,
    }
}

#[tauri::command]
pub fn get_safe_diagnostics(
    state: tauri::State<'_, Mutex<AppState>>,
) -> Result<SafeDiagnostics, String> {
    let storage = crate::storage_services::storage_snapshot(&state);
    let cookies = storage
        .manual_cookies
        .load()
        .map_err(crate::storage_services::command_store_error)?
        .into_writable_for(StoreKind::ManualCookies)
        .map_err(crate::storage_services::command_store_error)?;
    let api_keys = storage
        .api_keys
        .load()
        .map_err(crate::storage_services::command_store_error)?
        .into_writable_for(StoreKind::ApiKeys)
        .map_err(crate::storage_services::command_store_error)?;
    Ok(safe_diagnostics_from(
        crate::storage_services::settings_snapshot(&state),
        cookies,
        api_keys,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_diagnostics_contains_no_secret_values() {
        let settings = Settings::default();
        let mut cookies = ManualCookies::default();
        cookies.set("codex", "session=secret-cookie-value");
        let mut keys = ApiKeys::default();
        keys.set("openrouter", "sk-secret-api-key", None);

        let payload = safe_diagnostics_from(settings, cookies, keys);
        let json = serde_json::to_string(&payload).expect("serialize diagnostics");

        assert!(json.contains("codex"));
        assert!(json.contains("openrouter"));
        assert!(!json.contains("secret-cookie-value"));
        assert!(!json.contains("sk-secret-api-key"));
        assert!(!json.contains("session="));
    }

    #[test]
    fn safe_diagnostics_reports_only_provider_key_counts() {
        let settings = Settings::default();
        let cookies = ManualCookies::default();
        let mut keys = ApiKeys::default();
        keys.add("openrouter", "sk-first", Some("Personal UUID-like label"))
            .unwrap();
        keys.add("openrouter", "sk-second", Some("Work")).unwrap();
        keys.add("kimi", "sk-third", None).unwrap();

        let payload = safe_diagnostics_from(settings, cookies, keys);
        let json = serde_json::to_string(&payload).expect("serialize diagnostics");

        assert_eq!(payload.api_key_counts.get("openrouter"), Some(&2));
        assert_eq!(payload.api_key_counts.get("kimi"), Some(&1));
        assert_eq!(payload.has_api_keys, vec!["kimi", "openrouter"]);
        assert!(!json.contains("Personal UUID-like label"));
        assert!(!json.contains("Work"));
        assert!(!json.contains("sk-first"));
    }
}
