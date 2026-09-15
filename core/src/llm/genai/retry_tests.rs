//! Retry policy tests use scripted failures and loopback HTTP only; no provider
//! credentials or external network. Zero backoff avoids process-global env edits.
use super::*;
use std::collections::VecDeque;
use std::time::{Duration, SystemTime};

fn settings(max_retries: u32) -> RetrySettings {
    RetrySettings {
        max_retries,
        base_delay_ms: 0,
        jitter_percent: 0,
    }
}

fn http_error(status: u16, body: &str) -> genai::Error {
    genai::Error::HttpError {
        status: reqwest::StatusCode::from_u16(status).unwrap(),
        canonical_reason: String::new(),
        body: body.into(),
        headers: Default::default(),
    }
}

fn web_error(error: genai::webc::Error) -> genai::Error {
    genai::Error::WebModelCall {
        model_iden: ModelIden::new(AdapterKind::BedrockSigv4, "global.openai.gpt-5.6-luna"),
        webc_error: error,
    }
}

#[test]
fn typed_http_status_controls_retries_not_body_or_model_numbers() {
    for status in [408, 429, 500, 502, 503, 504, 529, 599] {
        assert!(
            is_retryable(&http_error(status, "temporarily unavailable")),
            "{status}"
        );
    }
    for status in [400, 401, 402, 403, 404, 413, 422, 501, 505] {
        assert!(
            !is_retryable(&http_error(
                status,
                "prompt mentions 429, 503 and connection reset by peer"
            )),
            "{status}"
        );
    }
    assert!(!is_retryable(&genai::Error::Internal(
        "model id contains 503; request says error sending request".into()
    )));
    assert!(!is_retryable(&genai::Error::NoAuthData {
        model_iden: ModelIden::new(AdapterKind::OpenAI, "model-503"),
    }));
}

#[test]
fn quota_failures_stay_permanent_but_shared_pool_limits_are_transient() {
    for body in [
        "Usage limit reached for 5 hour",
        "insufficient_quota",
        "insufficient balance",
        "please recharge",
    ] {
        assert!(!is_retryable(&http_error(429, body)), "{body}");
    }
    assert!(is_retryable(&http_error(
        429,
        "insufficient_quota: temporarily rate-limited upstream. Please retry shortly"
    )));
}

#[test]
fn both_web_error_wrappers_use_typed_status() {
    let response_error = || genai::webc::Error::ResponseFailedStatus {
        status: reqwest::StatusCode::SERVICE_UNAVAILABLE,
        body: "upstream failed".into(),
        headers: Default::default(),
    };
    assert!(is_retryable(&web_error(response_error())));
    assert!(is_retryable(&genai::Error::WebAdapterCall {
        adapter_kind: AdapterKind::OpenAI,
        webc_error: response_error(),
    }));
}

#[test]
fn malformed_http_envelopes_are_retryable_but_request_build_errors_are_not() {
    assert!(is_retryable(&web_error(
        genai::webc::Error::ResponseFailedInvalidJson {
            body: "{\"choices\":".into(),
            cause: "EOF".into(),
        }
    )));
    assert!(is_retryable(&web_error(
        genai::webc::Error::ResponseFailedNotJson {
            content_type: "text/html".into(),
            body: "<html>gateway unavailable</html>".into(),
        }
    )));
    let error = reqwest::Client::new().get("not a URL").build().unwrap_err();
    assert!(!is_retryable(&web_error(genai::webc::Error::Reqwest(
        error
    ))));
}

#[tokio::test]
async fn twenty_retries_allow_the_twenty_first_attempt_and_reset_for_each_completion() {
    for _ in 0..2 {
        let mut calls = 0;
        let result = retry_provider_call("test", settings(DEFAULT_MAX_RETRIES), || {
            calls += 1;
            std::future::ready(if calls <= DEFAULT_MAX_RETRIES {
                Err(http_error(503, "busy"))
            } else {
                Ok("recovered")
            })
        })
        .await
        .unwrap();
        assert_eq!(result, "recovered");
        assert_eq!(calls, 21);
    }
}

#[tokio::test]
async fn exhaustion_preserves_the_last_error_and_zero_disables_retries() {
    for max in [0, 2] {
        let mut calls = 0;
        let result = retry_provider_call::<(), _, _>("test", settings(max), || {
            calls += 1;
            std::future::ready(Err(http_error(503, &format!("failure {calls}"))))
        })
        .await
        .unwrap_err();
        assert_eq!(calls, max + 1);
        assert!(result.to_string().contains(&format!("failure {calls}")));
    }
}

#[tokio::test]
async fn permanent_errors_do_not_consume_retry_budget_even_after_transient_failures() {
    for status in [400, 401, 403, 429, 501] {
        let mut replies = VecDeque::from([
            Err(http_error(503, "busy")),
            Err(http_error(
                status,
                "insufficient_quota; 503 in quoted input",
            )),
            Ok("must not be reached"),
        ]);
        let result = retry_provider_call("test", settings(20), || {
            std::future::ready(replies.pop_front().unwrap())
        })
        .await;
        assert!(result.is_err());
        assert_eq!(replies.len(), 1);
    }
}

