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
        request_timeout: None,
    }
}

#[test]
fn zero_millis_disables_the_deadline_rather_than_expiring_instantly() {
    assert_eq!(attempt_deadline(0), None);
    assert_eq!(
        attempt_deadline(60_000),
        Some(Duration::from_millis(60_000))
    );
}

#[tokio::test]
async fn a_blocking_deadline_turns_a_stalled_attempt_into_a_timeout() {
    // The deadline still exists for the blocking transport; it is applied
    // inside `deadline_attempt`, so a never-answering request ends as a
    // retryable stall rather than hanging.
    let err = deadline_attempt::<(), _>(Some(Duration::from_millis(20)), async {
        std::future::pending::<()>().await;
        Ok(())
    })
    .await
    .unwrap_err();
    assert!(matches!(err, AttemptError::Timeout(limit) if limit == Duration::from_millis(20)));
}

#[tokio::test]
async fn a_stalled_attempt_is_retried_like_a_5xx() {
    let mut calls = 0;
    let result = retry_provider_call("test", settings(3), || {
        calls += 1;
        async move {
            if calls == 1 {
                Err(AttemptError::Timeout(Duration::from_millis(20)))
            } else {
                Ok("recovered after timeout")
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(result, "recovered after timeout");
    assert_eq!(calls, 2, "the stalled attempt is retried exactly once");
}

#[tokio::test]
async fn timeout_exhaustion_names_the_budget_and_the_attempt_count() {
    let mut calls = 0;
    let err = retry_provider_call::<(), _, _>("test", settings(2), || {
        calls += 1;
        async { Err(AttemptError::Timeout(Duration::from_millis(15))) }
    })
    .await
    .unwrap_err();
    assert_eq!(calls, 3, "one initial attempt plus the retry budget");
    let message = err.to_string();
    assert!(message.contains("no output within 15ms"), "{message}");
    assert!(message.contains("3 attempt"), "{message}");
}

#[tokio::test]
async fn a_truncated_stream_is_retryable_and_names_itself() {
    let mut calls = 0;
    let result = retry_provider_call("test", settings(1), || {
        calls += 1;
        async move {
            if calls == 1 {
                Err(AttemptError::Truncated)
            } else {
                Ok("recovered after truncation")
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(result, "recovered after truncation");
    assert_eq!(calls, 2);
    assert_eq!(
        attempt_kind(&AttemptError::Truncated),
        "stream ended without an end event"
    );
    assert!(attempt_message(&AttemptError::Truncated, 2).contains("2 attempt"));
}

#[tokio::test]
async fn no_deadline_means_a_slow_answer_is_not_cancelled() {
    let result = deadline_attempt::<&str, _>(None, async {
        tokio::time::sleep(Duration::from_millis(30)).await;
        Ok("slow but fine")
    })
    .await
    .unwrap();
    assert_eq!(result, "slow but fine");
}

#[test]
fn a_timeout_is_retryable_and_carries_no_provider_retry_after() {
    let err = AttemptError::Timeout(Duration::from_secs(60));
    assert!(attempt_is_retryable(&err));
    assert_eq!(attempt_kind(&err), "no output within 60s");
    assert_eq!(
        attempt_kind(&AttemptError::Timeout(Duration::from_millis(1500))),
        "no output within 1500ms"
    );
    assert!(attempt_message(&err, 4).contains("4 attempt"));
    // The 5xx/transport classification is unchanged for provider answers.
    assert!(attempt_is_retryable(&AttemptError::Provider(http_error(
        503, "busy"
    ))));
    assert!(!attempt_is_retryable(&AttemptError::Provider(http_error(
        400,
        "bad request"
    ))));
    let retry = RetrySettings {
        base_delay_ms: 5_000,
        ..settings(20)
    };
    assert_eq!(
        retry.backoff_attempt(2, &err, SystemTime::UNIX_EPOCH),
        Duration::from_secs(10)
    );
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
                Err(AttemptError::Provider(http_error(503, "busy")))
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
            std::future::ready(Err(AttemptError::Provider(http_error(
                503,
                &format!("failure {calls}"),
            ))))
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
            Err(AttemptError::Provider(http_error(503, "busy"))),
            Err(AttemptError::Provider(http_error(
                status,
                "insufficient_quota; 503 in quoted input",
            ))),
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
            blocking_chat(&client.client, "openai::gpt-4o", &request, &options, None)
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        convert_reply(response).content.as_deref(),
        Some("recovered")
    );
    let requests = server.await.unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0], requests[1]);
    assert_eq!(requests[1], requests[2]);
}

// region: --- streamed completion (loopback SSE, no credentials)

/// One scripted SSE connection: frames are written in order, each after its
/// delay; `hold_open_ms` keeps the socket alive afterwards instead of closing,
/// which is how a stalled-but-connected stream is simulated.
struct SseScript {
    frames: Vec<(Duration, String)>,
    hold_open_ms: Option<u64>,
}

impl SseScript {
    fn new(frames: Vec<(u64, &str)>) -> Self {
        Self {
            frames: frames
                .into_iter()
                .map(|(ms, frame)| (Duration::from_millis(ms), frame.to_string()))
                .collect(),
            hold_open_ms: None,
        }
    }

    fn holding(mut self, ms: u64) -> Self {
        self.hold_open_ms = Some(ms);
        self
    }
}

/// Serves one SSE connection per script on a loopback port and returns the
/// endpoint plus a handle yielding the request bodies it received. Each
/// connection is served in its own task, so a stalled connection does not
/// block the retry that follows it.
async fn sse_server(scripts: Vec<SseScript>) -> (String, tokio::task::JoinHandle<Vec<Value>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn serve(mut stream: tokio::net::TcpStream, script: SseScript) -> Value {
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
        let headers = String::from_utf8_lossy(&data[..header_end]).to_string();
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
            data.extend_from_slice(&chunk[..n]);
        }
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                  Cache-Control: no-cache\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        for (delay, frame) in script.frames {
            tokio::time::sleep(delay).await;
            stream
                .write_all(format!("data: {frame}\n\n").as_bytes())
                .await
                .unwrap();
            stream.flush().await.unwrap();
        }
        if let Some(hold) = script.hold_open_ms {
            tokio::time::sleep(Duration::from_millis(hold)).await;
        }
        let _ = stream.shutdown().await;
        serde_json::from_slice::<Value>(&data[header_end..]).unwrap()
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1/", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move {
        let mut tasks = vec![];
        for script in scripts {
            let (stream, _) = listener.accept().await.unwrap();
            tasks.push(tokio::spawn(serve(stream, script)));
        }
        let mut bodies = vec![];
        for task in tasks {
            bodies.push(task.await.unwrap());
        }
        bodies
    });
    (endpoint, handle)
}

/// The capture options `complete` uses, so the wrapper assembles the same reply.
fn capture_options() -> ChatOptions {
    ChatOptions::default()
        .with_max_tokens(64)
        .with_capture_content(true)
        .with_capture_tool_calls(true)
        .with_capture_reasoning_content(true)
        .with_capture_usage(true)
}

fn stream_frames() -> Vec<(u64, &'static str)> {
    vec![
        (
            0,
            r#"{"id":"1","object":"chat.completion.chunk","model":"gpt-4o","choices":[{"index":0,"delta":{"reasoning_content":"weighing options"},"finish_reason":null}]}"#,
        ),
        (
            0,
            r#"{"id":"1","object":"chat.completion.chunk","model":"gpt-4o","choices":[{"index":0,"delta":{"content":"Reading "},"finish_reason":null}]}"#,
        ),
        (
            0,
            r#"{"id":"1","object":"chat.completion.chunk","model":"gpt-4o","choices":[{"index":0,"delta":{"content":"the file."},"finish_reason":null}]}"#,
        ),
        (
            0,
            r#"{"id":"1","object":"chat.completion.chunk","model":"gpt-4o","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"read_file","arguments":"{\"path\":"}}]},"finish_reason":null}]}"#,
        ),
        (
            0,
            r#"{"id":"1","object":"chat.completion.chunk","model":"gpt-4o","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"a.py\"}"}}]},"finish_reason":"tool_calls"}]}"#,
        ),
        (
            0,
            r#"{"id":"1","object":"chat.completion.chunk","model":"gpt-4o","choices":[],"usage":{"prompt_tokens":11,"completion_tokens":9,"total_tokens":20}}"#,
        ),
        (0, "[DONE]"),
    ]
}

#[tokio::test]
async fn a_streamed_reply_is_assembled_like_the_blocking_one() {
    let (endpoint, server) = sse_server(vec![SseScript::new(stream_frames())]).await;
    let client = ProviderClient::new(&endpoint, "local-test-key");
    let request = GChatRequest::from_messages(vec![ChatMessage::user("Read a.py")]);
    let reply = stream_chat(
        &client.client,
        "openai::gpt-4o",
        &request,
        &capture_options(),
        Some(Duration::from_secs(5)),
    )
    .await
    .unwrap();
    let converted = convert_reply(reply);
    assert_eq!(converted.content.as_deref(), Some("Reading the file."));
    assert_eq!(converted.thinking.as_deref(), Some("weighing options"));
    assert_eq!(converted.tool_calls.len(), 1);
    assert_eq!(converted.tool_calls[0].id, "call_1");
    assert_eq!(converted.tool_calls[0].name, "read_file");
    assert_eq!(converted.tool_calls[0].arguments, "{\"path\":\"a.py\"}");
    let usage = converted.usage.unwrap();
    assert_eq!(usage.input_tokens, 11);
    assert_eq!(usage.output_tokens, 9);
    let bodies = server.await.unwrap();
    assert_eq!(bodies[0]["stream"], Value::Bool(true));
    assert_eq!(bodies[0]["messages"][0]["content"], "Read a.py");
}

#[tokio::test]
async fn a_slow_but_flowing_stream_is_never_cancelled() {
    // Eight chunks, 40 ms apart, under a 100 ms idle budget: the whole answer
    // takes 320 ms — longer than the idle budget — but no gap does.
    let frames = vec![
        (
            0,
            r#"{"id":"1","object":"chat.completion.chunk","model":"gpt-4o","choices":[{"index":0,"delta":{"content":"a"},"finish_reason":null}]}"#,
        ),
        (
            40,
            r#"{"id":"1","object":"chat.completion.chunk","model":"gpt-4o","choices":[{"index":0,"delta":{"content":"b"},"finish_reason":null}]}"#,
        ),
        (
            40,
            r#"{"id":"1","object":"chat.completion.chunk","model":"gpt-4o","choices":[{"index":0,"delta":{"content":"c"},"finish_reason":null}]}"#,
        ),
        (
            40,
            r#"{"id":"1","object":"chat.completion.chunk","model":"gpt-4o","choices":[{"index":0,"delta":{"content":"d"},"finish_reason":null}]}"#,
        ),
        (
            40,
            r#"{"id":"1","object":"chat.completion.chunk","model":"gpt-4o","choices":[{"index":0,"delta":{"content":"e"},"finish_reason":"stop"}]}"#,
        ),
        (
            40,
            r#"{"id":"1","object":"chat.completion.chunk","model":"gpt-4o","choices":[],"usage":{"prompt_tokens":3,"completion_tokens":5,"total_tokens":8}}"#,
        ),
        (40, "[DONE]"),
    ];
    let (endpoint, server) = sse_server(vec![SseScript::new(frames)]).await;
    let client = ProviderClient::new(&endpoint, "local-test-key");
    let request = GChatRequest::from_messages(vec![ChatMessage::user("slow please")]);
    let reply = stream_chat(
        &client.client,
        "openai::gpt-4o",
        &request,
        &capture_options(),
        Some(Duration::from_millis(100)),
    )
    .await
    .unwrap();
    assert_eq!(convert_reply(reply).content.as_deref(), Some("abcde"));
    assert_eq!(server.await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_silent_stream_is_a_retryable_stall() {
    // First connection answers nothing at all; the idle budget must end that
    // attempt and the identical request must be retried, exactly like a dropped
    // connection — and the retry succeeds.
    let stalled = SseScript::new(vec![]).holding(5_000);
    let good = SseScript::new(vec![
        (
            0,
            r#"{"id":"1","object":"chat.completion.chunk","model":"gpt-4o","choices":[{"index":0,"delta":{"content":"recovered"},"finish_reason":"stop"}]}"#,
        ),
        (0, "[DONE]"),
    ]);
    let (endpoint, server) = sse_server(vec![stalled, good]).await;
    let client = ProviderClient::new(&endpoint, "local-test-key");
    let request = GChatRequest::from_messages(vec![ChatMessage::user("say something")]);
    let options = capture_options();
    let stream = StreamSettings {
        enabled: true,
        idle: Some(Duration::from_millis(60)),
    };
    let reply = tokio::time::timeout(
        Duration::from_secs(10),
        retry_provider_call("openai::gpt-4o", settings(2), || {
            provider_attempt(
                &client.client,
                "openai::gpt-4o",
                &request,
                &options,
                stream,
                None,
            )
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(convert_reply(reply).content.as_deref(), Some("recovered"));
    let bodies = server.await.unwrap();
    assert_eq!(bodies.len(), 2, "the stalled attempt was retried");
    assert_eq!(bodies[0]["messages"], bodies[1]["messages"]);
}

#[tokio::test]
async fn a_stream_without_an_end_event_is_a_truncated_attempt() {
    let truncated = SseScript::new(vec![(
        0,
        r#"{"id":"1","object":"chat.completion.chunk","model":"gpt-4o","choices":[{"index":0,"delta":{"content":"half an ans"},"finish_reason":null}]}"#,
    )]);
    let (endpoint, _server) = sse_server(vec![truncated]).await;
    let client = ProviderClient::new(&endpoint, "local-test-key");
    let request = GChatRequest::from_messages(vec![ChatMessage::user("hi")]);
    let err = stream_chat(
        &client.client,
        "openai::gpt-4o",
        &request,
        &capture_options(),
        Some(Duration::from_secs(5)),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, AttemptError::Truncated), "{err:?}");
    assert!(attempt_is_retryable(&err));
}

// region: --- inline reasoning parity

#[test]
fn inline_think_blocks_split_the_same_way_on_both_transports() {
    // genai's blocking path normalizes inline think blocks; the streaming path
    // gets no such help, so the shared conversion has to do it.
    let (content, thinking) = split_inline_think(Some(
        "  \u{3c}thinking\u{3e}weighing options\u{3c}/thinking\u{3e}final answer  ".into(),
    ));
    assert_eq!(content.as_deref(), Some("final answer"));
    assert_eq!(thinking.as_deref(), Some("weighing options"));
    // No block: content survives (trimmed, as the blocking path does), no reasoning.
    let (content, thinking) = split_inline_think(Some("plain answer".into()));
    assert_eq!(content.as_deref(), Some("plain answer"));
    assert_eq!(thinking, None);
    // A block with nothing after it leaves no content.
    let (content, thinking) = split_inline_think(Some(
        "\u{3c}thinking\u{3e}only reasoning\u{3c}/thinking\u{3e}".into(),
    ));
    assert_eq!(content, None);
    assert_eq!(thinking.as_deref(), Some("only reasoning"));
    // An unterminated block is not a block.
    let (content, thinking) =
        split_inline_think(Some("start  \u{3c}thinking\u{3e} but never end".into()));
    assert_eq!(
        content.as_deref(),
        Some("start  \u{3c}thinking\u{3e} but never end")
    );
    assert_eq!(thinking, None);
    assert_eq!(split_inline_think(None), (None, None));
}

#[tokio::test]
async fn a_reasoning_only_stream_assembles_to_empty_content_not_an_error() {
    // Gemini-style: reasoning deltas, then the end event, with no visible text.
    // The blocking call reports this as empty content plus reasoning; the
    // streamed path must not turn it into a transport error.
    let (endpoint, _server) = sse_server(vec![SseScript::new(vec![
        (0, r#"{"id":"1","object":"chat.completion.chunk","model":"gpt-4o","choices":[{"index":0,"delta":{"reasoning_content":"thinking hard"},"finish_reason":null}]}"#),
        (0, r#"{"id":"1","object":"chat.completion.chunk","model":"gpt-4o","choices":[{"index":0,"delta":{},"finish_reason":"length"}]}"#),
        (0, "[DONE]"),
    ])])
    .await;
    let client = ProviderClient::new(&endpoint, "local-test-key");
    let request = GChatRequest::from_messages(vec![ChatMessage::user("write a huge listing")]);
    let reply = stream_chat(
        &client.client,
        "openai::gpt-4o",
        &request,
        &capture_options(),
        Some(Duration::from_secs(5)),
    )
    .await
    .unwrap();
    let converted = convert_reply(reply);
    assert_eq!(converted.content, None);
    assert_eq!(converted.thinking.as_deref(), Some("thinking hard"));
    assert!(converted.tool_calls.is_empty());
}
