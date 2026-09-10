//! Augment provider implementation
//!
//! Fetches usage data from Augment Code AI
//! Augment stores auth tokens and config locally

mod keepalive;

// Re-exports for future session management
#[allow(unused_imports)]
pub use keepalive::{AugmentSessionKeepalive, KeepaliveConfig};

use async_trait::async_trait;
use regex_lite::Regex;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;

use crate::core::{
    FetchContext, Provider, ProviderError, ProviderFetchResult, ProviderId, ProviderMetadata,
    RateWindow, SourceMode, UsageSnapshot,
};

/// Augment provider
pub struct AugmentProvider {
    metadata: ProviderMetadata,
}

impl AugmentProvider {
    pub fn new() -> Self {
        Self {
            metadata: ProviderMetadata {
                id: ProviderId::Augment,
                display_name: "Augment",
                session_label: "Session",
                weekly_label: "Monthly",
                supports_opus: false,
                supports_credits: true,
                default_enabled: false,
                is_primary: false,
                dashboard_url: Some("https://app.augmentcode.com/account"),
                status_page_url: Some("https://status.augmentcode.com"),
            },
        }
    }

    /// Get Augment config directory
    fn get_augment_config_path() -> Option<PathBuf> {
        #[cfg(target_os = "windows")]
        {
            dirs::config_dir().map(|p| p.join("augment"))
        }
        #[cfg(not(target_os = "windows"))]
        {
            dirs::home_dir().map(|p| p.join(".augment"))
        }
    }

    /// Find Augment CLI
    fn which_augment() -> Option<PathBuf> {
        let possible_paths = [
            which::which("augment").ok(),
            which::which("auggie").ok(),
            #[cfg(target_os = "windows")]
            dirs::data_local_dir().map(|p| p.join("Programs").join("Augment").join("augment.exe")),
            #[cfg(target_os = "windows")]
            dirs::data_local_dir().map(|p| p.join("Programs").join("Augment").join("auggie.exe")),
            #[cfg(not(target_os = "windows"))]
            None,
            #[cfg(not(target_os = "windows"))]
            None,
        ];

        possible_paths.into_iter().flatten().find(|p| p.exists())
    }

    /// Read Augment auth token
    async fn read_auth_token(&self) -> Result<String, ProviderError> {
        let config_path = Self::get_augment_config_path()
            .ok_or_else(|| ProviderError::NotInstalled("Augment config not found".to_string()))?;

        // Check for token file
        let token_file = config_path.join("auth.json");
        if token_file.exists() {
            let content = tokio::fs::read_to_string(&token_file)
                .await
                .map_err(|e| ProviderError::Other(e.to_string()))?;

            let json: serde_json::Value =
                serde_json::from_str(&content).map_err(|e| ProviderError::Parse(e.to_string()))?;

            if let Some(token) = json.get("access_token").and_then(|v| v.as_str()) {
                return Ok(token.to_string());
            }
        }

        // Check for credentials in VS Code extension settings
        let vscode_settings = Self::get_vscode_augment_settings().await;
        if let Some(token) = vscode_settings {
            return Ok(token);
        }

        Err(ProviderError::AuthRequired)
    }

    async fn get_vscode_augment_settings() -> Option<String> {
        #[cfg(target_os = "windows")]
        let settings_path = dirs::config_dir().map(|p| {
            p.join("Code")
                .join("User")
                .join("globalStorage")
                .join("augment.augment-vscode")
                .join("auth.json")
        });
        #[cfg(not(target_os = "windows"))]
        let settings_path = dirs::config_dir().map(|p| {
            p.join("Code")
                .join("User")
                .join("globalStorage")
                .join("augment.augment-vscode")
                .join("auth.json")
        });

        if let Some(path) = settings_path
            && path.exists()
            && let Ok(content) = tokio::fs::read_to_string(&path).await
            && let Ok(json) = serde_json::from_str::<serde_json::Value>(&content)
            && let Some(token) = json.get("accessToken").and_then(|v| v.as_str())
        {
            return Some(token.to_string());
        }

        None
    }

    /// Fetch usage via Augment API
    async fn fetch_via_web(&self) -> Result<UsageSnapshot, ProviderError> {
        let token = self.read_auth_token().await?;

        let client = crate::core::credentialed_http_client_builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| ProviderError::Other(e.to_string()))?;