#[test]
fn backoff_respects_retry_after_seconds_milliseconds_dates_and_invalid_headers() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let mut err = http_error(503, "busy");
    let retry = RetrySettings {
        base_delay_ms: 5000,
        ..settings(20)
    };
    assert_eq!(retry.backoff(2, &err, now), Duration::from_secs(10));
    let genai::Error::HttpError { headers, .. } = &mut err else {
        unreachable!()
    };
    headers.insert("retry-after", "15".parse().unwrap());
    headers.insert("retry-after-ms", "16500".parse().unwrap());
    assert_eq!(retry.backoff(2, &err, now), Duration::from_millis(16500));
    assert_eq!(retry.backoff(4, &err, now), Duration::from_secs(20));
    let genai::Error::HttpError { headers, .. } = &mut err else {
        unreachable!()
    };
    headers.insert(
        "retry-after",
        httpdate::fmt_http_date(now + Duration::from_secs(30))
            .parse()
            .unwrap(),
    );
    assert_eq!(retry.backoff(2, &err, now), Duration::from_secs(30));
    let genai::Error::HttpError { headers, .. } = &mut err else {
        unreachable!()
    };
    headers.insert(
        "retry-after",
        httpdate::fmt_http_date(now - Duration::from_secs(30))
            .parse()
            .unwrap(),
    );
    headers.remove("retry-after-ms");
    assert_eq!(retry.backoff(2, &err, now), Duration::from_secs(10));
    for bad in ["not a date", "-1", "18446744073709551615"] {
        let genai::Error::HttpError { headers, .. } = &mut err else {
            unreachable!()
        };
        headers.insert("retry-after", bad.parse().unwrap());
        assert_eq!(retry.backoff(2, &err, now), Duration::from_secs(10));
    }
    assert_eq!(
        retry_delay(u32::MAX, u64::MAX),
        Duration::from_millis(u64::MAX)
    );
}

#[tokio::test]
async fn real_reqwest_timeout_is_retryable_and_keeps_underlying_cause() {
    // A listening socket that never sends a response. No race against an
    // external service or an assumed-unused port.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let err = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_millis(20))
        .build()
        .unwrap()
        .get(format!("http://{}/", listener.local_addr().unwrap()))
        .send()
        .await
        .unwrap_err();
    assert!(err.is_timeout());
    let err = web_error(genai::webc::Error::Reqwest(err));
    assert!(is_retryable(&err));
    assert!(provider_error_message(&err).contains("timed out"));
}

#[tokio::test]
async fn real_genai_request_recovers_from_503_and_dropped_connection_without_changing_body() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1/", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let mut requests = vec![];
        for attempt in 0..3 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut data = vec![];
            let header_end = loop {
                let mut chunk = [0; 4096];
                let n = stream.read(&mut chunk).await.unwrap();
                assert!(n > 0);
                data.extend_from_slice(&chunk[..n]);
                if let Some(i) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                    break i + 4;
                }
            };
            let headers = String::from_utf8_lossy(&data[..header_end]);
            assert!(headers.starts_with("POST /v1/chat/completions HTTP/1.1"));
            let length = headers
                .lines()
                .find_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    key.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap();
            while data.len() < header_end + length {
                let mut chunk = [0; 4096];
                let n = stream.read(&mut chunk).await.unwrap();
                assert!(n > 0);
                data.extend_from_slice(&chunk[..n]);
            }
            requests.push(
                serde_json::from_slice::<Value>(&data[header_end..header_end + length]).unwrap(),
            );
            if attempt == 1 {
                continue;
            } // close after receiving the request, before responding
            let (status, body) = if attempt == 0 {
                (
                    "503 Service Unavailable",
                    r#"{"error":{"message":"temporarily unavailable"}}"#,
                )
            } else {
                (
                    "200 OK",
                    r#"{"id":"test","object":"chat.completion","model":"gpt-4o","choices":[{"index":0,"message":{"role":"assistant","content":"recovered"},"finish_reason":"stop"}],"usage":{"prompt_tokens":5,"completion_tokens":1,"total_tokens":6}}"#,
                )
            };
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        }
        requests
    });
    let client = ProviderClient::new(&endpoint, "local-test-key");
    let request =
        GChatRequest::from_messages(vec![ChatMessage::user("Keep this exact conversation")]);
    let options = ChatOptions::default().with_max_tokens(64);
    let response = tokio::time::timeout(
        Duration::from_secs(10),
        retry_provider_call("openai::gpt-4o", settings(2), || {
            client
                .client
                .exec_chat("openai::gpt-4o", request.clone(), Some(&options))
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        convert_response(response).content.as_deref(),
        Some("recovered")
    );
    let requests = server.await.unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0], requests[1]);
    assert_eq!(requests[1], requests[2]);
}
