//! Xiaomi MiMo provider implementation.
//!
//! Uses browser cookies to read balance and token-plan usage.

use async_trait::async_trait;
use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use reqwest::Client;
use serde::Deserialize;

use crate::core::{
    FetchContext, Provider, ProviderError, ProviderFetchResult, ProviderId, ProviderMetadata,
    RateWindow, SourceMode, UsageSnapshot,
};

const MIMO_API_BASE: &str = "https://platform.xiaomimimo.com/api/v1";

pub struct MiMoProvider {
    metadata: ProviderMetadata,
    client: Client,
}

#[derive(Debug, Deserialize)]
struct BalanceResponse {
    code: i64,
    #[serde(default)]
    message: Option<String>,
    data: Option<BalanceData>,
}

#[derive(Debug, Deserialize)]
struct BalanceData {
    balance: String,
    currency: String,
    #[serde(default, alias = "cashBalance")]
    cash_balance: Option<String>,
    #[serde(default, alias = "giftBalance")]
    gift_balance: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TokenPlanDetailResponse {
    code: i64,
    data: Option<TokenPlanDetailData>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TokenPlanDetailData {
    plan_code: Option<String>,
    current_period_end: Option<String>,
    #[serde(default)]
    expired: bool,
}

#[derive(Debug, Deserialize)]
struct TokenPlanUsageResponse {
    code: i64,
    data: Option<TokenPlanUsageData>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TokenPlanUsageData {
    month_usage: Option<TokenPlanMonthUsage>,
}

#[derive(Debug, Deserialize)]
struct TokenPlanMonthUsage {
    #[serde(default)]
    items: Vec<TokenPlanUsageItem>,
}

#[derive(Debug, Deserialize)]
struct TokenPlanUsageItem {
    used: i64,
    limit: i64,
    percent: f64,
}

impl MiMoProvider {
    pub fn new() -> Self {
        Self {
            metadata: ProviderMetadata {
                id: ProviderId::MiMo,
                display_name: "Xiaomi MiMo",
                session_label: "Tokens",
                weekly_label: "Balance",
                supports_opus: false,
                supports_credits: true,
                default_enabled: false,
                is_primary: false,
                dashboard_url: Some("https://platform.xiaomimimo.com/#/console/balance"),
                status_page_url: None,
            },
            client: crate::core::credentialed_http_client_builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .unwrap_or_else(|_| Client::new()),
        }
    }

    async fn fetch_web(&self, cookie_header: &str) -> Result<UsageSnapshot, ProviderError> {
        let cookie = normalize_cookie_header(cookie_header).ok_or(ProviderError::NoCookies)?;
        let balance: BalanceResponse = self.get_json("balance", &cookie).await?;
        if balance.code == 401 {
            return Err(ProviderError::AuthRequired);
        }
        if balance.code != 0 {
            return Err(ProviderError::Parse(format!(
                "MiMo balance error: {}",
                balance.message.unwrap_or_else(|| balance.code.to_string())
            )));
        }
        let data = balance
            .data
            .ok_or_else(|| ProviderError::Parse("MiMo balance payload missing".into()))?;
        let balance_value = data
            .balance
            .parse::<f64>()
            .map_err(|_| ProviderError::Parse("MiMo balance value invalid".into()))?;

        let detail: Option<TokenPlanDetailResponse> =
            self.get_json("tokenPlan/detail", &cookie).await.ok();
        let usage: Option<TokenPlanUsageResponse> =
            self.get_json("tokenPlan/usage", &cookie).await.ok();
        let snapshot = snapshot_from_parts(
            balance_value,
            data.currency,
            data.cash_balance,
            data.gift_balance,
            detail,
            usage,
        );
        Ok(apply_cookie_identity(snapshot, &cookie))
    }

    async fn get_json<T: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        cookie: &str,
    ) -> Result<T, ProviderError> {
        let response = self
            .client
            .get(format!("{MIMO_API_BASE}/{path}"))
            .header("Cookie", cookie)
            .header("Accept", "application/json, text/plain, */*")
            .header("Origin", "https://platform.xiaomimimo.com")
            .header(
                "Referer",
                "https://platform.xiaomimimo.com/#/console/balance",
            )
            .header("x-timeZone", "UTC+01:00")
            .send()
            .await?;

        if response.status() == reqwest::StatusCode::UNAUTHORIZED
            || response.status() == reqwest::StatusCode::FORBIDDEN
        {
            return Err(ProviderError::AuthRequired);
        }
        if !response.status().is_success() {
            return Err(ProviderError::Other(format!(
                "MiMo API returned status {}",
                response.status()
            )));
        }
        response
            .json::<T>()
            .await
            .map_err(|e| ProviderError::Parse(format!("Failed to parse MiMo response: {e}")))
    }
}

fn normalize_cookie_header(raw: &str) -> Option<String> {
    let known = [
        "api-platform_serviceToken",
        "userId",
        "api-platform_ph",
        "api-platform_slh",
    ];
    let required = ["api-platform_serviceToken", "userId"];
    let mut pairs = Vec::new();
    for chunk in raw.trim().split(';') {
        let Some((name, value)) = chunk.trim().split_once('=') else {
            continue;
        };
        let name = name.trim();
        let value = value.trim();
        if known.contains(&name) && !value.is_empty() {
            pairs.push((name.to_string(), value.to_string()));
        }
    }
    if required
        .iter()
        .all(|required| pairs.iter().any(|(name, _)| name == required))
    {
        pairs.sort_by(|a, b| a.0.cmp(&b.0));
        Some(
            pairs
                .into_iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect::<Vec<_>>()
                .join("; "),
        )
    } else {
        None
    }
}

fn apply_cookie_identity(usage: UsageSnapshot, cookie_header: &str) -> UsageSnapshot {
    let user_id = cookie_header.split(';').find_map(|chunk| {
        let (name, value) = chunk.trim().split_once('=')?;
        (name.trim() == "userId")
            .then(|| value.trim())
            .filter(|value| !value.is_empty())
    });
    match user_id {
        Some(user_id) => usage.with_account_group_id(format!("mimo:{user_id}")),
        None => usage,
    }
}

fn snapshot_from_parts(
    balance: f64,
    currency: String,
    cash_balance: Option<String>,
    gift_balance: Option<String>,
    detail: Option<TokenPlanDetailResponse>,
    usage: Option<TokenPlanUsageResponse>,
) -> UsageSnapshot {
    let detail_data =
        detail.and_then(|response| (response.code == 0).then_some(response.data).flatten());
    let usage_item = usage
        .and_then(|response| (response.code == 0).then_some(response.data).flatten())
        .and_then(|data| data.month_usage)
        .and_then(|month| month.items.into_iter().next());

    let plan_name = detail_data
        .as_ref()
        .and_then(|data| data.plan_code.clone())
        .filter(|plan| !plan.trim().is_empty());
    let period_end = detail_data
        .as_ref()
        .and_then(|data| data.current_period_end.as_deref())
        .and_then(parse_mimo_date);

    let primary = if let Some(item) = usage_item {
        let description = format!("{}/{} tokens", item.used, item.limit);
        if item.limit > 0 {
            RateWindow::with_details(item.percent, Some(43_200), period_end, Some(description))
        } else {
            let mut window = RateWindow::informational(description);
            window.resets_at = period_end;
            window
        }
    } else {
        let mut window = RateWindow::informational("No token-plan usage");
        window.resets_at = period_end;
        window
    };
    let mut secondary = RateWindow::new(0.0);
    secondary.reset_description = Some(balance_description(
        balance,
        &currency,
        cash_balance.as_deref(),
        gift_balance.as_deref(),
    ));

    let mut snapshot = UsageSnapshot::new(primary).with_secondary(secondary);
    if let Some(plan) = plan_name {
        snapshot = snapshot.with_login_method(plan);
    } else {
        snapshot = snapshot.with_login_method(format!("{balance:.2} {currency}"));
    }
    snapshot
}

fn parse_mimo_date(value: &str) -> Option<DateTime<Utc>> {
    NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S")
        .ok()
        .map(|dt| Utc.from_utc_datetime(&dt))
}

fn parse_decimal(value: Option<&str>) -> Option<f64> {
    value?.trim().parse().ok()
}

fn balance_description(
    balance: f64,
    currency: &str,
    cash_balance: Option<&str>,
    gift_balance: Option<&str>,
) -> String {
    let currency = currency.trim();
    let total = format!("{balance:.2} {currency} balance");
    let Some(cash) = parse_decimal(cash_balance) else {
        return total;
    };
    let Some(gift) = parse_decimal(gift_balance) else {
        return total;
    };
    format!("{total} (Paid: {cash:.2} {currency} / Granted: {gift:.2} {currency})")
}

impl Default for MiMoProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Provider for MiMoProvider {
    fn id(&self) -> ProviderId {
        ProviderId::MiMo
    }

    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }

    async fn fetch_usage(&self, ctx: &FetchContext) -> Result<ProviderFetchResult, ProviderError> {
        match ctx.source_mode {
            SourceMode::Auto | SourceMode::Web => {
                let cookie = match ctx.manual_cookie_header.as_deref() {
                    Some(cookie) => cookie.to_string(),
                    None => crate::providers::browser_cookie_header(&["platform.xiaomimimo.com"])?,
                };
                Ok(ProviderFetchResult::new(
                    self.fetch_web(&cookie).await?,
                    "web",
                ))
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

    #[test]
    fn mimo_cookie_requires_service_token_and_user_id() {
        assert!(normalize_cookie_header("api-platform_serviceToken=abc; userId=42").is_some());
        assert!(normalize_cookie_header("api-platform_serviceToken=abc").is_none());
    }

    #[test]
    fn mimo_balance_description_includes_paid_and_granted_components() {
        assert_eq!(
            balance_description(12.5, "CNY", Some("8.25"), Some("4.25")),
            "12.50 CNY balance (Paid: 8.25 CNY / Granted: 4.25 CNY)"
        );
        assert_eq!(
            balance_description(12.5, "CNY", Some("8.25"), None),
            "12.50 CNY balance"
        );
    }

    #[test]
    fn task_2a_mimo_cookie_user_id_sets_group_without_leaking_service_token() {
        let snapshot = apply_cookie_identity(
            UsageSnapshot::new(RateWindow::new(17.0)),
            "api-platform_serviceToken=super-secret-token; userId=user_42",
        );
        let serialized = serde_json::to_string(&snapshot).unwrap();

        assert_eq!(snapshot.account_group_id.as_deref(), Some("mimo:user_42"));
        assert!(!serialized.contains("super-secret-token"));
        assert!(!serialized.contains("serviceToken"));
    }

    #[test]
    fn task_2a_mimo_real_month_item_is_43200_minutes() {
        let usage = Some(TokenPlanUsageResponse {
            code: 0,
            data: Some(TokenPlanUsageData {
                month_usage: Some(TokenPlanMonthUsage {
                    items: vec![TokenPlanUsageItem {
                        used: 250,
                        limit: 1_000,
                        percent: 25.0,
                    }],
                }),
            }),
        });

        let snapshot = snapshot_from_parts(12.5, "CNY".into(), None, None, None, usage);

        assert_eq!(snapshot.primary.window_minutes, Some(43_200));
        assert!(!snapshot.primary.is_informational);
        assert_eq!(
            snapshot.primary.reset_description.as_deref(),
            Some("250/1000 tokens")
        );
    }

    #[test]
    fn mimo_non_positive_month_limits_stay_informational_with_period_context() {
        let observed = [0, -1].map(|limit| {
            let detail = Some(TokenPlanDetailResponse {
                code: 0,
                data: Some(TokenPlanDetailData {
                    plan_code: Some("pro".into()),
                    current_period_end: Some("2026-09-01 12:30:00".into()),
                    expired: false,
                }),
            });
            let usage = Some(TokenPlanUsageResponse {
                code: 0,
                data: Some(TokenPlanUsageData {
                    month_usage: Some(TokenPlanMonthUsage {
                        items: vec![TokenPlanUsageItem {
                            used: 10,
                            limit,
                            percent: 25.0,
                        }],
                    }),
                }),
            });

            let snapshot = snapshot_from_parts(12.5, "CNY".into(), None, None, detail, usage);

            (
                snapshot.primary.is_informational,
                snapshot.primary.window_minutes,
                snapshot.primary.reset_description,
                snapshot.primary.resets_at.map(|value| value.to_rfc3339()),
            )
        });

        assert_eq!(
            observed,
            [
                (
                    true,
                    None,
                    Some("10/0 tokens".to_string()),
                    Some("2026-09-01T12:30:00+00:00".to_string()),
                ),
                (
                    true,
                    None,
                    Some("10/-1 tokens".to_string()),
                    Some("2026-09-01T12:30:00+00:00".to_string()),
                ),
            ]
        );
    }

    #[test]
    fn task_2a_mimo_no_month_item_is_informational_not_monthly() {
        let snapshot = snapshot_from_parts(12.5, "CNY".into(), None, None, None, None);

        assert_eq!(snapshot.primary.window_minutes, None);
        assert!(snapshot.primary.is_informational);
        assert_eq!(
            snapshot.primary.reset_description.as_deref(),
            Some("No token-plan usage")
        );
    }

    #[test]
    fn task_2a_mimo_sources_remain_browser_only() {
        let sources = MiMoProvider::new().available_sources();

        assert_eq!(sources, vec![SourceMode::Auto, SourceMode::Web]);
        assert!(!sources.contains(&SourceMode::OAuth));
        assert!(!sources.contains(&SourceMode::Cli));
    }
}
