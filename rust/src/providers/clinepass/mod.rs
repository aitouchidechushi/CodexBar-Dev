//! ClinePass API-key usage provider (upstream 0.44 #2219).
//!
//! `GET https://api.cline.bot/api/v1/users/me/plan/usage-limits`

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::Deserialize;

use crate::core::{
    FetchContext, Provider, ProviderError, ProviderFetchResult, ProviderId, ProviderMetadata,
    RateWindow, SourceMode, UsageSnapshot,
};

const USAGE_URL: &str = "https://api.cline.bot/api/v1/users/me/plan/usage-limits";
const IDENTITY_URL: &str = "https://api.cline.bot/api/v1/users/me";
const CREDENTIAL_TARGET: &str = "codexbar-clinepass";
const ENV_KEYS: &[&str] = &["CLINEPASS_API_KEY", "CLINE_API_KEY"];

#[derive(Debug, Deserialize)]
struct LimitsResponse {
    success: bool,
    data: LimitsData,
}

#[derive(Debug, Deserialize)]
struct LimitsData {
    limits: Vec<LimitEntry>,
}

#[derive(Debug, Deserialize)]
struct LimitEntry {
    #[serde(rename = "type")]
    limit_type: String,
    #[serde(rename = "percentUsed")]
    percent_used: f64,
    #[serde(rename = "resetsAt")]
    resets_at: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClineIdentityResponse {
    #[serde(default)]
    active_account_id: Option<String>,
    #[serde(default)]
    user_id: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    email: Option<String>,
}

pub struct ClinePassProvider {
    metadata: ProviderMetadata,
    client: Client,
}

impl ClinePassProvider {
    pub fn new() -> Self {
        Self {
            metadata: ProviderMetadata {
                id: ProviderId::ClinePass,
                display_name: "ClinePass",
                session_label: "5-hour",
                weekly_label: "Weekly",
                supports_opus: true,
                supports_credits: false,
                default_enabled: false,
                is_primary: false,
                dashboard_url: Some("https://app.cline.bot/dashboard/subscription?personal=true"),
                status_page_url: None,
            },
            client: crate::core::credentialed_http_client_builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .unwrap_or_else(|_| Client::new()),
        }
    }

    async fn fetch_limits(&self, key: &str) -> Result<LimitsResponse, ProviderError> {
        let response = self
            .client
            .get(USAGE_URL)
            .bearer_auth(key)
            .header("Accept", "application/json")
            .send()
            .await?;
        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(ProviderError::AuthRequired);
        }
        if !status.is_success() {
            return Err(ProviderError::Other(format!(
                "ClinePass API error: HTTP {status}"
            )));
        }
        response.json().await.map_err(|error| {
            ProviderError::Parse(format!("Failed to parse ClinePass usage: {error}"))
        })
    }

    async fn fetch_identity(&self, key: &str) -> Result<ClineIdentityResponse, ProviderError> {
        let response = self
            .client
            .get(IDENTITY_URL)
            .bearer_auth(key)
            .header("Accept", "application/json")
            .send()
            .await?;
        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(ProviderError::AuthRequired);
        }
        if !status.is_success() {
            return Err(ProviderError::Other(format!(
                "ClinePass identity API error: HTTP {status}"
            )));
        }
        response.json().await.map_err(|error| {
            ProviderError::Parse(format!("Failed to parse ClinePass identity: {error}"))
        })
    }
}

impl Default for ClinePassProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Provider for ClinePassProvider {
    fn id(&self) -> ProviderId {
        ProviderId::ClinePass
    }

    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }

    async fn fetch_usage(&self, ctx: &FetchContext) -> Result<ProviderFetchResult, ProviderError> {
        match ctx.source_mode {
            SourceMode::Auto | SourceMode::OAuth => {
                let key = crate::providers::resolve_api_key(
                    ctx.api_key.as_deref(),
                    CREDENTIAL_TARGET,
                    ENV_KEYS,
                )?;
                let (limits, identity) =
                    tokio::join!(self.fetch_limits(&key), self.fetch_identity(&key));
                let snap = snapshot_from_results(limits, identity)?;
                Ok(ProviderFetchResult::new(snap, "api"))
            }
            SourceMode::Web | SourceMode::Cli => {
                Err(ProviderError::UnsupportedSource(ctx.source_mode))
            }
        }
    }

    fn available_sources(&self) -> Vec<SourceMode> {
        vec![SourceMode::Auto, SourceMode::OAuth]
    }
}

