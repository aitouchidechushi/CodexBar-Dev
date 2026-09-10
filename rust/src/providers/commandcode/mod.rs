//! Command Code provider implementation.
//!
//! Uses a browser session cookie to fetch monthly and purchased credit balances.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use regex_lite::Regex;
use reqwest::Client;
use serde_json::Value;

use crate::core::{
    CostSnapshot, FetchContext, Provider, ProviderError, ProviderFetchResult, ProviderId,
    ProviderMetadata, RateWindow, SourceMode, UsageSnapshot,
};

const COMMAND_CODE_API_BASE: &str = "https://api.commandcode.ai";
const COMMAND_CODE_CREDITS_PATH: &str = "/internal/billing/credits";
const COMMAND_CODE_SUBSCRIPTIONS_PATH: &str = "/internal/billing/subscriptions";

pub struct CommandCodeProvider {
    metadata: ProviderMetadata,
    client: Client,
}

impl CommandCodeProvider {
    pub fn new() -> Self {
        Self {
            metadata: ProviderMetadata {
                id: ProviderId::CommandCode,
                display_name: "Command Code",
                session_label: "Credits",
                weekly_label: "Monthly",
                supports_opus: false,
                supports_credits: true,
                default_enabled: false,
                is_primary: false,
                dashboard_url: Some("https://commandcode.ai"),
                status_page_url: None,
            },
            client: crate::core::credentialed_http_client_builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .unwrap_or_else(|_| Client::new()),
        }
    }

    async fn fetch_web(&self, cookie_header: &str) -> Result<ProviderFetchResult, ProviderError> {
        let cookie_header =
            normalize_cookie_header(cookie_header).ok_or_else(|| ProviderError::NoCookies)?;
        let credits = self
            .get_json(
                &format!("{COMMAND_CODE_API_BASE}{COMMAND_CODE_CREDITS_PATH}"),
                &cookie_header,
            )
            .await?;
        let subscription = self
            .get_json(
                &format!("{COMMAND_CODE_API_BASE}{COMMAND_CODE_SUBSCRIPTIONS_PATH}"),
                &cookie_header,
            )
            .await
            .ok();
        result_from_payloads(&credits, subscription.as_ref())
    }

    async fn get_json(&self, url: &str, cookie_header: &str) -> Result<Value, ProviderError> {
        let response = self
            .client
            .get(url)
            .header("Cookie", cookie_header)
            .header("Accept", "application/json, text/plain, */*")
            .header("Origin", "https://commandcode.ai")
            .header("Referer", "https://commandcode.ai/")
            .send()
            .await?;

        if response.status() == reqwest::StatusCode::UNAUTHORIZED
            || response.status() == reqwest::StatusCode::FORBIDDEN
        {
            return Err(ProviderError::AuthRequired);
        }
        if !response.status().is_success() {
            return Err(ProviderError::Other(format!(
                "Command Code API returned status {}",
                response.status()
            )));
        }
        response.json::<Value>().await.map_err(|e| {
            ProviderError::Parse(format!("Failed to parse Command Code response: {e}"))
        })
    }
}

fn normalize_cookie_header(raw: &str) -> Option<String> {
    let extracted;
    let mut header = raw.trim();
    if let Some(cookie_header) = cookie_header_from_curl(raw) {
        extracted = cookie_header;
        header = extracted.trim();
    } else if looks_like_curl_capture(header) {
        return None;
    }
    if header
        .get(.."cookie:".len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("cookie:"))
    {
        header = header["cookie:".len()..].trim();
    }
    if header.is_empty() {
        return None;
    }
    if !header.contains('=') && !header.contains(';') {
        return Some(format!("__Secure-better-auth.session_token={header}"));
    }

    let mut cookies = Vec::new();
    for chunk in header.split(';') {
        let Some((name, value)) = chunk.trim().split_once('=') else {
            continue;
        };
        let name = name.trim();
        let value = value.trim();
        if name.is_empty() || value.is_empty() {
            continue;
        }
        cookies.push(format!("{name}={value}"));
    }
    if !cookies.is_empty() {
        Some(cookies.join("; "))
    } else {
        None
    }
}

fn cookie_header_from_curl(raw: &str) -> Option<String> {
    let re =
        Regex::new(r#"(?s)(?:^|\s)(?:-H|--header)(?:\s+|=)(?:'([^']*)'|"([^"]*)"|(\S+))"#).ok()?;
    re.captures_iter(raw).find_map(|caps| {
        let field = caps
            .get(1)
            .or_else(|| caps.get(2))
            .or_else(|| caps.get(3))?
            .as_str();
        let field = unescape_shell_segment(field);
        let (name, value) = split_header(&field)?;
        name.eq_ignore_ascii_case("cookie")
            .then(|| value.trim().to_string())
            .filter(|value| !value.is_empty())
    })
}

fn looks_like_curl_capture(raw: &str) -> bool {
    let lower = raw.trim_start().to_ascii_lowercase();
    lower.starts_with("curl ") || lower.starts_with("curl.exe ")
}

fn split_header(field: &str) -> Option<(&str, &str)> {
    let colon = field.find(':')?;
    Some((field[..colon].trim(), field[colon + 1..].trim()))
}

fn unescape_shell_segment(raw: &str) -> String {
    let mut output = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(next) = chars.next() {
                output.push(next);
            }
        } else {
            output.push(ch);
        }
    }
    output
}

