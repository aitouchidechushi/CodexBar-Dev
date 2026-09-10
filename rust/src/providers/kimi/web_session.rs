use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::core::ProviderError;

const KIMI_WEB_REFRESH_URL: &str =
    "https://auth.kimi.com/api/account.gateway.v1.AuthService/RefreshToken";
const KIMI_WEB_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36";

#[derive(Clone, Default, PartialEq, Eq)]
pub struct KimiWebRequestContext {
    pub traffic_id: Option<String>,
    pub device_id: Option<String>,
    pub session_id: Option<String>,
}

pub struct KimiWebTokenPair {
    pub access_token: String,
    pub refresh_token: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct KimiWebRefreshResponse {
    access_token: String,
    refresh_token: String,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawKimiWebRequestContext {
    user_id: Option<String>,
    web_id: Option<String>,
    ssid: Option<String>,
}

pub fn parse_web_request_context(raw: Option<&str>) -> KimiWebRequestContext {
    let parsed = raw
        .and_then(|value| serde_json::from_str::<RawKimiWebRequestContext>(value).ok())
        .unwrap_or_default();
    KimiWebRequestContext {
        traffic_id: clean_context_value(parsed.user_id),
        device_id: clean_context_value(parsed.web_id),
        session_id: clean_context_value(parsed.ssid),
    }
}

pub fn web_refresh_token_fingerprint(refresh_token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(refresh_token.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn clean_context_value(value: Option<String>) -> Option<String> {
    value.map(|value| value.trim().to_string()).filter(|value| {
        !value.is_empty() && value.len() <= 512 && !value.chars().any(char::is_control)
    })
}

pub async fn refresh_web_session(
    refresh_token: &str,
    context: &KimiWebRequestContext,
) -> Result<KimiWebTokenPair, ProviderError> {
    let client = crate::core::credentialed_http_client_builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|_| ProviderError::Other("Could not create Kimi web client".into()))?;
    refresh_web_session_at(&client, KIMI_WEB_REFRESH_URL, refresh_token, context).await
}

async fn refresh_web_session_at(
    client: &reqwest::Client,
    endpoint: &str,
    refresh_token: &str,
    context: &KimiWebRequestContext,
) -> Result<KimiWebTokenPair, ProviderError> {
    if refresh_token.trim().is_empty() {
        return Err(ProviderError::AuthRequired);
    }

    let mut request = client
        .post(endpoint)
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .header("Connect-Protocol-Version", "1")
        .header("x-msh-platform", "web")
        .header("User-Agent", KIMI_WEB_USER_AGENT)
        .json(&serde_json::json!({ "refreshToken": refresh_token }));
    if let Some(value) = context.traffic_id.as_deref() {
        request = request.header("X-Traffic-Id", value);
    }
    if let Some(value) = context.device_id.as_deref() {
        request = request.header("x-msh-device-id", value);
    }
    if let Some(value) = context.session_id.as_deref() {
        request = request.header("x-msh-session-id", value);
    }

    let response = request.send().await.map_err(ProviderError::from)?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED
        || response.status() == reqwest::StatusCode::FORBIDDEN
    {
        return Err(ProviderError::AuthRequired);
    }
    if !response.status().is_success() {
        return Err(ProviderError::Other(format!(
            "Kimi web refresh returned status {}",
            response.status()
        )));
    }

    let body = response
        .json::<KimiWebRefreshResponse>()
        .await
        .map_err(|_| ProviderError::Parse("Kimi web refresh response was invalid".into()))?;
    let access_token = body.access_token.trim().to_string();
    let refresh_token = body.refresh_token.trim().to_string();
    if access_token.is_empty() || refresh_token.is_empty() {
        return Err(ProviderError::Parse(
            "Kimi web refresh response omitted a token".into(),
        ));
    }

    Ok(KimiWebTokenPair {
        access_token,
        refresh_token,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request_context() -> KimiWebRequestContext {
        KimiWebRequestContext {
            traffic_id: Some("traffic".to_string()),
            device_id: Some("device".to_string()),
            session_id: Some("session".to_string()),
        }
    }

    #[tokio::test]
    async fn refresh_posts_connect_json_and_returns_both_rotated_tokens() {
        let mut server = mockito::Server::new_async().await;
        let refresh = server
            .mock("POST", "/api/account.gateway.v1.AuthService/RefreshToken")
            .match_header("accept", "application/json")
            .match_header("content-type", "application/json")
            .match_header("connect-protocol-version", "1")
            .match_header("x-msh-platform", "web")
            .match_header("x-traffic-id", "traffic")
            .match_header("x-msh-device-id", "device")
            .match_header("x-msh-session-id", "session")
            .match_body(r#"{"refreshToken":"old-refresh"}"#)
            .with_status(200)
            .with_body(r#"{"accessToken":"new-access","refreshToken":"new-refresh"}"#)
            .create_async()
            .await;
        let client = reqwest::Client::new();

        let tokens = refresh_web_session_at(
            &client,
            &format!(
                "{}/api/account.gateway.v1.AuthService/RefreshToken",
                server.url()
            ),
            "old-refresh",
            &request_context(),
        )
        .await
        .expect("refresh response should contain both rotated tokens");

        refresh.assert_async().await;
        assert_eq!(tokens.access_token, "new-access");
        assert_eq!(tokens.refresh_token, "new-refresh");
    }

    #[tokio::test]
    async fn unauthenticated_refresh_maps_to_auth_required_without_response_body() {
        const RESPONSE_SECRET: &str = "TEST_REFRESH_SECRET_DO_NOT_LOG";
        let mut server = mockito::Server::new_async().await;
        let refresh = server
            .mock("POST", "/api/account.gateway.v1.AuthService/RefreshToken")
            .with_status(401)
            .with_body(RESPONSE_SECRET)
            .create_async()
            .await;
        let client = reqwest::Client::new();

        let error = match refresh_web_session_at(
            &client,
            &format!(
                "{}/api/account.gateway.v1.AuthService/RefreshToken",
                server.url()
            ),
            "old-refresh",
            &request_context(),
        )
        .await
        {
            Ok(_) => panic!("401 refresh must not succeed"),
            Err(error) => error,
        };

        refresh.assert_async().await;
        assert!(matches!(error, ProviderError::AuthRequired));
        assert!(!error.to_string().contains(RESPONSE_SECRET));
    }

    #[tokio::test]
    async fn blank_rotated_token_is_rejected_without_exposing_response_body() {
        const RESPONSE_SECRET: &str = "TEST_ACCESS_SECRET_DO_NOT_LOG";
        let mut server = mockito::Server::new_async().await;
        let refresh = server
            .mock("POST", "/api/account.gateway.v1.AuthService/RefreshToken")
            .with_status(200)
            .with_body(format!(
                r#"{{"accessToken":" ","refreshToken":"{RESPONSE_SECRET}"}}"#
            ))
            .create_async()
            .await;
        let client = reqwest::Client::new();

        let error = match refresh_web_session_at(
            &client,
            &format!(
                "{}/api/account.gateway.v1.AuthService/RefreshToken",
                server.url()
            ),
            "old-refresh",
            &request_context(),
        )
        .await
        {
            Ok(_) => panic!("blank access token must not succeed"),
            Err(error) => error,
        };

        refresh.assert_async().await;
        assert!(matches!(error, ProviderError::Parse(_)));
        assert!(!error.to_string().contains(RESPONSE_SECRET));
    }

    #[test]
    fn request_context_reads_only_known_non_secret_fields() {
        let context = parse_web_request_context(Some(
            r#"{"userId":"traffic","webId":"device","ssid":"session","unknown":"ignored"}"#,
        ));

        assert_eq!(context.traffic_id.as_deref(), Some("traffic"));
        assert_eq!(context.device_id.as_deref(), Some("device"));
        assert_eq!(context.session_id.as_deref(), Some("session"));
    }
}