        let resp = client
            .get("https://api.augmentcode.com/v1/user/usage")
            .header("Authorization", format!("Bearer {}", token))
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(ProviderError::AuthRequired);
        }

        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| ProviderError::Parse(e.to_string()))?;

        self.parse_usage_response(&json)
    }

    fn parse_usage_response(
        &self,
        json: &serde_json::Value,
    ) -> Result<UsageSnapshot, ProviderError> {
        let used = json
            .get("used_credits")
            .or_else(|| json.get("usage"))
            .and_then(|v| v.as_f64())
            .filter(|value| value.is_finite());

        let limit = json
            .get("credit_limit")
            .or_else(|| json.get("limit"))
            .and_then(|v| v.as_f64())
            .filter(|value| value.is_finite() && *value > 0.0);

        let used_percent = match (used, limit) {
            (Some(used), Some(limit)) => (used / limit) * 100.0,
            _ => 0.0,
        };

        let email = json
            .get("email")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let plan = json
            .get("plan")
            .or_else(|| json.get("subscription"))
            .and_then(|v| v.as_str())
            .unwrap_or("Augment");

        let hard_monthly = used.is_some() && limit.is_some() && has_explicit_monthly_period(json);
        let window =
            RateWindow::with_details(used_percent, hard_monthly.then_some(43_200), None, None);
        let mut usage = UsageSnapshot::new(window).with_login_method(plan);

        if let Some(email) = email {
            usage = usage.with_email(email);
        }

        Ok(usage)
    }

    async fn fetch_via_cli(&self) -> Result<UsageSnapshot, ProviderError> {
        let cli_path = Self::which_augment().ok_or_else(|| {
            ProviderError::NotInstalled(
                "Augment CLI not found. Install from https://www.augmentcode.com".to_string(),
            )
        })?;

        #[cfg(windows)]
        const CREATE_NO_WINDOW: u32 = 0x08000000;

        let mut cmd = Command::new(cli_path);
        cmd.args(["account", "status"]);
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);

        let output = timeout(Duration::from_secs(15), cmd.output())
            .await
            .map_err(|_| ProviderError::Timeout)?
            .map_err(|e| ProviderError::Other(format!("Failed to run Augment CLI: {e}")))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !output.status.success() {
            let message = if stderr.trim().is_empty() {
                stdout.trim()
            } else {
                stderr.trim()
            };
            if message.contains("Authentication failed") || message.contains("auggie login") {
                return Err(ProviderError::AuthRequired);
            }
            return Err(ProviderError::Other(format!(
                "Augment CLI failed: {}",
                message
            )));
        }
        if stdout.trim().is_empty() {
            return Err(ProviderError::Parse(
                "Augment CLI returned no account status output".to_string(),
            ));
        }
        parse_auggie_account_status(&stdout)
    }

    #[allow(dead_code)]
    /// Probe CLI for detection
    async fn probe_cli(&self) -> Result<UsageSnapshot, ProviderError> {
        self.fetch_via_cli().await.or_else(|_| {
            let augment_path = Self::which_augment();
            let config_path = Self::get_augment_config_path();

            if augment_path.map(|p| p.exists()).unwrap_or(false)
                || config_path.map(|p| p.exists()).unwrap_or(false)
            {
                let usage = UsageSnapshot::new(RateWindow::new(0.0))
                    .with_login_method("Augment (installed)");
                Ok(usage)
            } else {
                Err(ProviderError::NotInstalled(
                    "Augment not found. Install from https://www.augmentcode.com".to_string(),
                ))
            }
        })
    }
}

fn has_explicit_monthly_period(json: &serde_json::Value) -> bool {
    ["period", "cycle", "interval"]
        .iter()
        .filter_map(|key| json.get(key).and_then(|value| value.as_str()))
        .any(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "month" | "monthly" | "30-day" | "30 day" | "30 days"
            )
        })
}

impl Default for AugmentProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Provider for AugmentProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Augment
    }

    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }

    async fn fetch_usage(&self, ctx: &FetchContext) -> Result<ProviderFetchResult, ProviderError> {
        tracing::debug!("Fetching Augment usage");

        match ctx.source_mode {
            SourceMode::Auto => {
                if let Ok(usage) = self.fetch_via_cli().await {
                    return Ok(ProviderFetchResult::new(usage, "cli"));
                }
                let usage = self.fetch_via_web().await?;
                Ok(ProviderFetchResult::new(usage, "web"))
            }
            SourceMode::Web => {
                let usage = self.fetch_via_web().await?;
                Ok(ProviderFetchResult::new(usage, "web"))
            }
            SourceMode::Cli => {
                let usage = self.fetch_via_cli().await?;
                Ok(ProviderFetchResult::new(usage, "cli"))
            }
            SourceMode::OAuth => Err(ProviderError::UnsupportedSource(SourceMode::OAuth)),
        }
    }

    fn available_sources(&self) -> Vec<SourceMode> {
        vec![SourceMode::Auto, SourceMode::Web, SourceMode::Cli]
    }

    fn supports_web(&self) -> bool {
        true
    }

    fn supports_cli(&self) -> bool {
        true
    }
}