fn result_from_payloads(
    credits_payload: &Value,
    subscription_payload: Option<&Value>,
) -> Result<ProviderFetchResult, ProviderError> {
    let credits = credits_payload
        .get("credits")
        .ok_or_else(|| ProviderError::Parse("Command Code credits object missing".into()))?;
    let monthly = number(credits.get("monthlyCredits"))
        .ok_or_else(|| ProviderError::Parse("Command Code monthlyCredits missing".into()))?;
    if !monthly.is_finite() {
        return Err(ProviderError::Parse(
            "Command Code monthlyCredits must be finite".into(),
        ));
    }
    let purchased = number(credits.get("purchasedCredits"))
        .filter(|value| value.is_finite())
        .unwrap_or(0.0);
    let premium = number(credits.get("premiumMonthlyCredits")).unwrap_or(0.0);
    let open_source = number(credits.get("opensourceMonthlyCredits")).unwrap_or(0.0);
    let total_monthly = premium + open_source;
    let has_hard_monthly_limit = total_monthly.is_finite() && total_monthly > 0.0;
    let used_percent = if has_hard_monthly_limit {
        ((total_monthly - monthly).max(0.0) / total_monthly * 100.0).clamp(0.0, 100.0)
    } else {
        0.0
    };
    let period_end = subscription_payload
        .and_then(|root| root.get("data"))
        .and_then(|data| data.get("currentPeriodEnd"))
        .and_then(|value| value.as_str())
        .and_then(parse_datetime);
    let plan = subscription_payload
        .and_then(|root| root.get("data"))
        .and_then(|data| data.get("planId"))
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty());

    let mut primary = RateWindow::with_details(
        used_percent,
        has_hard_monthly_limit.then_some(43_200),
        period_end,
        Some(format!("{monthly:.2} monthly credits remaining")),
    );
    if !primary.used_percent.is_finite() {
        primary.used_percent = 0.0;
    }
    let mut secondary = RateWindow::new(0.0);
    secondary.reset_description = Some(format!("{purchased:.2} purchased credits"));

    let mut snapshot = UsageSnapshot::new(primary).with_secondary(secondary);
    if let Some(plan) = plan {
        snapshot = snapshot.with_login_method(plan.to_string());
    }
    let mut cost = CostSnapshot::new((total_monthly - monthly).max(0.0), "USD", "monthly credits");
    if has_hard_monthly_limit {
        cost = cost.with_limit(total_monthly);
    }
    Ok(ProviderFetchResult::new(snapshot, "web").with_cost(cost))
}

fn number(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

fn parse_datetime(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|date| date.with_timezone(&Utc))
}

