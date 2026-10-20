#![cfg(feature = "openai")]

use adk_core::{Content, ErrorCategory, Llm, LlmRequest, LlmResponse, Part};
use adk_model::openai::{OpenAIResponsesClient, OpenAIResponsesConfig, RequestAdapter};
use adk_model::retry::RetryConfig;
use futures::{StreamExt, TryStreamExt};
use serde_json::{Value, json};
use std::sync::Arc;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn request() -> LlmRequest {
    LlmRequest::new("test-model", vec![Content::new("user").with_text("Hello")])
}

fn client(base_url: &str, compatible: bool) -> OpenAIResponsesClient {
    OpenAIResponsesClient::new(
        OpenAIResponsesConfig::new("test-key", "test-model")
            .with_base_url(base_url)
            .with_open_responses_mode(compatible),
    )
    .expect("client should be created")
    .with_retry_config(RetryConfig::disabled())
}

fn completion() -> Value {
    json!({
        "type": "response.completed",
        "sequence_number": 2,
        "response": {
            "id": "resp_test",
            "object": "response",
            "created_at": 0,
            "model": "test-model",
            "status": "completed",
            "output": [{
                "type": "message",
                "id": "msg_test",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "done"}]
            }],
            "usage": {
                "input_tokens": 2,
                "input_tokens_details": {"cached_tokens": 0},
                "output_tokens": 1,
                "output_tokens_details": {"reasoning_tokens": 0},
                "total_tokens": 3
            }
        }
    })
}

fn read_request(socket: &mut std::net::TcpStream) -> Value {
    use std::io::Read;

    let mut received = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        let count = socket.read(&mut buffer).expect("request should be readable");
        assert_ne!(count, 0, "request should include a body");
        received.extend_from_slice(&buffer[..count]);
        if let Some(end) = received.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&received[..end]);
            let length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().expect("valid content length"))
                })
                .expect("request should include a content length");
            if received.len() >= end + 4 + length {
                return serde_json::from_slice(&received[end + 4..end + 4 + length])
                    .expect("request body should be JSON");
            }
        }
    }
}

async fn fragmented_stream(
    ending: &str,
    compatible: bool,
    content_type: Option<&str>,
) -> Vec<LlmResponse> {
    use std::io::Write;
    use std::net::TcpListener;
    use std::time::Duration;

    let listener = TcpListener::bind("127.0.0.1:0").expect("fixture should bind");
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let delta = json!({
        "type": "response.output_text.delta",
        "sequence_number": 1,
        "item_id": "msg_test",
        "output_index": 0,
        "content_index": 0,
        "delta": "done"
    });
    let mut completed = completion();
    if !compatible {
        // Standard mode passes OpenAI payloads through without compatibility defaults.
        let message = &mut completed["response"]["output"][0];
        message["status"] = json!("completed");
        message["content"][0]["annotations"] = json!([]);
    }
    let delta_body = format!("data: {delta}{ending}{ending}: keep-alive{ending}");
    let body = format!("data: {completed}{ending}{ending}data: [DONE]{ending}{ending}");
    let content_type =
        content_type.map_or(String::new(), |value| format!("Content-Type: {value}\r\n"));
    let (release, waiting) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().expect("fixture should accept");
        socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let received = read_request(&mut socket);
        assert_eq!(received["stream"], true);
        assert_eq!(received["metadata"]["fixture"], "adapted");
        socket.write_all(format!("HTTP/1.1 200 OK\r\n{content_type}Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n").as_bytes()).unwrap();
        socket.write_all(format!("{:X}\r\n{delta_body}\r\n", delta_body.len()).as_bytes()).unwrap();
        socket.flush().unwrap();
        waiting
            .recv_timeout(Duration::from_secs(3))
            .expect("consumer must observe the delta before completion");
        for chunk in body.as_bytes().chunks(11) {
            socket.write_all(format!("{:X}\r\n", chunk.len()).as_bytes()).unwrap();
            socket.write_all(chunk).unwrap();
            socket.write_all(b"\r\n").unwrap();
            std::thread::sleep(Duration::from_millis(1));
        }
        socket.write_all(b"0\r\n\r\n").unwrap();
    });

    let adapter: RequestAdapter = Arc::new(|body, _headers| {
        body["metadata"] = json!({"fixture": "adapted"});
        Ok(())
    });
    let client = client(&base_url, compatible).with_request_adapter(adapter);
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        let mut stream = client.generate_content(request(), true).await?;
        let first = tokio::time::timeout(Duration::from_secs(1), stream.next()).await;
        release.send(()).unwrap();
        let mut rest = stream.try_collect::<Vec<_>>().await?;
        let first = first
            .expect("the first partial must not wait for completion")
            .expect("first response")
            .expect("first response should decode");
        assert!(first.partial);
        rest.insert(0, first);
        Ok::<_, adk_core::AdkError>(rest)
    })
    .await
    .expect("stream should terminate")
    .expect("stream should deserialize");
    server.join().expect("fixture should finish");
    result
}