fn parse_auggie_account_status(output: &str) -> Result<UsageSnapshot, ProviderError> {
    static MONTHLY_RE: OnceLock<Regex> = OnceLock::new();
    static REMAINING_RE: OnceLock<Regex> = OnceLock::new();
    static LEGACY_RE: OnceLock<Regex> = OnceLock::new();
    static LEGACY_REMAINING_RE: OnceLock<Regex> = OnceLock::new();
    static BILLING_RE: OnceLock<Regex> = OnceLock::new();

    let monthly_re = MONTHLY_RE.get_or_init(|| {
        Regex::new(r"(?i)(?:^|\s)([+-]?[\d,]+(?:\.\d+)?)\s+credits\s*/\s*month")
            .expect("valid regex")
    });
    let remaining_re = REMAINING_RE
        .get_or_init(|| Regex::new(r"(?i)([\d,]+)\s+credits\s+remaining").expect("valid regex"));
    let legacy_re = LEGACY_RE.get_or_init(|| {
        Regex::new(r"(?i)([\d,]+)\s*/\s*([\d,]+)\s+credits used").expect("valid regex")
    });
    let legacy_remaining_re = LEGACY_REMAINING_RE
        .get_or_init(|| Regex::new(r"(?i)([\d,]+)\s+remaining").expect("valid regex"));
    let billing_re = BILLING_RE
        .get_or_init(|| Regex::new(r"(?i)billing cycle.*ends\s+([\d/]+)").expect("valid regex"));

    let mut max_credits: Option<f64> = None;
    let mut remaining: Option<f64> = None;
    let mut used: Option<f64> = None;
    let mut total: Option<f64> = None;
    let mut reset_description: Option<String> = None;

    for line in output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if let Some(caps) = monthly_re.captures(line) {
            let value = parse_credit_number(&caps[1]);
            max_credits = value;
            total = total.or(value);
        }

        if let Some(caps) = remaining_re.captures(line) {
            remaining = parse_credit_number(&caps[1]);
        } else if line.to_ascii_lowercase().contains("credits used")
            && let Some(caps) = legacy_remaining_re.captures(line)
        {
            remaining = parse_credit_number(&caps[1]);
        }

        if let Some(caps) = legacy_re.captures(line) {
            used = parse_credit_number(&caps[1]);
            total = parse_credit_number(&caps[2]);
        }

        if let Some(caps) = billing_re.captures(line) {
            reset_description = Some(format!("ends {}", &caps[1]));
        }
    }

    let remaining = remaining.ok_or_else(|| {
        ProviderError::Parse("Could not extract Augment remaining credits".to_string())
    })?;
    let total = total.or(max_credits).ok_or_else(|| {
        ProviderError::Parse("Could not extract Augment credit limit".to_string())
    })?;
    let used = used.unwrap_or_else(|| (total - remaining).max(0.0));
    let has_hard_monthly_limit = max_credits
        .is_some_and(|credits| credits.is_finite() && credits > 0.0)
        && total.is_finite()
        && total > 0.0;
    let used_percent = if has_hard_monthly_limit {
        (used / total) * 100.0
    } else {
        0.0
    };

    let window = RateWindow::with_details(
        used_percent,
        has_hard_monthly_limit.then_some(43_200),
        None,
        reset_description,
    );
    let plan = max_credits
        .map(|credits| format!("{} credits/month", format_integer_credits(credits)))
        .unwrap_or_else(|| "Augment".to_string());
    Ok(UsageSnapshot::new(window).with_login_method(plan))
}

fn parse_credit_number(value: &str) -> Option<f64> {
    value.replace(',', "").trim().parse::<f64>().ok()
}