impl Default for CommandCodeProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Provider for CommandCodeProvider {
    fn id(&self) -> ProviderId {
        ProviderId::CommandCode
    }

    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }

    async fn fetch_usage(&self, ctx: &FetchContext) -> Result<ProviderFetchResult, ProviderError> {
        match ctx.source_mode {
            SourceMode::Auto | SourceMode::Web => {
                let cookie = match ctx.manual_cookie_header.as_deref() {
                    Some(cookie) => cookie.to_string(),
                    None => crate::providers::browser_cookie_header(&["commandcode.ai"])?,
                };
                self.fetch_web(&cookie).await
            }
            SourceMode::OAuth | SourceMode::Cli => {
                Err(ProviderError::UnsupportedSource(ctx.source_mode))
            }
        }
    }

    fn available_sources(&self) -> Vec<SourceMode> {
        vec![SourceMode::Auto, SourceMode::Web]
    }

    fn supports_web(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn command_code_accepts_bare_session_token() {
        assert_eq!(
            normalize_cookie_header("abc123").as_deref(),
            Some("__Secure-better-auth.session_token=abc123")
        );
    }

    #[test]
    fn command_code_accepts_production_session_cookies() {
        assert_eq!(
            normalize_cookie_header(
                "__Secure-commandcode_prod_.session_token=token; __Secure-commandcode_prod_.session_data=data; stripe=ignored"
            )
            .as_deref(),
            Some("__Secure-commandcode_prod_.session_token=token; __Secure-commandcode_prod_.session_data=data; stripe=ignored")
        );
    }

    #[test]
    fn command_code_preserves_unknown_full_cookie_header() {
        assert_eq!(
            normalize_cookie_header("Cookie: sidebar=value; stripe_mid=mid").as_deref(),
            Some("sidebar=value; stripe_mid=mid")
        );
    }

    #[test]
    fn command_code_extracts_cookie_header_from_curl() {
        let curl = r#"curl 'https://commandcode.ai' -H 'User-Agent: Browser' -H 'Cookie: __Secure-commandcode_prod_.session_token=token; __Secure-commandcode_prod_.session_data=data' "#;
        assert_eq!(
            normalize_cookie_header(curl).as_deref(),
            Some(
                "__Secure-commandcode_prod_.session_token=token; __Secure-commandcode_prod_.session_data=data"
            )
        );
    }

    #[test]
    fn command_code_rejects_curl_without_cookie_header() {
        let curl = r#"curl 'https://commandcode.ai' -H 'User-Agent: Browser'"#;
        assert_eq!(normalize_cookie_header(curl), None);
    }

    #[test]
    fn command_code_rejects_empty_or_malformed_cookie_header() {
        assert_eq!(normalize_cookie_header("Cookie:   "), None);
        assert_eq!(normalize_cookie_header("not-a-cookie; also-bad"), None);
    }

    #[test]
    fn command_code_result_uses_monthly_credits() {
        let result = result_from_payloads(
            &json!({"credits":{"monthlyCredits":25,"purchasedCredits":2,"premiumMonthlyCredits":100}}),
            None,
        )
        .unwrap();
        assert_eq!(result.usage.primary.used_percent, 75.0);
    }

    #[test]
    fn command_code_rejects_non_finite_required_monthly_credits() {
        let rejected = ["NaN", "inf", "-inf"].map(|monthly| {
            matches!(
                result_from_payloads(
                    &json!({
                        "credits": {
                            "monthlyCredits": monthly,
                            "purchasedCredits": 2,
                            "premiumMonthlyCredits": 100
                        }
                    }),
                    None,
                ),
                Err(ProviderError::Parse(_))
            )
        });

        assert_eq!(rejected, [true, true, true]);
    }

    #[test]
    fn command_code_ignores_non_finite_optional_purchased_credits() {
        let visible = ["NaN", "inf", "-inf"].map(|purchased| {
            let result = result_from_payloads(
                &json!({
                    "credits": {
                        "monthlyCredits": 25,
                        "purchasedCredits": purchased,
                        "premiumMonthlyCredits": 100
                    }
                }),
                None,
            )
            .expect("optional purchased credits must not fail the provider");

            (
                result.usage.primary.reset_description,
                result
                    .usage
                    .secondary
                    .and_then(|window| window.reset_description),
            )
        });

        assert_eq!(
            visible,
            [
                (
                    Some("25.00 monthly credits remaining".to_string()),
                    Some("0.00 purchased credits".to_string()),
                ),
                (
                    Some("25.00 monthly credits remaining".to_string()),
                    Some("0.00 purchased credits".to_string()),
                ),
                (
                    Some("25.00 monthly credits remaining".to_string()),
                    Some("0.00 purchased credits".to_string()),
                ),
            ]
        );
    }

    #[test]
    fn command_code_marks_valid_total_as_hard_month() {
        let result = result_from_payloads(
            &json!({
                "credits": {
                    "monthlyCredits": 30,
                    "purchasedCredits": 4,
                    "premiumMonthlyCredits": 100,
                    "opensourceMonthlyCredits": 20
                }
            }),
            Some(&json!({
                "data": {
                    "currentPeriodEnd": "2026-09-01T00:00:00Z",
                    "planId": "premium"
                }
            })),
        )
        .unwrap();

        assert_eq!(result.usage.primary.used_percent, 75.0);
        assert_eq!(result.usage.primary.window_minutes, Some(43_200));
        assert_eq!(
            result.usage.primary.reset_description.as_deref(),
            Some("30.00 monthly credits remaining")
        );
        assert_eq!(
            result
                .usage
                .secondary
                .as_ref()
                .and_then(|window| window.reset_description.as_deref()),
            Some("4.00 purchased credits")
        );
        assert_eq!(
            result
                .usage
                .primary
                .resets_at
                .map(|value| value.to_rfc3339()),
            Some("2026-09-01T00:00:00+00:00".to_string())
        );
        let cost = result.cost.as_ref().unwrap();
        assert_eq!(cost.used, 90.0);
        assert_eq!(cost.limit, Some(120.0));
        assert_eq!(result.usage.account_group_id, None);
    }

    #[test]
    fn command_code_rejects_invalid_totals_as_hard_months() {
        let fixtures = [
            json!({"credits":{"monthlyCredits":25}}),
            json!({"credits":{"monthlyCredits":25,"premiumMonthlyCredits":"NaN"}}),
            json!({"credits":{"monthlyCredits":25,"premiumMonthlyCredits":0}}),
            json!({"credits":{"monthlyCredits":25,"premiumMonthlyCredits":-10}}),
        ];

        for fixture in fixtures {
            let result = result_from_payloads(&fixture, None).unwrap();

            assert_eq!(result.usage.primary.window_minutes, None);
            assert_eq!(result.cost.as_ref().and_then(|cost| cost.limit), None);
        }
    }
}