#[tokio::test]
async fn frames_incremental_sse_with_mislabeled_headers() {
    for (ending, compatible, content_type) in ["\n", "\r\n", "\r"].into_iter().flat_map(|ending| {
        [true, false].into_iter().flat_map(move |compatible| {
            [None, Some("application/json"), Some("text/plain")]
                .into_iter()
                .map(move |content_type| (ending, compatible, content_type))
        })
    }) {
        let results = fragmented_stream(ending, compatible, content_type).await;
        let text: String = results
            .iter()
            .filter_map(|response| response.content.as_ref())
            .flat_map(|content| &content.parts)
            .filter_map(|part| match part {
                Part::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "done", "line ending {ending:?}, compatible {compatible}");
        let final_response = results.last().expect("completion response");
        assert!(final_response.turn_complete);
        assert_eq!(final_response.usage_metadata.as_ref().unwrap().total_token_count, 3);
    }
}

#[tokio::test]
async fn preserves_non_streaming_json() {
    for compatible in [false, true] {
        let server = MockServer::start().await;
        let mut body = completion()["response"].clone();
        body["output"][0]["status"] = json!("completed");
        body["output"][0]["content"][0]["annotations"] = json!([]);
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(1)
            .mount(&server)
            .await;
        let responses = client(&server.uri(), compatible)
            .generate_content(request(), false)
            .await
            .expect("request should build")
            .try_collect::<Vec<_>>()
            .await
            .expect("JSON response should decode");
        assert_eq!(responses.len(), 1);
        let response = &responses[0];
        assert!(!response.partial);
        assert!(response.turn_complete);
        assert_eq!(
            response.content.as_ref().unwrap().parts,
            vec![Part::Text { text: "done".into() }]
        );
        assert_eq!(response.usage_metadata.as_ref().unwrap().total_token_count, 3);
        let received = server.received_requests().await.unwrap();
        assert_eq!(received.len(), 1);
        assert_eq!(serde_json::from_slice::<Value>(&received[0].body).unwrap()["stream"], false);
    }
}

async fn failed(
    status: u16,
    body: &str,
    compatible: bool,
    retry_after: Option<&str>,
) -> adk_core::AdkError {
    let server = MockServer::start().await;
    let mut template = ResponseTemplate::new(status).set_body_string(body);
    if let Some(retry_after) = retry_after {
        template = template.insert_header("retry-after", retry_after);
    }
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(template)
        .expect(1)
        .mount(&server)
        .await;
    let stream = client(&server.uri(), compatible)
        .generate_content(request(), false)
        .await
        .expect("request should build");
    let error = stream.try_collect::<Vec<_>>().await.expect_err("request should fail");
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    error
}

#[tokio::test]
async fn preserves_status_and_provider_codes() {
    for (status, body, compatible, category, provider_code) in [
        (
            400,
            r#"{"error":{"message":"invalid","type":"invalid_request_error","code":"invalid_schema"}}"#,
            false,
            ErrorCategory::InvalidInput,
            Some("invalid_schema"),
        ),
        (
            429,
            r#"{"error":{"message":"limited","type":"rate_limit_error","code":"rate_limit_exceeded"}}"#,
            false,
            ErrorCategory::RateLimited,
            Some("rate_limit_exceeded"),
        ),
        (
            503,
            r#"{"detail":"service unavailable","code":"upstream_unavailable"}"#,
            true,
            ErrorCategory::Unavailable,
            Some("upstream_unavailable"),
        ),
        (502, "malformed provider body", true, ErrorCategory::Unavailable, None),
        (520, "malformed provider body", false, ErrorCategory::Unavailable, None),
        (521, "malformed provider body", false, ErrorCategory::Unavailable, None),
        (522, "malformed provider body", false, ErrorCategory::Unavailable, None),
        (523, "malformed provider body", false, ErrorCategory::Unavailable, None),
        (524, "malformed provider body", false, ErrorCategory::Unavailable, None),
        (401, "malformed provider body", false, ErrorCategory::Unauthorized, None),
    ] {
        let error = failed(status, body, compatible, None).await;
        assert_eq!(error.category, category, "status {status}");
        assert_eq!(error.details.upstream_status_code, Some(status));
        assert_eq!(
            error.details.metadata.get("provider_error_code").and_then(Value::as_str),
            provider_code,
            "status {status}"
        );
    }
}

#[tokio::test]
async fn carries_retry_after_hints() {
    for (header, delay) in
        [("2", Some(2_000)), ("1.5", None), ("invalid", None), ("18446744073709551616", None)]
    {
        let error = failed(
            503,
            r#"{"error":{"message":"unavailable","type":"server_error","code":"upstream_busy"}}"#,
            false,
            Some(header),
        )
        .await;
        assert_eq!(error.details.upstream_status_code, Some(503));
        assert_eq!(error.details.metadata["provider_error_code"], "upstream_busy");
        assert_eq!(error.retry.retry_after_ms, delay, "header {header}");
        assert!(error.retry.should_retry, "header {header}");
    }

    let date = (chrono::Utc::now() + chrono::Duration::seconds(30))
        .format("%a, %d %b %Y %H:%M:%S GMT")
        .to_string();
    let error = failed(503, "malformed provider body", true, Some(&date)).await;
    let delay = error.retry.retry_after_ms.expect("HTTP-date should produce a retry hint");
    assert!(delay > 0 && delay <= 30_000);

    let error = failed(400, "malformed provider body", false, Some("2")).await;
    assert!(!error.retry.should_retry);
    assert_eq!(error.retry.retry_after_ms, None);
}

#[tokio::test]
async fn retry_attempts_follow_header_validity() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    // Invalid hints use ordinary backoff; valid hints above the limit are refused.
    for (header, expected_attempts) in [
        ("0", 2),
        ("invalid", 2),
        ("18446744073709551616", 2),
        ("1.5", 2),
        ("+1", 2),
        ("-1", 2),
        ("61", 1),
        ("3600", 1),
    ] {
        let server = MockServer::start().await;
        let attempts = Arc::new(AtomicUsize::new(0));
        let observed = attempts.clone();
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(move |_request: &wiremock::Request| {
                observed.fetch_add(1, Ordering::SeqCst);
                ResponseTemplate::new(503).insert_header("retry-after", header).set_body_json(
                    json!({
                        "error": {"message": "unavailable", "type": "server_error"}
                    }),
                )
            })
            .expect(expected_attempts)
            .mount(&server)
            .await;
        let client = client(&server.uri(), false).with_retry_config(
            RetryConfig::default()
                .with_max_retries(1)
                .with_initial_delay(Duration::ZERO)
                .with_max_delay(Duration::ZERO),
        );
        let stream = client.generate_content(request(), false).await.expect("request should build");
        let error = stream.try_collect::<Vec<_>>().await.expect_err("request should fail");
        assert_eq!(error.details.upstream_status_code, Some(503));
        assert_eq!(attempts.load(Ordering::SeqCst), expected_attempts as usize);
    }
}