fn format_integer_credits(value: f64) -> String {
    let mut digits = format!("{:.0}", value).chars().rev().collect::<Vec<_>>();
    let mut out = String::new();
    for (idx, ch) in digits.drain(..).enumerate() {
        if idx > 0 && idx % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn augment_web_missing_limit_does_not_use_a_fallback() {
        let usage = AugmentProvider::new()
            .parse_usage_response(&json!({
                "used_credits": 50,
                "period": "monthly",
                "plan": "Augment Pro"
            }))
            .unwrap();

        assert_eq!(usage.primary.used_percent, 0.0);
        assert_eq!(usage.primary.window_minutes, None);
    }

    #[test]
    fn augment_web_requires_real_month_limit_for_hard_month() {
        let provider = AugmentProvider::new();
        let monthly = provider
            .parse_usage_response(&json!({
                "used_credits": 50,
                "credit_limit": 200,
                "cycle": "month",
                "plan": "Augment Pro",
                "email": "display-only@example.com"
            }))
            .unwrap();
        let unspecified = provider
            .parse_usage_response(&json!({
                "used_credits": 50,
                "credit_limit": 200,
                "plan": "Augment Pro"
            }))
            .unwrap();

        assert_eq!(monthly.primary.used_percent, 25.0);
        assert_eq!(monthly.primary.window_minutes, Some(43_200));
        assert_eq!(monthly.account_group_id, None);
        assert_eq!(unspecified.primary.used_percent, 25.0);
        assert_eq!(unspecified.primary.window_minutes, None);
    }

    #[test]
    fn augment_web_invalid_limits_do_not_create_hard_months() {
        let provider = AugmentProvider::new();
        let fixtures = [
            json!({"usage": 50, "limit": "NaN", "interval": "monthly"}),
            json!({"usage": 50, "limit": 0, "interval": "monthly"}),
        ];

        for fixture in fixtures {
            let usage = provider.parse_usage_response(&fixture).unwrap();

            assert_eq!(usage.primary.used_percent, 0.0);
            assert_eq!(usage.primary.window_minutes, None);
        }
    }

    #[test]
    fn parses_current_auggie_account_status() {
        let usage = parse_auggie_account_status(
            r#"
            319,054 credits remaining                     Max Plan
            450,000 credits / month
            9 days remaining in this billing cycle (ends 6/9/2026)
            "#,
        )
        .unwrap();

        assert!((usage.primary.used_percent - 29.098).abs() < 0.01);
        assert_eq!(usage.primary.window_minutes, Some(43_200));
        assert_eq!(usage.login_method.as_deref(), Some("450,000 credits/month"));
        assert_eq!(
            usage.primary.reset_description.as_deref(),
            Some("ends 6/9/2026")
        );
    }

    #[test]
    fn parses_legacy_auggie_account_status() {
        let usage = parse_auggie_account_status(
            r#"
            Max Plan 450,000 credits / month
            11,657 remaining · 953,170 / 964,827 credits used
            2 days remaining in this billing cycle (ends 1/8/2026)
            "#,
        )
        .unwrap();

        assert!((usage.primary.used_percent - 98.79).abs() < 0.01);
        assert_eq!(usage.primary.window_minutes, Some(43_200));
        assert_eq!(usage.login_method.as_deref(), Some("450,000 credits/month"));
    }

    #[test]
    fn zero_cli_monthly_limit_is_not_hard_even_with_legacy_total() {
        let usage =
            parse_auggie_account_status("0 credits / month\n50 remaining · 50 / 100 credits used")
                .unwrap();

        assert_eq!(usage.primary.used_percent, 0.0);
        assert_eq!(usage.primary.window_minutes, None);
    }

    #[test]
    fn decimal_cli_monthly_limit_uses_the_complete_token() {
        let usage = parse_auggie_account_status(
            "Max Plan 450,000.5 credits / month\n50 remaining · 50 / 100 credits used",
        )
        .unwrap();

        assert_eq!(usage.primary.used_percent, 50.0);
        assert_eq!(usage.primary.window_minutes, Some(43_200));
        assert_eq!(usage.login_method.as_deref(), Some("450,000 credits/month"));
    }

    #[test]
    fn negative_cli_monthly_limit_is_not_reinterpreted_as_positive() {
        let usage =
            parse_auggie_account_status("-5 credits / month\n50 remaining · 50 / 100 credits used")
                .unwrap();

        assert_eq!(usage.primary.used_percent, 0.0);
        assert_eq!(usage.primary.window_minutes, None);
    }

    #[test]
    fn negative_decimal_cli_monthly_limit_is_not_reinterpreted_as_positive() {
        let usage = parse_auggie_account_status(
            "-0.5 credits / month\n50 remaining · 50 / 100 credits used",
        )
        .unwrap();

        assert_eq!(usage.primary.used_percent, 0.0);
        assert_eq!(usage.primary.window_minutes, None);
    }

    #[test]
    fn rejects_auggie_status_without_a_credit_total() {
        let error = parse_auggie_account_status(
            "319,054 credits remaining\n9 days remaining in this billing cycle (ends 6/9/2026)",
        )
        .unwrap_err();

        assert!(error.to_string().contains("credit limit"));
    }
}
