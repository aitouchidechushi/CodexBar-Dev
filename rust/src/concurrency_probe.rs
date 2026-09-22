//! User-initiated two-request diagnostic. Never used by quota refresh.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Verdict {
    Passed,
    Limited,
    Inconclusive,
    Quota,
    Auth,
    RateLimited,
    Busy,
    Failed,
    Cancelled,
}

fn classify(provider: &str, status: u16, body: &Value) -> Verdict {
    let error = body.get("error").unwrap_or(body);
    let code_value = error
        .get("code")
        .or_else(|| body.pointer("/base_resp/status_code"));
    let code = code_value
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| v.to_string())
        })
        .unwrap_or_default();
    let message = error
        .get("message")
        .or_else(|| body.pointer("/base_resp/status_msg"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_lowercase();
    if provider == "kimi" && message.contains("you've reached your concurrent request limit")
        || provider == "minimax" && code == "1041"
        || provider == "zai"
            && code == "1302"
            && [
                "已达到并发限制",
                "超过并发限制",
                "并发数过高",
                "concurrent request limit",
                "concurrency limit exceeded",
            ]
            .iter()
            .any(|phrase| message.contains(phrase))
    {
        return Verdict::Limited;
    }
    if provider == "minimax" && matches!(code.as_str(), "1008" | "2056")
        || provider == "zai"
            && matches!(
                code.as_str(),
                "1113" | "1308" | "1310" | "1316" | "1317" | "1318" | "1319" | "1320" | "1321"
            )
        || message.contains("usage limit")
        || message.contains("insufficient balance")
    {
        return Verdict::Quota;
    }
    if matches!(status, 401 | 403)
        || provider == "minimax" && matches!(code.as_str(), "1004" | "2049")
        || provider == "zai"
            && matches!(
                code.as_str(),
                "1000" | "1001" | "1003" | "1309" | "1311" | "1315"
            )
    {
        return Verdict::Auth;
    }
    if status >= 500 || code == "1305" || message.contains("overloaded") {
        return Verdict::Busy;
    }
    if status == 429 || matches!(code.as_str(), "1002" | "1039" | "2045" | "1302" | "1313") {
        return Verdict::RateLimited;
    }
    Verdict::Failed
}

#[derive(Default)]
struct StreamObservation {
    first: Option<u64>,
    last: Option<u64>,
    complete: bool,
    error: Option<Verdict>,
}
fn decide(a: &StreamObservation, b: &StreamObservation) -> Verdict {
    if a.error == Some(Verdict::Limited) || b.error == Some(Verdict::Limited) {
        return Verdict::Limited;
    }
    if let Some(error) = a.error.or(b.error) {
        return error;
    }
    if a.complete && b.complete {
        if let (Some(af), Some(al), Some(bf), Some(bl)) = (a.first, a.last, b.first, b.last) {
            if af.max(bf) < al.min(bl) {
                return Verdict::Passed;
            }
        }
    }
    Verdict::Inconclusive
}
#[derive(Default)]
struct Sse {
    pending: Vec<u8>,
    observation: StreamObservation,
}
impl Sse {
    fn feed(&mut self, provider: &str, bytes: &[u8], at: u64) -> Result<(), Verdict> {
        self.pending.extend_from_slice(bytes);
        if self.pending.len() > 131072 {
            return Err(Verdict::Failed);
        }
        loop {
            let boundary = self
                .pending
                .windows(2)
                .position(|w| w == b"\n\n")
                .map(|p| (p, 2))
                .into_iter()
                .chain(
                    self.pending
                        .windows(4)
                        .position(|w| w == b"\r\n\r\n")
                        .map(|p| (p, 4)),
                )
                .min_by_key(|(p, _)| *p);
            let Some((end, delimiter)) = boundary else {
                break;
            };
            let frame = self.pending.drain(..end + delimiter).collect::<Vec<_>>();
            let text = std::str::from_utf8(&frame).map_err(|_| Verdict::Failed)?;
            let data = text
                .lines()
                .filter_map(|l| l.strip_prefix("data:"))
                .map(str::trim)
                .collect::<Vec<_>>()
                .join("\n");
            if data.is_empty() {
                continue;
            }
            if data == "[DONE]" {
                self.observation.complete = true;
                continue;
            }
            let value: Value = serde_json::from_str(&data).map_err(|_| Verdict::Failed)?;
            if value.get("error").is_some()
                || value
                    .pointer("/base_resp/status_code")
                    .is_some_and(|c| c.as_i64().is_some_and(|n| n != 0))
            {
                self.observation.error = Some(classify(provider, 200, &value));
                continue;
            }
            if let Some(choices) = value.get("choices").and_then(Value::as_array) {
                for choice in choices {
                    let output = ["content", "reasoning_content"].iter().any(|key| {
                        choice
                            .get("delta")
                            .and_then(|v| v.get(key))
                            .and_then(Value::as_str)
                            .is_some_and(|s| !s.is_empty())
                    });
                    if output {
                        self.observation.first.get_or_insert(at);
                        self.observation.last = Some(at);
                    }
                    if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                        if matches!(reason, "stop" | "length") {
                            self.observation.complete = true;
                        } else {
                            self.observation.error = Some(Verdict::Failed);
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

/// An explicit endpoint preview is shown to the user before sending credentials.
pub fn endpoint(provider: &str, region: &str) -> Option<&'static str> {
    match provider {
        "kimi" => Some("https://api.kimi.com/coding/v1/chat/completions"),
        "minimax" => Some(if matches!(region, "global" | "io" | "international") {
            "https://api.minimax.io/v1/chat/completions"
        } else {
            "https://api.minimaxi.com/v1/chat/completions"
        }),
        "zai" => Some(
            if matches!(region, "cn" | "bigmodel" | "bigmodel-cn" | "bigmodel_cn") {
                "https://open.bigmodel.cn/api/coding/paas/v4/chat/completions"
            } else {
                "https://api.z.ai/api/coding/paas/v4/chat/completions"
            },
        ),
        _ => None,
    }
}

pub fn valid_model(model: &str) -> bool {
    !model.is_empty()
        && model.len() <= 100
        && model
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-._/:".contains(&c))
}

pub async fn probe(provider: &str, region: &str, model: &str, secret: &str) -> Verdict {
    let Some(url) = endpoint(provider, region) else {
        return Verdict::Failed;
    };
    if !valid_model(model) || secret.trim().is_empty() {
        return Verdict::Failed;
    }
    let Ok(client) = crate::core::credentialed_http_client_builder()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .user_agent(concat!("CodexBar/", env!("CARGO_PKG_VERSION")))
        .build()
    else {
        return Verdict::Failed;
    };
    probe_at(&client, provider, url, model, secret).await
}

async fn probe_at(
    client: &reqwest::Client,
    provider: &str,
    url: &str,
    model: &str,
    secret: &str,
) -> Verdict {
    let epoch = Instant::now();
    let (a, b) = tokio::join!(
        request(client, provider, url, model, secret, epoch),
        request(client, provider, url, model, secret, epoch)
    );
    decide(&a, &b)
}

async fn request(
    client: &reqwest::Client,
    provider: &str,
    url: &str,
    model: &str,
    secret: &str,
    epoch: Instant,
) -> StreamObservation {
    let fail = |e| StreamObservation {
        error: Some(e),
        ..Default::default()
    };
    // Fixed non-private coding prompt. No tools, files, history, retries, or model fallback.
    let body = serde_json::json!({"model":model,"stream":true,"max_tokens":512,"messages":[{"role":"user","content":"For a coding connectivity diagnostic, write a simple Python function to return the first n Fibonacci numbers, followed by a short explanation. Do not use tools."}]});
    let Ok(mut response) = client
        .post(url)
        .bearer_auth(secret.trim())
        .header("Accept", "text/event-stream")
        .json(&body)
        .send()
        .await
    else {
        return fail(Verdict::Failed);
    };
    let status = response.status().as_u16();
    if !response.status().is_success() {
        let mut bytes = Vec::new();
        while let Ok(Some(chunk)) = response.chunk().await {
            if bytes.len() + chunk.len() > 65536 {
                return fail(Verdict::Failed);
            }
            bytes.extend_from_slice(&chunk);
        }
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        return fail(classify(provider, status, &value));
    }
    let is_stream = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("text/event-stream"));
    if !is_stream {
        let mut bytes = Vec::new();
        while let Ok(Some(chunk)) = response.chunk().await {
            if bytes.len() + chunk.len() > 65536 {
                return fail(Verdict::Failed);
            }
            bytes.extend_from_slice(&chunk);
        }
        return fail(classify(
            provider,
            status,
            &serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        ));
    }
    let mut parser = Sse::default();
    let mut total = 0;
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                total += chunk.len();
                if total > 1048576 {
                    return fail(Verdict::Failed);
                }
                if let Err(error) =
                    parser.feed(provider, &chunk, epoch.elapsed().as_micros() as u64)
                {
                    return fail(error);
                }
                if parser.observation.complete || parser.observation.error.is_some() {
                    return parser.observation;
                }
            }
            Ok(None) => return parser.observation,
            Err(_) => return fail(Verdict::Failed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn endpoint_is_provider_and_region_specific_and_models_cannot_inject_headers() {
        assert_eq!(
            endpoint("zai", "cn"),
            Some("https://open.bigmodel.cn/api/coding/paas/v4/chat/completions")
        );
        assert_eq!(
            endpoint("minimax", "global"),
            Some("https://api.minimax.io/v1/chat/completions")
        );
        assert_eq!(endpoint("codex", ""), None);
        assert!(!valid_model("x\r\nAuthorization: abc"));
        assert!(!valid_model(""));
        assert!(valid_model("kimi-for-coding"));
    }

    #[tokio::test]
    async fn real_http_two_streams_overlap_without_extra_requests() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/chat/completions", listener.local_addr().unwrap());
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let server = tokio::spawn(async move {
            let mut tasks = vec![];
            for _ in 0..2 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let barrier = barrier.clone();
                tasks.push(tokio::spawn(async move {
                    let mut request=Vec::new();
                    let mut buf=[0;1024];
                    loop {
                        let n=socket.read(&mut buf).await.unwrap(); assert!(n>0);
                        request.extend_from_slice(&buf[..n]);
                        if let Some(end)=request.windows(4).position(|w|w==b"\r\n\r\n") {
                            let header=String::from_utf8_lossy(&request[..end]).to_lowercase();
                            let length:usize=header.lines().find_map(|l|l.strip_prefix("content-length: ")).unwrap().parse().unwrap();
                            if request.len()>=end+4+length {
                                let body:Value=serde_json::from_slice(&request[end+4..]).unwrap();
                                assert_eq!(body["stream"],true); assert_eq!(body["max_tokens"],512);
                                assert!(header.contains("authorization: bearer fake-test-key"));
                                break;
                            }
                        }
                    }
                    socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n").await.unwrap();
                    barrier.wait().await;
                    socket.write_all(b"data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\n\n").await.unwrap();
                    barrier.wait().await;
                    tokio::time::sleep(Duration::from_millis(80)).await;
                    socket.write_all(b"data: {\"choices\":[{\"delta\":{\"content\":\"b\"}}]}\n\ndata: [DONE]\n\n").await.unwrap();
                }));
            }
            for task in tasks {
                task.await.unwrap();
            }
        });
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        assert_eq!(
            probe_at(&client, "kimi", &url, "fake-model", "fake-test-key").await,
            Verdict::Passed
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn provider_json_error_is_handled_without_retries_or_raw_error_output() {
        let mut server = mockito::Server::new_async().await;
        let expected=server.mock("POST","/chat/completions").match_header("authorization","Bearer fake-test-key")
            .with_status(200).with_header("content-type","application/json")
            .with_body(r#"{"base_resp":{"status_code":1041,"status_msg":"conn limit fake-private-string"}}"#).expect(2).create_async().await;
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let result = probe_at(
            &client,
            "minimax",
            &format!("{}/chat/completions", server.url()),
            "fake-model",
            "fake-test-key",
        )
        .await;
        assert_eq!(result, Verdict::Limited);
        assert_eq!(serde_json::to_string(&result).unwrap(), "\"limited\"");
        expected.assert_async().await;
    }

    #[test]
    fn overlapping_output_passes_but_serial_output_does_not() {
        let a = StreamObservation {
            first: Some(10),
            last: Some(40),
            complete: true,
            error: None,
        };
        let b = StreamObservation {
            first: Some(20),
            last: Some(50),
            complete: true,
            error: None,
        };
        assert_eq!(decide(&a, &b), Verdict::Passed);
        let serial = StreamObservation {
            first: Some(41),
            last: Some(90),
            complete: true,
            error: None,
        };
        assert_eq!(decide(&a, &serial), Verdict::Inconclusive);
        let failed = StreamObservation {
            error: Some(Verdict::Quota),
            ..Default::default()
        };
        assert_eq!(decide(&a, &failed), Verdict::Quota);
        let limited = StreamObservation {
            error: Some(Verdict::Limited),
            ..Default::default()
        };
        assert_eq!(decide(&a, &limited), Verdict::Limited);
    }

    #[test]
    fn split_utf8_sse_is_decoded_but_heartbeats_are_not_output() {
        let mut parser = Sse::default();
        parser
            .feed(
                "kimi",
                b": heartbeat\n\ndata: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\n",
                1,
            )
            .unwrap();
        assert!(parser.observation.first.is_none());
        let event = "data: {\"choices\":[{\"delta\":{\"content\":\"中文\"}}]}\r\n\r\n";
        for byte in event.as_bytes() {
            parser.feed("kimi", &[*byte], 20).unwrap();
        }
        assert_eq!(parser.observation.first, Some(20));
        parser.feed("kimi", b"data: [DONE]\n\n", 30).unwrap();
        assert!(parser.observation.complete);
    }

    #[test]
    fn stream_errors_and_truncated_streams_never_pass() {
        let mut parser = Sse::default();
        parser
            .feed(
                "minimax",
                b"data: {\"base_resp\":{\"status_code\":1041}}\n\n",
                1,
            )
            .unwrap();
        assert_eq!(parser.observation.error, Some(Verdict::Limited));
        let partial = StreamObservation {
            first: Some(1),
            last: Some(10),
            ..Default::default()
        };
        assert_ne!(decide(&partial, &partial), Verdict::Passed);
        let mut large = Sse::default();
        assert!(large.feed("kimi", &vec![b'x'; 131073], 0).is_err());
    }

    #[test]
    fn explicit_concurrency_is_distinct_from_quota_and_generic_rate_limits() {
        let cases = [
            (
                "minimax",
                200,
                json!({"base_resp":{"status_code":1001}}),
                Verdict::Failed,
            ),
            (
                "zai",
                429,
                json!({"error":{"code":"1302","message":"Please reduce concurrency and retry"}}),
                Verdict::RateLimited,
            ),
            (
                "kimi",
                403,
                json!({"error":{"message":"You've reached your concurrent request limit. Please wait."}}),
                Verdict::Limited,
            ),
            (
                "kimi",
                403,
                json!({"error":{"message":"You've reached your 5-hour usage limit."}}),
                Verdict::Quota,
            ),
            (
                "kimi",
                429,
                json!({"error":{"message":"We're receiving too many requests at the moment."}}),
                Verdict::RateLimited,
            ),
            (
                "kimi",
                429,
                json!({"error":{"message":"The engine is currently overloaded"}}),
                Verdict::Busy,
            ),
            (
                "minimax",
                200,
                json!({"base_resp":{"status_code":1041,"status_msg":"conn limit"}}),
                Verdict::Limited,
            ),
            (
                "minimax",
                200,
                json!({"base_resp":{"status_code":1002}}),
                Verdict::RateLimited,
            ),
            (
                "minimax",
                200,
                json!({"base_resp":{"status_code":1008}}),
                Verdict::Quota,
            ),
            (
                "zai",
                429,
                json!({"error":{"code":"1302","message":"Rate limit reached for requests"}}),
                Verdict::RateLimited,
            ),
            (
                "zai",
                429,
                json!({"error":{"code":"1302","message":"已达到并发限制"}}),
                Verdict::Limited,
            ),
            ("zai", 429, json!({"error":{"code":"1113"}}), Verdict::Quota),
            ("zai", 429, json!({"error":{"code":"1308"}}), Verdict::Quota),
            (
                "kimi",
                401,
                json!({"error":{"message":"invalid key"}}),
                Verdict::Auth,
            ),
        ];
        for (p, s, b, want) in cases {
            assert_eq!(classify(p, s, &b), want, "{p} {s} {b}");
        }
    }
}