#[tokio::test]
async fn keeps_request_ids_and_body_excerpts() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(
            ResponseTemplate::new(502)
                .insert_header("x-request-id", "req_abc-123")
                .set_body_string("upstream connect error"),
        )
        .expect(1)
        .mount(&server)
        .await;
    let stream = client(&server.uri(), false)
        .generate_content(request(), false)
        .await
        .expect("request should build");
    let error = stream.try_collect::<Vec<_>>().await.expect_err("request should fail");
    assert_eq!(error.details.request_id.as_deref(), Some("req_abc-123"));
    assert!(error.message.contains("upstream connect error"), "{}", error.message);

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(
            ResponseTemplate::new(502)
                .insert_header("x-request-id", "not an id")
                .set_body_string("bad gateway"),
        )
        .expect(1)
        .mount(&server)
        .await;
    let stream = client(&server.uri(), false)
        .generate_content(request(), false)
        .await
        .expect("request should build");
    let error = stream.try_collect::<Vec<_>>().await.expect_err("request should fail");
    assert_eq!(error.details.request_id, None);
}

#[tokio::test]
async fn streaming_failures_keep_status_and_retry_hints() {
    for compatible in [false, true] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("retry-after", "3")
                    .insert_header("x-request-id", "req_stream")
                    .set_body_json(json!({
                        "error": {"message": "limited", "type": "rate_limit_error",
                            "code": "rate_limit_exceeded"}
                    })),
            )
            .expect(1)
            .mount(&server)
            .await;
        let error = match client(&server.uri(), compatible).generate_content(request(), true).await
        {
            Ok(stream) => stream.try_collect::<Vec<_>>().await.expect_err("stream should fail"),
            Err(error) => error,
        };
        assert_eq!(error.category, ErrorCategory::RateLimited);
        assert_eq!(error.details.upstream_status_code, Some(429));
        assert_eq!(error.details.metadata["provider_error_code"], "rate_limit_exceeded");
        assert_eq!(error.details.request_id.as_deref(), Some("req_stream"));
        assert_eq!(error.retry.retry_after_ms, Some(3_000));
        assert!(error.retry.should_retry);
    }
}