fn parse_iso(raw: Option<&str>) -> Option<DateTime<Utc>> {
    let raw = raw?.trim();
    if raw.is_empty() {
        return None;
    }
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

fn window_for(entry: &LimitEntry) -> Option<(RateWindow, &'static str)> {
    let minutes = match entry.limit_type.as_str() {
        "five_hour" => Some(5 * 60),
        "weekly" => Some(7 * 24 * 60),
        "monthly" => Some(30 * 24 * 60),
        _ => None,
    }?;
    let mut w = RateWindow::new(entry.percent_used.clamp(0.0, 100.0));
    w.window_minutes = Some(minutes);
    w.resets_at = parse_iso(entry.resets_at.as_deref());
    let slot = match entry.limit_type.as_str() {
        "five_hour" => "primary",
        "weekly" => "secondary",
        "monthly" => "tertiary",
        _ => return None,
    };
    Some((w, slot))
}

fn snapshot_from_limits(body: &LimitsResponse) -> Result<UsageSnapshot, ProviderError> {
    if !body.success {
        return Err(ProviderError::Parse(
            "ClinePass response success was false".into(),
        ));
    }
    let mut primary = None;
    let mut secondary = None;
    let mut tertiary = None;
    for limit in &body.data.limits {
        if let Some((w, slot)) = window_for(limit) {
            match slot {
                "primary" => primary = Some(w),
                "secondary" => secondary = Some(w),
                "tertiary" => tertiary = Some(w),
                _ => {}
            }
        }
    }
    let primary = primary.ok_or_else(|| {
        ProviderError::Parse("ClinePass response missing five_hour window".into())
    })?;
    let mut snap = UsageSnapshot::new(primary).with_login_method("API key");
    if let Some(s) = secondary {
        snap = snap.with_secondary(s);
    }
    if let Some(t) = tertiary {
        snap = snap.with_tertiary(t);
    }
    Ok(snap)
}

fn snapshot_from_results(
    limits: Result<LimitsResponse, ProviderError>,
    identity: Result<ClineIdentityResponse, ProviderError>,
) -> Result<UsageSnapshot, ProviderError> {
    let mut snapshot = snapshot_from_limits(&limits?)?;
    let Ok(identity) = identity else {
        return Ok(snapshot);
    };
    let account_id = identity
        .active_account_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .or_else(|| {
            identity
                .user_id
                .as_deref()
                .map(str::trim)
                .filter(|id| !id.is_empty())
        });
    let Some(account_id) = account_id else {
        return Ok(snapshot);
    };

    snapshot = snapshot.with_account_group_id(format!("clinepass:{account_id}"));
    let display_name = identity
        .display_name
        .as_deref()
        .or(identity.name.as_deref())
        .map(str::trim)
        .filter(|display| !display.is_empty());
    if let Some(display_name) = display_name {
        snapshot = snapshot.with_account_display_name(display_name);
    }
    let email = identity
        .email
        .as_deref()
        .map(str::trim)
        .filter(|email| !email.is_empty());
    if let Some(email) = email {
        snapshot = snapshot.with_email(email);
    }
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task_2a_limits_fixture() -> LimitsResponse {
        serde_json::from_str(
            r#"{
                "success": true,
                "data": {
                    "limits": [
                        { "type": "five_hour", "percentUsed": 12.5, "resetsAt": "2026-07-16T15:00:00Z" },
                        { "type": "weekly", "percentUsed": 25, "resetsAt": "2026-07-20T00:00:00Z" },
                        { "type": "monthly", "percentUsed": 40, "resetsAt": null }
                    ]
                }
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn ignores_unknown_limit_types() {
        let body: LimitsResponse = serde_json::from_str(
            r#"{
          "success": true,
          "data": {
            "limits": [
              { "type": "five_hour", "percentUsed": 12.5, "resetsAt": "2026-07-16T15:00:00Z" },
              { "type": "experimental_pool", "percentUsed": 77, "resetsAt": "2026-07-16T15:00:00Z" },
              { "type": "weekly", "percentUsed": 25, "resetsAt": "2026-07-20T00:00:00Z" },
              { "type": "monthly", "percentUsed": 40, "resetsAt": null }
            ]
          }
        }"#,
        )
        .unwrap();
        let snap = snapshot_from_limits(&body).unwrap();
        assert!((snap.primary.used_percent - 12.5).abs() < 0.01);
        assert_eq!(snap.primary.window_minutes, Some(300));
        assert!((snap.secondary.as_ref().unwrap().used_percent - 25.0).abs() < 0.01);
        assert!((snap.tertiary.as_ref().unwrap().used_percent - 40.0).abs() < 0.01);
    }

    #[test]
    fn task_2a_cline_active_account_id_precedes_user_id() {
        let identity: ClineIdentityResponse = serde_json::from_str(
            r#"{
                "activeAccountId": "acct_active",
                "userId": "user_fallback",
                "displayName": "Ada"
            }"#,
        )
        .unwrap();

        let snapshot = snapshot_from_results(Ok(task_2a_limits_fixture()), Ok(identity)).unwrap();

        assert_eq!(
            snapshot.account_group_id.as_deref(),
            Some("clinepass:acct_active")
        );
        assert_eq!(snapshot.account_display_name.as_deref(), Some("Ada"));
    }

    #[test]
    fn task_2a_cline_user_id_is_stable_fallback() {
        let identity: ClineIdentityResponse = serde_json::from_str(
            r#"{ "activeAccountId": "  ", "userId": "user_42", "name": "Grace" }"#,
        )
        .unwrap();

        let snapshot = snapshot_from_results(Ok(task_2a_limits_fixture()), Ok(identity)).unwrap();

        assert_eq!(
            snapshot.account_group_id.as_deref(),
            Some("clinepass:user_42")
        );
        assert_eq!(snapshot.account_display_name.as_deref(), Some("Grace"));
    }

    #[test]
    fn cline_email_only_identity_uses_account_email_not_display_name() {
        let identity: ClineIdentityResponse =
            serde_json::from_str(r#"{ "userId": "user_email", "email": "  ada@example.com  " }"#)
                .unwrap();

        let snapshot = snapshot_from_results(Ok(task_2a_limits_fixture()), Ok(identity)).unwrap();

        assert_eq!(
            snapshot.account_group_id.as_deref(),
            Some("clinepass:user_email")
        );
        assert_eq!(snapshot.account_display_name, None);
        assert_eq!(snapshot.account_email.as_deref(), Some("ada@example.com"));
    }

    #[test]
    fn cline_identity_preserves_display_name_and_email_separately() {
        let identity: ClineIdentityResponse = serde_json::from_str(
            r#"{
                "activeAccountId": "acct_display_email",
                "displayName": "  Ada Lovelace  ",
                "email": "  ada@example.com  "
            }"#,
        )
        .unwrap();

        let snapshot = snapshot_from_results(Ok(task_2a_limits_fixture()), Ok(identity)).unwrap();

        assert_eq!(
            snapshot.account_group_id.as_deref(),
            Some("clinepass:acct_display_email")
        );
        assert_eq!(
            snapshot.account_display_name.as_deref(),
            Some("Ada Lovelace")
        );
        assert_eq!(snapshot.account_email.as_deref(), Some("ada@example.com"));
    }

    #[test]
    fn task_2a_cline_display_only_identity_stays_ungrouped() {
        let identity: ClineIdentityResponse = serde_json::from_str(
            r#"{ "displayName": "Mutable Name", "email": "mutable@example.com" }"#,
        )
        .unwrap();

        let snapshot = snapshot_from_results(Ok(task_2a_limits_fixture()), Ok(identity)).unwrap();

        assert!(snapshot.account_group_id.is_none());
    }

    #[test]
    fn task_2a_cline_identity_error_keeps_successful_usage_ungrouped() {
        let snapshot = snapshot_from_results(
            Ok(task_2a_limits_fixture()),
            Err(ProviderError::AuthRequired),
        )
        .unwrap();

        assert_eq!(snapshot.primary.used_percent, 12.5);
        assert!(snapshot.account_group_id.is_none());
    }

    #[test]
    fn task_2a_cline_usage_error_still_fails() {
        let identity: ClineIdentityResponse =
            serde_json::from_str(r#"{ "activeAccountId": "acct_active", "userId": "user_42" }"#)
                .unwrap();

        let result = snapshot_from_results(
            Err(ProviderError::Other("usage unavailable".into())),
            Ok(identity),
        );

        assert!(matches!(
            result,
            Err(ProviderError::Other(message)) if message == "usage unavailable"
        ));
    }

    #[test]
    fn task_2a_cline_monthly_duration_remains_43200_minutes() {
        let snapshot = snapshot_from_limits(&task_2a_limits_fixture()).unwrap();

        assert_eq!(
            snapshot.tertiary.as_ref().unwrap().window_minutes,
            Some(43_200)
        );
    }

    #[test]
    fn task_2a_cline_sources_remain_bearer_only() {
        let sources = ClinePassProvider::new().available_sources();

        assert_eq!(sources, vec![SourceMode::Auto, SourceMode::OAuth]);
        assert!(!sources.contains(&SourceMode::Web));
    }
}
