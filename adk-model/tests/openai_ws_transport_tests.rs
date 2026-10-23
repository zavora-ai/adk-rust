//! The Responses WebSocket transport ends every response with its real outcome and never
//! lets one response's frames reach another request.

#![cfg(feature = "openai-ws")]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_core::{AdkError, ErrorCategory, FinishReason, LlmResponse, Part};
use adk_model::openai::{OpenAIResponsesConfig, WsTransport};
use adk_model::retry::RetryConfig;
use async_openai::types::responses::{CreateResponse, CreateResponseArgs, InputParam};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server as handshake;

enum Step {
    Send(Value),
    Ping,
    Pause(u64),
    Close,
    Refuse(u16),
}

type Script = Arc<dyn Fn(&Value) -> Vec<Step> + Send + Sync>;
type Plan = Arc<dyn Fn(usize) -> Option<u16> + Send + Sync>;

struct Capture(Arc<Mutex<Vec<http::HeaderMap>>>);

impl handshake::Callback for Capture {
    fn on_request(
        self,
        request: &handshake::Request,
        mut response: handshake::Response,
    ) -> Result<handshake::Response, handshake::ErrorResponse> {
        self.0.lock().unwrap().push(request.headers().clone());
        response.headers_mut().insert("x-handshake", http::HeaderValue::from_static("observed"));
        Ok(response)
    }
}

/// A WebSocket server that answers each connection's requests one at a time, in order.
struct Server {
    base: String,
    handshakes: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<Value>>>,
    headers: Arc<Mutex<Vec<http::HeaderMap>>>,
}

impl Server {
    async fn start(script: Script, plan: Plan) -> Self {
        Self::combined(script, plan, false).await
    }

    async fn combined(script: Script, plan: Plan, http: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let handshakes = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let headers = Arc::new(Mutex::new(Vec::new()));
        let captured = headers.clone();
        let (count, log) = (handshakes.clone(), requests.clone());
        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                let (script, log) = (script.clone(), log.clone());
                let captured = captured.clone();
                let plan = plan.clone();
                let count = count.clone();
                tokio::spawn(async move {
                    let mut first = [0; 1];
                    if http && socket.peek(&mut first).await.is_ok() && first[0] == b'P' {
                        serve_http(socket, script, log, captured).await;
                        return;
                    }
                    let refusal = plan(count.fetch_add(1, Ordering::SeqCst));
                    match refusal {
                        Some(status) => refuse(socket, status).await,
                        None => serve(socket, script, log, captured).await,
                    }
                });
            }
        });
        Self { base, handshakes, requests, headers }
    }

    fn handshakes(&self) -> usize {
        self.handshakes.load(Ordering::SeqCst)
    }

    fn config(&self) -> OpenAIResponsesConfig {
        OpenAIResponsesConfig::new("sk-test", "test-model").with_base_url(&self.base)
    }
}

fn accept_all() -> Plan {
    Arc::new(|_| None)
}

async fn refuse(mut socket: TcpStream, status: u16) {
    let mut buffer = [0_u8; 4096];
    let mut received = Vec::new();
    while !received.windows(4).any(|window| window == b"\r\n\r\n") {
        match socket.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(read) => received.extend_from_slice(&buffer[..read]),
        }
    }
    let reply =
        format!("HTTP/1.1 {status} Refused\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
    let _ = socket.write_all(reply.as_bytes()).await;
    let _ = socket.shutdown().await;
}

async fn serve(
    socket: TcpStream,
    script: Script,
    log: Arc<Mutex<Vec<Value>>>,
    headers: Arc<Mutex<Vec<http::HeaderMap>>>,
) {
    let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(socket, Capture(headers)).await else {
        return;
    };
    while let Some(Ok(message)) = ws.next().await {
        let Message::Text(text) = message else {
            continue;
        };
        let request: Value = serde_json::from_str(text.as_str()).unwrap();
        log.lock().unwrap().push(request.clone());
        for step in script(&request) {
            let sent = match step {
                Step::Send(event) => ws.send(Message::Text(event.to_string().into())).await,
                Step::Ping => ws.send(Message::Ping(Vec::new().into())).await,
                Step::Pause(millis) => {
                    tokio::time::sleep(Duration::from_millis(millis)).await;
                    Ok(())
                }
                Step::Close => {
                    let _ = ws.close(None).await;
                    return;
                }
                Step::Refuse(code) => {
                    let _ = ws
                        .close(Some(tokio_tungstenite::tungstenite::protocol::CloseFrame {
                            code: code.into(),
                            reason: "refused".into(),
                        }))
                        .await;
                    return;
                }
            };
            if sent.is_err() {
                return;
            }
        }
    }
}

async fn serve_http(
    mut socket: TcpStream,
    script: Script,
    log: Arc<Mutex<Vec<Value>>>,
    headers: Arc<Mutex<Vec<http::HeaderMap>>>,
) {
    let mut data = Vec::new();
    let end = loop {
        let mut buffer = [0; 4096];
        match socket.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(read) => data.extend_from_slice(&buffer[..read]),
        }
        if let Some(end) = data.windows(4).position(|value| value == b"\r\n\r\n") {
            break end + 4;
        }
    };
    let mut captured = http::HeaderMap::new();
    for line in std::str::from_utf8(&data[..end]).unwrap().lines().skip(1) {
        if let Some((name, value)) = line.split_once(':') {
            captured.append(
                http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                http::HeaderValue::from_str(value.trim()).unwrap(),
            );
        }
    }
    let length: usize = captured["content-length"].to_str().unwrap().parse().unwrap();
    headers.lock().unwrap().push(captured);
    while data.len() < end + length {
        let mut buffer = [0; 4096];
        match socket.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(read) => data.extend_from_slice(&buffer[..read]),
        }
    }
    let request: Value = serde_json::from_slice(&data[end..end + length]).unwrap();
    log.lock().unwrap().push(request.clone());
    let steps = script(&request);
    if let Some(Step::Refuse(status)) = steps.first() {
        let body = json!({"error":{"message":"temporary failure","type":"server_error"}});
        let body = body.to_string();
        socket.write_all(format!("HTTP/1.1 {status} Refused\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        return;
    }
    if socket
        .write_all(
            b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n",
        )
        .await
        .is_err()
    {
        return;
    }
    for step in steps {
        match step {
            Step::Send(value) => {
                if socket.write_all(format!("data: {value}\n\n").as_bytes()).await.is_err() {
                    return;
                }
            }
            Step::Pause(millis) => tokio::time::sleep(Duration::from_millis(millis)).await,
            Step::Ping => {
                if socket.write_all(b": ping\n\n").await.is_err() {
                    return;
                }
            }
            Step::Close | Step::Refuse(_) => return,
        }
    }
}

fn delta(text: &str) -> Value {
    json!({"type": "response.output_text.delta", "sequence_number": 1, "item_id": "msg_1",
        "output_index": 0, "content_index": 0, "delta": text, "logprobs": []})
}

fn response(id: &str, status: &str, text: &str) -> Value {
    json!({"id": id, "object": "response", "created_at": 0, "model": "test-model",
        "status": status,
        "output": [{"type": "message", "id": "msg_1", "role": "assistant", "status": status,
            "content": [{"type": "output_text", "text": text, "annotations": []}]}]})
}

fn completed(id: &str, text: &str) -> Value {
    json!({"type": "response.completed", "sequence_number": 9,
        "response": response(id, "completed", text)})
}

fn answer(text: &str) -> Vec<Step> {
    vec![Step::Send(delta(text)), Step::Send(completed("resp_1", text))]
}

fn request(text: &str) -> CreateResponse {
    CreateResponseArgs::default()
        .model("test-model")
        .input(InputParam::Text(text.into()))
        .build()
        .unwrap()
}

fn input(request: &Value) -> &str {
    request["input"].as_str().unwrap_or_default()
}

fn text(responses: &[LlmResponse]) -> String {
    responses
        .iter()
        .flat_map(|response| response.content.iter().flat_map(|content| &content.parts))
        .filter_map(Part::text)
        .collect()
}

/// Every item one request yields, stopping at the first error; panics on a hang.
async fn collect(transport: &WsTransport, text: &str) -> (Vec<LlmResponse>, Option<AdkError>) {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut responses = Vec::new();
        let mut stream = match transport.send_request(request(text)).await {
            Ok(stream) => stream,
            Err(error) => return (responses, Some(error)),
        };
        while let Some(item) = stream.next().await {
            match item {
                Ok(response) => responses.push(response),
                Err(error) => return (responses, Some(error)),
            }
        }
        (responses, None)
    })
    .await
    .expect("the response stream must end")
}

async fn single(script: Script) -> (Vec<LlmResponse>, Option<AdkError>) {
    let server = Server::start(script, accept_all()).await;
    let transport = WsTransport::connect(&server.config()).await.unwrap();
    collect(&transport, "q1").await
}

fn no_retry_delay() -> RetryConfig {
    RetryConfig::default().with_initial_delay(Duration::ZERO).with_max_delay(Duration::ZERO)
}

#[tokio::test]
async fn requests_are_response_create_events() {
    let server = Server::start(Arc::new(|_| answer("hi")), accept_all()).await;
    let transport = WsTransport::connect(&server.config()).await.unwrap();
    let mut with_stream = request("q1");
    with_stream.stream = Some(true);
    let mut stream = transport.send_request(with_stream).await.unwrap();
    while stream.next().await.is_some() {}
    let frame = server.requests.lock().unwrap()[0].clone();
    assert_eq!((&frame["type"], frame.get("stream")), (&json!("response.create"), None));
}

#[tokio::test]
async fn answer_is_streamed_once() {
    let (responses, error) = single(Arc::new(|_| {
        vec![
            Step::Send(delta("The capital ")),
            Step::Send(delta("is Paris.")),
            Step::Send(completed("resp_1", "The capital is Paris.")),
        ]
    }))
    .await;
    assert!(error.is_none(), "{error:?}");
    let last = responses.last().unwrap();
    assert_eq!(
        (text(&responses).as_str(), last.partial, last.turn_complete, last.finish_reason),
        ("The capital is Paris.", false, true, Some(FinishReason::Stop))
    );
}

#[tokio::test]
async fn abandoned_response_does_not_reach_the_next_request() {
    let script: Script = Arc::new(|request| {
        if input(request) == "q1" {
            vec![
                Step::Send(delta("OLD-")),
                Step::Pause(200),
                Step::Send(delta("ANSWER")),
                Step::Send(completed("resp_old", "OLD-ANSWER")),
            ]
        } else {
            answer("NEW-ANSWER")
        }
    });
    let server = Server::start(script, accept_all()).await;
    let transport = WsTransport::connect(&server.config()).await.unwrap();
    let mut stream = transport.send_request(request("q1")).await.unwrap();
    assert_eq!(text(&[stream.next().await.unwrap().unwrap()]), "OLD-");
    drop(stream);
    assert!(!transport.is_connected());

    let (responses, error) = collect(&transport, "q2").await;
    assert!(error.is_none(), "{error:?}");
    assert_eq!((text(&responses).as_str(), server.handshakes()), ("NEW-ANSWER", 2));
}

#[tokio::test]
async fn requests_wait_for_the_open_response() {
    let script: Script = Arc::new(|request| {
        let reply = format!("answer-{}", input(request));
        vec![Step::Pause(100), Step::Send(delta(&reply)), Step::Send(completed("resp", &reply))]
    });
    let server = Server::start(script, accept_all()).await;
    let transport = WsTransport::connect(&server.config()).await.unwrap();
    let (first, second) = tokio::join!(collect(&transport, "q1"), collect(&transport, "q2"));
    assert_eq!(
        (
            text(&first.0),
            text(&second.0),
            first.1.is_none() && second.1.is_none(),
            server.handshakes()
        ),
        ("answer-q1".to_string(), "answer-q2".to_string(), true, 1)
    );
}

#[tokio::test]
async fn incomplete_responses_end_the_stream() {
    let incomplete = |reason: &str| {
        let mut body = response("resp_1", "incomplete", "The capital");
        body["incomplete_details"] = json!({"reason": reason});
        json!({"type": "response.incomplete", "sequence_number": 9, "response": body})
    };
    let truncated = incomplete("max_output_tokens");
    let (responses, error) = single(Arc::new(move |_| {
        vec![Step::Send(delta("The capital")), Step::Send(truncated.clone())]
    }))
    .await;
    assert!(error.is_none(), "{error:?}");
    assert_eq!(responses.last().and_then(|r| r.finish_reason), Some(FinishReason::MaxTokens));

    let interrupted = incomplete("interrupted");
    let (_, error) = single(Arc::new(move |_| vec![Step::Send(interrupted.clone())])).await;
    assert_eq!(error.map(|e| e.code), Some("model.openai_responses.incomplete"));
}

#[tokio::test]
async fn cancelled_and_unknown_terminal_events_end_the_stream() {
    let (_, error) =
        single(Arc::new(|_| vec![Step::Send(json!({"type": "response.cancelled"}))])).await;
    assert_eq!(error.map(|e| e.category), Some(ErrorCategory::Cancelled));

    // An event type this version does not know still ends the response by its status.
    let (responses, error) = single(Arc::new(|_| {
        let mut event = completed("resp_1", "done");
        event["type"] = json!("response.finalized");
        vec![Step::Send(event)]
    }))
    .await;
    assert!(error.is_none(), "{error:?}");
    assert_eq!(text(&responses), "done");
}

#[tokio::test]
async fn close_before_a_terminal_event_is_an_error() {
    let server = Server::start(
        Arc::new(|_| vec![Step::Send(delta("The capital")), Step::Close]),
        accept_all(),
    )
    .await;
    let transport = WsTransport::connect(&server.config()).await.unwrap();
    let (_, error) = collect(&transport, "q1").await;
    let error = error.expect("a closed connection must fail the stream");
    assert_eq!(
        (error.category, error.code),
        (ErrorCategory::Unavailable, "model.openai_responses.unfinished_stream")
    );
    assert!(!error.is_retryable());
    assert!(!transport.is_connected());
}

#[tokio::test]
async fn failed_and_error_events_are_errors() {
    let failed = {
        let mut body = response("resp_1", "failed", "");
        body["error"] = json!({"code": "server_error", "message": "boom"});
        json!({"type": "response.failed", "sequence_number": 9, "response": body})
    };
    let server = Server::start(
        Arc::new(move |request| {
            if input(request) == "q1" {
                vec![Step::Send(failed.clone())]
            } else {
                answer("after failure")
            }
        }),
        accept_all(),
    )
    .await;
    let transport = WsTransport::connect(&server.config()).await.unwrap();
    let (_, error) = collect(&transport, "q1").await;
    assert_eq!(
        error.as_ref().map(|e| (e.category, e.code)),
        Some((ErrorCategory::Unavailable, "model.openai_responses.response_failed"))
    );
    assert!(error.unwrap().is_retryable());
    assert!(transport.is_connected());
    let (next, error) = collect(&transport, "q2").await;
    assert!(error.is_none(), "{error:?}");
    assert_eq!(text(&next), "after failure");
    assert_eq!(server.handshakes(), 1);

    let server = Server::start(Arc::new(|request| {
        if input(request) == "q1" {
            vec![Step::Send(json!({"type": "error", "status": 429,
                "error": {"type": "requests", "code": "rate_limit_exceeded", "message": "slow down"}}))]
        } else {
            answer("after rate limit")
        }
    }), accept_all())
    .await;
    let transport = WsTransport::connect(&server.config()).await.unwrap();
    let (_, error) = collect(&transport, "q1").await;
    assert_eq!(
        error.as_ref().map(|e| (e.category, e.code)),
        Some((ErrorCategory::RateLimited, "model.openai_responses.ws_error"))
    );
    assert!(error.unwrap().is_retryable());
    assert!(transport.is_connected());
    let (next, error) = collect(&transport, "q2").await;
    assert!(error.is_none(), "{error:?}");
    assert_eq!(text(&next), "after rate limit");
    assert_eq!(server.handshakes(), 1);
}

#[tokio::test]
async fn unparseable_completed_response_is_an_error() {
    let (_, error) = single(Arc::new(|_| {
        let mut event = completed("resp_1", "x");
        event["response"]["output"] = json!([{"type": "message", "content": "not-a-list"}]);
        vec![Step::Send(event)]
    }))
    .await;
    assert_eq!(error.map(|e| e.code), Some("model.openai_responses.parse"));
}

#[tokio::test]
async fn idle_connection_closed_by_the_server_is_replaced() {
    let script: Script = Arc::new(|request| {
        let mut steps = answer(&format!("answer-{}", input(request)));
        steps.push(Step::Close);
        steps
    });
    let server = Server::start(script, accept_all()).await;
    let transport = WsTransport::connect(&server.config()).await.unwrap();
    assert_eq!(text(&collect(&transport, "q1").await.0), "answer-q1");
    tokio::time::sleep(Duration::from_millis(100)).await;
    let (responses, error) = collect(&transport, "q2").await;
    assert!(error.is_none(), "{error:?}");
    assert_eq!((text(&responses).as_str(), server.handshakes()), ("answer-q2", 2));
}

#[tokio::test]
async fn transient_handshake_failures_are_retried() {
    let server = Server::start(
        Arc::new(|_| answer("hi")),
        Arc::new(|attempt| (attempt == 0).then_some(503)),
    )
    .await;
    let transport =
        WsTransport::connect_with_retry_config(&server.config(), no_retry_delay()).await.unwrap();
    assert_eq!((text(&collect(&transport, "q1").await.0).as_str(), server.handshakes()), ("hi", 2));
}

#[tokio::test]
async fn rejected_handshakes_are_not_retried() {
    let server = Server::start(Arc::new(|_| answer("hi")), Arc::new(|_| Some(401))).await;
    let error = WsTransport::connect_with_retry_config(&server.config(), no_retry_delay())
        .await
        .err()
        .expect("a 401 handshake must fail");
    assert_eq!(
        (error.category, error.details.upstream_status_code, server.handshakes()),
        (ErrorCategory::Unauthorized, Some(401), 1)
    );

    // A reconnect that meets a 401 stops at once as well.
    let script: Script = Arc::new(|_| {
        let mut steps = answer("hi");
        steps.push(Step::Close);
        steps
    });
    let server = Server::start(script, Arc::new(|attempt| (attempt > 0).then_some(401))).await;
    let transport =
        WsTransport::connect_with_retry_config(&server.config(), no_retry_delay()).await.unwrap();
    assert_eq!(text(&collect(&transport, "q1").await.0), "hi");
    tokio::time::sleep(Duration::from_millis(100)).await;
    let (_, error) = collect(&transport, "q2").await;
    assert_eq!(
        (error.map(|e| e.category), server.handshakes()),
        (Some(ErrorCategory::Unauthorized), 2)
    );
}

#[tokio::test]
async fn client_adapts_before_selecting_transport() {
    use adk_core::{Content, Llm, LlmRequest};
    use adk_model::openai::{OpenAIResponsesClient, WsOptions};
    let server = Server::start(Arc::new(|_| answer("adapted")), accept_all()).await;
    let observed = Arc::new(AtomicUsize::new(0));
    let observation = observed.clone();
    let client = OpenAIResponsesClient::new(server.config().with_organization("fixture-org"))
        .unwrap()
        .with_retry_config(RetryConfig::disabled())
        .with_request_adapter(Arc::new(|body, headers| {
            body["store"] = json!(false);
            body["service_tier"] = json!("priority");
            headers.insert("session-id", http::HeaderValue::from_static("session"));
            headers.insert("openai-beta", http::HeaderValue::from_static("caller_feature=v1"));
            Ok(())
        }))
        .with_response_observer(Arc::new(move |headers| {
            assert_eq!(headers["x-handshake"], "observed");
            observation.fetch_add(1, Ordering::SeqCst);
        }))
        .with_websocket(WsOptions::default());
    client.preconnect().await.unwrap();
    assert_eq!(server.handshakes(), 1);
    assert!(server.requests.lock().unwrap().is_empty());
    let mut stream = client
        .generate_content(
            LlmRequest::new("test-model", vec![Content::new("user").with_text("first")]),
            true,
        )
        .await
        .unwrap();
    while let Some(response) = stream.next().await {
        response.unwrap();
    }
    assert_eq!(server.headers.lock().unwrap()[0]["session-id"], "session");
    assert_eq!(server.headers.lock().unwrap()[0]["openai-organization"], "fixture-org");
    assert_eq!(observed.load(Ordering::SeqCst), 1);
    assert_eq!(
        server.headers.lock().unwrap()[0]["openai-beta"],
        "caller_feature=v1,responses_websockets=2026-02-06"
    );
    let request = server.requests.lock().unwrap()[0].clone();
    assert_eq!(request["store"], false);
    assert_eq!(request["service_tier"], "priority");
    assert!(request.get("stream").is_none());
    assert_eq!(server.handshakes(), 1);
}

#[tokio::test]
async fn adapter_can_remove_default_authorization() {
    use adk_core::{Content, Llm, LlmRequest};
    use adk_model::openai::{OpenAIResponsesClient, WsOptions};
    for fallback in [false, true] {
        let server = Server::combined(
            Arc::new(move |request| {
                if fallback && request["type"] == "response.create" {
                    vec![Step::Refuse(1008)]
                } else {
                    answer("custom authorization")
                }
            }),
            accept_all(),
            true,
        )
        .await;
        let client = OpenAIResponsesClient::new(server.config())
            .unwrap()
            .with_retry_config(RetryConfig::disabled())
            .with_request_adapter(Arc::new(|_, headers| {
                headers.remove("authorization");
                headers.remove("x-api-key");
                headers.insert("x-provider-token", http::HeaderValue::from_static("custom grant"));
                Ok(())
            }))
            .with_websocket(WsOptions { fallback_to_http: true, ..Default::default() });
        client.preconnect().await.unwrap();
        let mut stream = client
            .generate_content(
                LlmRequest::new("test-model", vec![Content::new("user").with_text("first")]),
                true,
            )
            .await
            .unwrap();
        let mut complete = false;
        while let Some(response) = stream.next().await {
            complete |= response.unwrap().turn_complete;
        }
        assert!(complete);
        let headers = server.headers.lock().unwrap();
        assert_eq!(headers.len(), if fallback { 2 } else { 1 });
        for headers in headers.iter() {
            assert_eq!(headers["x-provider-token"], "custom grant");
            assert!(!headers.contains_key("authorization"));
            assert!(!headers.contains_key("x-api-key"));
        }
    }
}

#[tokio::test]
async fn continuation_uses_only_the_new_input() {
    let server = Server::start(Arc::new(|_| answer("answer")), accept_all()).await;
    let transport = WsTransport::lazy(&server.base, "key", Default::default());
    let mut first: CreateResponse = serde_json::from_value(
        json!({"model":"test-model","store":false,"input":[{"role":"user","content":"first"}]}),
    )
    .unwrap();
    let mut stream = transport.send_request(first.clone()).await.unwrap();
    while let Some(response) = stream.next().await {
        response.unwrap();
    }
    first.input=serde_json::from_value(json!([{"role":"user","content":"first"},{"role":"assistant","content":"answer"},{"role":"user","content":"next"}])).unwrap();
    let mut stream = transport.send_request(first).await.unwrap();
    while let Some(response) = stream.next().await {
        response.unwrap();
    }
    let requests = server.requests.lock().unwrap();
    assert_eq!(server.headers.lock().unwrap()[0]["authorization"], "Bearer key");
    assert_eq!(requests[1]["previous_response_id"], "resp_1");
    assert_eq!(requests[1]["input"], json!([{"type":"message","role":"user","content":"next"}]));
    assert_eq!(server.handshakes(), 1);
}

#[tokio::test]
async fn missing_continuation_resends_full_input_once() {
    let server = Server::start(
        Arc::new(|request| {
            if request.get("previous_response_id").is_some() {
                vec![Step::Send(
                    json!({"type":"error","error":{"code":"previous_response_not_found"}}),
                )]
            } else {
                answer("answer")
            }
        }),
        accept_all(),
    )
    .await;
    let transport = WsTransport::lazy(&server.base, "key", Default::default());
    for input in [
        json!([{"role":"user","content":"first"}]),
        json!([{"role":"user","content":"first"},{"role":"assistant","content":"answer"},{"role":"user","content":"next"}]),
    ] {
        let request =
            serde_json::from_value(json!({"model":"test-model","store":false,"input":input}))
                .unwrap();
        let mut stream = transport.send_request(request).await.unwrap();
        while let Some(response) = stream.next().await {
            response.unwrap();
        }
    }
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(requests[2].get("previous_response_id").is_none());
    assert_eq!(requests[2]["input"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn read_timeout_is_classified() {
    let server = Server::start(Arc::new(|_| vec![Step::Pause(1000)]), accept_all()).await;
    let transport = WsTransport::lazy(
        &server.base,
        "key",
        adk_model::openai::WsOptions {
            read_timeout: Duration::from_millis(10),
            ..Default::default()
        },
    );
    let error = transport.send_request(request("first")).await.err().unwrap();
    assert_eq!(error.category, ErrorCategory::Timeout);
    assert!(!error.is_retryable(), "the server may have accepted the request");
}

#[tokio::test]
async fn handshake_timeout_is_classified() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (_socket, _) = listener.accept().await.unwrap();
        std::future::pending::<()>().await;
    });
    let transport = WsTransport::lazy(
        &endpoint,
        "key",
        adk_model::openai::WsOptions {
            connect_timeout: Duration::from_millis(10),
            ..Default::default()
        },
    );
    let error = transport.preconnect(http::HeaderMap::new()).await.unwrap_err();
    assert_eq!(error.category, ErrorCategory::Timeout);
    assert!(!transport.is_connected());
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn adapted_headers_replace_the_socket() {
    use adk_core::{Content, Llm, LlmRequest};
    use adk_model::openai::{OpenAIResponsesClient, WsOptions};
    let server = Server::start(Arc::new(|_| answer("answer")), accept_all()).await;
    let token = Arc::new(Mutex::new("first"));
    let current = token.clone();
    let client = OpenAIResponsesClient::new(server.config())
        .unwrap()
        .with_request_adapter(Arc::new(move |_, headers| {
            headers.insert(
                "authorization",
                http::HeaderValue::from_str(&current.lock().unwrap()).unwrap(),
            );
            Ok(())
        }))
        .with_websocket(WsOptions::default());
    for value in ["first", "first", "second"] {
        *token.lock().unwrap() = value;
        client.preconnect().await.unwrap();
        let mut stream = client
            .generate_content(
                LlmRequest::new("test-model", vec![Content::new("user").with_text("request")]),
                true,
            )
            .await
            .unwrap();
        while let Some(result) = stream.next().await {
            result.unwrap();
        }
    }
    assert_eq!(server.handshakes(), 2);
    let headers = server.headers.lock().unwrap();
    assert_eq!(headers[0]["authorization"], "first");
    assert_eq!(headers[1]["authorization"], "second");
}

#[tokio::test]
async fn unused_preconnection_closes_without_another_call() {
    use adk_model::openai::WsOptions;
    for lifetime in [WsOptions::default().preconnect_timeout, Duration::from_secs(5)] {
        let server = Server::start(Arc::new(|_| answer("answer")), accept_all()).await;
        let options = WsOptions { preconnect_timeout: lifetime, ..Default::default() };
        let transport = WsTransport::lazy(&server.base, "key", options);
        transport.preconnect(http::HeaderMap::new()).await.unwrap();
        assert!(transport.is_connected());
        assert!(server.requests.lock().unwrap().is_empty());
        // The socket owner, rather than a later request or external timer, closes it.
        tokio::time::pause();
        tokio::task::yield_now().await;
        tokio::time::advance(lifetime - Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        assert!(transport.is_connected());
        tokio::time::advance(Duration::from_millis(1)).await;
        tokio::time::timeout(Duration::from_secs(1), async {
            while transport.is_connected() {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        assert!(!transport.is_connected());
        tokio::time::resume();
        assert!(collect(&transport, "request").await.1.is_none());
        assert_eq!(server.handshakes(), 2);
    }
}

#[tokio::test]
async fn max_reasoning_preconnects_with_generation_headers() {
    use adk_core::{Content, Llm, LlmRequest};
    use adk_model::openai::{OpenAIReasoningEffort, OpenAIResponsesClient, WsOptions};
    let server = Server::start(Arc::new(|_| answer("deliberate")), accept_all()).await;
    let client = OpenAIResponsesClient::new_with_reasoning_effort(
        server.config(),
        OpenAIReasoningEffort::Max,
    )
    .unwrap()
    .with_retry_config(RetryConfig::disabled())
    .with_request_adapter(Arc::new(|_, headers| {
        headers.insert("session-id", http::HeaderValue::from_static("session"));
        Ok(())
    }))
    .with_websocket(WsOptions::default());
    client.preconnect().await.unwrap();
    assert_eq!(server.handshakes(), 1);
    assert!(server.requests.lock().unwrap().is_empty());
    let mut stream = client
        .generate_content(
            LlmRequest::new("test-model", vec![Content::new("user").with_text("think")]),
            true,
        )
        .await
        .unwrap();
    while let Some(response) = stream.next().await {
        response.unwrap();
    }
    // The generation reuses the preconnected socket, so both prepared the same headers.
    assert_eq!(server.handshakes(), 1);
    assert_eq!(server.headers.lock().unwrap()[0]["session-id"], "session");
    assert_eq!(server.requests.lock().unwrap()[0]["reasoning"]["effort"], "max");
}

#[tokio::test]
async fn error_after_response_start_closes_the_socket() {
    let server = Server::start(
        Arc::new(|request| {
            if input(request) == "q1" {
                vec![
                    Step::Send(json!({"type": "response.created", "sequence_number": 0,
                        "response": response("resp_1", "in_progress", "")})),
                    Step::Send(delta("partial")),
                    Step::Send(json!({"type": "error", "status": 500,
                        "error": {"type": "server_error", "code": "server_error", "message": "boom"}})),
                    Step::Send(completed("resp_1", "stale")),
                ]
            } else {
                answer("fresh")
            }
        }),
        accept_all(),
    )
    .await;
    let transport = WsTransport::connect(&server.config()).await.unwrap();
    let (_, error) = collect(&transport, "q1").await;
    assert_eq!(error.map(|e| e.code), Some("model.openai_responses.ws_error"));
    assert!(!transport.is_connected());
    let (next, error) = collect(&transport, "q2").await;
    assert!(error.is_none(), "{error:?}");
    assert_eq!(text(&next), "fresh");
    assert_eq!(server.handshakes(), 2);
}

#[tokio::test]
async fn expiry_preserves_an_active_lease() {
    for maximum_age in [false, true] {
        let server = Server::start(
            Arc::new(|_| {
                vec![
                    Step::Send(delta("answer")),
                    Step::Pause(40),
                    Step::Send(completed("response", "answer")),
                ]
            }),
            accept_all(),
        )
        .await;
        let mut options = adk_model::openai::WsOptions::default();
        if maximum_age {
            options.max_age = Duration::from_millis(10);
        } else {
            options.idle_timeout = Duration::from_millis(10);
        }
        let transport = WsTransport::lazy(&server.base, "key", options);
        let mut stream = transport.send_request(request("first")).await.unwrap();
        assert!(stream.next().await.unwrap().unwrap().partial);
        tokio::time::sleep(Duration::from_millis(20)).await;
        transport.expire();
        assert!(transport.is_connected());
        assert!(stream.next().await.unwrap().unwrap().turn_complete);
        assert!(stream.next().await.is_none());
        tokio::time::sleep(Duration::from_millis(20)).await;
        transport.expire();
        assert!(!transport.is_connected());
        assert!(collect(&transport, "second").await.1.is_none());
        assert_eq!(server.handshakes(), 2);
    }
}

#[tokio::test]
async fn routes_final_background_requests() {
    use adk_core::{Content, GenerateContentConfig, Llm, LlmRequest};
    use adk_model::openai::{OpenAIResponsesClient, WsOptions};
    use futures::TryStreamExt;

    for (model, explicit, adapted, background) in [
        ("test-model", Some(true), None, true),
        ("o3-deep-research", None, None, true),
        ("test-model", None, Some(true), true),
        ("test-model", Some(true), Some(false), false),
        ("o3-deep-research", None, Some(false), false),
        ("test-model", None, None, false),
    ] {
        let server = Server::combined(Arc::new(|_| answer("done")), accept_all(), true).await;
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let client = OpenAIResponsesClient::new(
            OpenAIResponsesConfig::new("sk-test", model).with_base_url(&server.base),
        )
        .unwrap()
        .with_retry_config(RetryConfig::disabled())
        .with_request_adapter(Arc::new(move |body, _| {
            observed.fetch_add(1, Ordering::SeqCst);
            if let Some(background) = adapted {
                body["background"] = json!(background);
            }
            Ok(())
        }))
        .with_websocket(WsOptions::default());
        let mut request = LlmRequest::new(model, vec![Content::new("user").with_text("first")]);
        if let Some(background) = explicit {
            let mut config = GenerateContentConfig::default();
            config.extensions.insert("openai".into(), json!({"background":background}));
            request.config = Some(config);
        }
        let responses: Vec<_> =
            client.generate_content(request, true).await.unwrap().try_collect().await.unwrap();
        assert_eq!(text(&responses), "done");
        assert!(responses.last().unwrap().turn_complete);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            server.handshakes(),
            usize::from(!background),
            "{model}/{explicit:?}/{adapted:?}"
        );
        let requests = server.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        if background {
            assert_eq!(requests[0]["background"], true);
            assert_eq!(requests[0]["stream"], true);
            assert!(requests[0].get("type").is_none());
        } else {
            assert_eq!(requests[0]["type"], "response.create");
            assert!(requests[0].get("background").is_none());
        }
    }
}

#[tokio::test]
async fn fallback_retries_adapt_original_requests() {
    use adk_core::{Content, Llm, LlmRequest};
    use adk_model::openai::{OpenAIResponsesClient, WsOptions};
    use futures::TryStreamExt;

    let attempts = Arc::new(AtomicUsize::new(0));
    let observed = attempts.clone();
    let server = Server::combined(
        Arc::new(move |_| {
            if observed.fetch_add(1, Ordering::SeqCst) == 0 {
                vec![Step::Refuse(503)]
            } else {
                answer("done")
            }
        }),
        Arc::new(|_| Some(426)),
        true,
    )
    .await;
    let calls = Arc::new(AtomicUsize::new(0));
    let adapted = calls.clone();
    let client = OpenAIResponsesClient::new(server.config())
        .unwrap()
        .with_retry_config(no_retry_delay().with_max_retries(1))
        .with_default_headers(http::HeaderMap::from_iter([(
            http::HeaderName::from_static("x-original"),
            http::HeaderValue::from_static("frozen"),
        )]))
        .unwrap()
        .with_request_adapter(Arc::new(move |body, headers| {
            let attempt = adapted.fetch_add(1, Ordering::SeqCst) + 1;
            body["input"].as_array_mut().unwrap().push(json!({
                "role":"user", "content":format!("nonce {attempt}")
            }));
            headers.append("x-nonce", http::HeaderValue::from_str(&attempt.to_string()).unwrap());
            Ok(())
        }))
        .with_websocket(WsOptions { fallback_to_http: true, ..Default::default() });
    let responses: Vec<_> = client
        .generate_content(
            LlmRequest::new("test-model", vec![Content::new("user").with_text("first")]),
            true,
        )
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    assert_eq!(text(&responses), "done");
    assert!(responses.last().unwrap().turn_complete);
    assert_eq!(server.handshakes(), 1);
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert_eq!(calls.load(Ordering::SeqCst), 2, "only a new HTTP attempt reruns adaptation");
    let requests = server.requests.lock().unwrap();
    let headers = server.headers.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(headers.len(), 2);
    assert_eq!(requests[0]["input"][0], requests[1]["input"][0]);
    for (index, (body, headers)) in requests.iter().zip(headers.iter()).enumerate() {
        assert_eq!(body["stream"], true);
        assert_eq!(body["input"].as_array().unwrap().len(), 2);
        assert_eq!(body["input"][1]["content"], format!("nonce {}", index + 1));
        assert_eq!(headers["x-original"], "frozen");
        let values: Vec<_> =
            headers.get_all("x-nonce").iter().map(|value| value.to_str().unwrap()).collect();
        assert_eq!(values, vec![(index + 1).to_string()]);
    }
}

#[tokio::test]
async fn fallback_preserves_the_adapted_request() {
    use adk_core::{Content, Llm, LlmRequest};
    use adk_model::openai::{OpenAIResponsesClient, TransportEvent, WsOptions, WsRefusal};
    #[derive(Clone, Copy)]
    enum Refusal {
        Status(u16),
        Close(u16),
        Limit,
    }
    for refusal in [
        Refusal::Status(404),
        Refusal::Status(426),
        Refusal::Status(401),
        Refusal::Close(1008),
        Refusal::Close(1013),
        Refusal::Limit,
    ] {
        let server = Server::combined(Arc::new(move |request| {
            if request["type"] != "response.create" { return answer("answer"); }
            match refusal {
                Refusal::Close(code) => vec![Step::Refuse(code)],
                Refusal::Limit => vec![Step::Send(json!({"type":"error","error":{"code":"websocket_connection_limit_reached"}}))],
                Refusal::Status(_) => unreachable!(),
            }
        }),Arc::new(move |_| match refusal { Refusal::Status(status) => Some(status), _=>None }),true).await;
        let adapted = Arc::new(AtomicUsize::new(0));
        let calls = adapted.clone();
        let events = Arc::new(Mutex::new(Vec::new()));
        let observed = events.clone();
        let client = OpenAIResponsesClient::new(server.config())
            .unwrap()
            .with_retry_config(RetryConfig::disabled())
            .with_request_adapter(Arc::new(move |body, headers| {
                calls.fetch_add(1, Ordering::SeqCst);
                body["store"] = json!(false);
                body["service_tier"] = json!("priority");
                headers.insert("authorization", http::HeaderValue::from_static("custom grant"));
                headers.insert("session-id", http::HeaderValue::from_static("session"));
                Ok(())
            }))
            .with_websocket(WsOptions {
                fallback_to_http: true,
                observe: Arc::new(move |event| observed.lock().unwrap().push(event)),
                ..Default::default()
            });
        let mut stream = client
            .generate_content(
                LlmRequest::new("test-model", vec![Content::new("user").with_text("first")]),
                true,
            )
            .await
            .unwrap();
        let mut complete = false;
        while let Some(response) = stream.next().await {
            complete |= response.unwrap().turn_complete;
        }
        assert!(complete);
        assert_eq!(adapted.load(Ordering::SeqCst), 1);
        assert_eq!(server.handshakes(), 1);
        let requests = server.requests.lock().unwrap();
        let http = requests.last().unwrap();
        assert_eq!(http["store"], false);
        assert_eq!(http["stream"], true);
        assert_eq!(http["service_tier"], "priority");
        let headers = server.headers.lock().unwrap();
        assert_eq!(headers.last().unwrap()["authorization"], "custom grant");
        assert_eq!(headers.last().unwrap()["session-id"], "session");
        let expected = if matches!(refusal, Refusal::Close(1013) | Refusal::Limit) {
            WsRefusal::Transient
        } else {
            WsRefusal::Definitive
        };
        let events = events.lock().unwrap();
        assert_eq!(events.len(), 2, "{events:?}");
        assert!(matches!(events[0], TransportEvent::Fallback { refusal } if refusal == expected));
        assert!(
            matches!(events[1], TransportEvent::HttpCompleted { refusal } if refusal == expected)
        );
    }
}

#[tokio::test]
async fn fallback_is_disabled_by_default() {
    use adk_core::{Content, Llm, LlmRequest};
    use adk_model::openai::{OpenAIResponsesClient, TransportEvent, WsOptions};

    assert!(!WsOptions::default().fallback_to_http);
    for failure in ["handshake", "close", "timeout"] {
        let server = Server::combined(
            Arc::new(move |request| {
                if request["type"] != "response.create" {
                    return answer("unexpected HTTP");
                }
                if failure == "close" { vec![Step::Close] } else { vec![Step::Pause(300)] }
            }),
            Arc::new(move |_| (failure == "handshake").then_some(426)),
            true,
        )
        .await;
        let observations = Arc::new(Mutex::new(Vec::new()));
        let observed = observations.clone();
        let client =
            OpenAIResponsesClient::new(server.config()).unwrap().with_websocket(WsOptions {
                first_event_timeout: Duration::from_millis(50),
                observe: Arc::new(move |event| observed.lock().unwrap().push(event)),
                ..Default::default()
            });
        let error = match client
            .generate_content(
                LlmRequest::new("test-model", vec![Content::new("user").with_text("first")]),
                true,
            )
            .await
        {
            Ok(_) => panic!("failed WebSocket attempt must be returned"),
            Err(error) => error,
        };
        if failure != "handshake" {
            assert!(!error.is_retryable(), "unacknowledged delivery is uncertain");
        }
        assert_eq!(server.handshakes(), 1);
        let requests = server.requests.lock().unwrap();
        assert_eq!(requests.len(), usize::from(failure != "handshake"));
        assert!(requests.iter().all(|request| request["type"] == "response.create"));
        assert!(!observations.lock().unwrap().iter().any(|event| matches!(
            event,
            TransportEvent::Fallback { .. } | TransportEvent::HttpCompleted { .. }
        )));
    }
}

#[tokio::test]
async fn first_event_deadline_bounds_unacknowledged_wait() {
    use adk_core::{Content, Llm, LlmRequest};
    use adk_model::openai::{OpenAIResponsesClient, TransportEvent, WsOptions, WsRefusal};
    for control_frames in [false, true] {
        let server = Server::combined(
            Arc::new(move |request| {
                if request["type"] != "response.create" {
                    return answer("HTTP fallback");
                }
                if control_frames {
                    vec![Step::Ping,Step::Pause(60),Step::Ping,Step::Pause(60),
                        Step::Ping,Step::Pause(60),Step::Send(json!({"type":"response.completed","sequence_number":1,"response":response("resp_1","completed","late")}))]
                } else {
                    vec![Step::Pause(300),Step::Send(json!({"type":"response.completed","sequence_number":1,"response":response("resp_1","completed","late")}))]
                }
            }),
            accept_all(),
            true,
        ).await;
        let observations = Arc::new(Mutex::new(Vec::new()));
        let observed = observations.clone();
        let client = OpenAIResponsesClient::new(server.config())
            .unwrap()
            .with_retry_config(RetryConfig::disabled())
            .with_websocket(WsOptions {
                fallback_to_http: true,
                first_event_timeout: Duration::from_millis(100),
                read_timeout: Duration::from_secs(5),
                observe: Arc::new(move |event| observed.lock().unwrap().push(event)),
                ..Default::default()
            });
        let mut stream = client
            .generate_content(
                LlmRequest::new("test-model", vec![Content::new("user").with_text("first")]),
                true,
            )
            .await
            .unwrap();
        let mut complete = false;
        while let Some(response) = stream.next().await {
            complete |= response.unwrap().turn_complete;
        }
        assert!(complete);
        assert_eq!(server.handshakes(), 1);
        assert_eq!(server.requests.lock().unwrap().len(), 2);
        let observations = observations.lock().unwrap();
        assert!(observations.iter().any(|event| matches!(
            event,
            TransportEvent::HttpCompleted { refusal: WsRefusal::Transient }
        )));
        assert!(
            !observations.iter().any(|event| matches!(event, TransportEvent::WebSocket { .. }))
        );
    }
}

#[tokio::test]
async fn provider_acknowledgment_prevents_http_replay() {
    use adk_core::{Content, Llm, LlmRequest};
    use adk_model::openai::{OpenAIResponsesClient, TransportEvent, WsOptions};
    for event in ["response.created", "response.in_progress"] {
        for completes in [false, true] {
            let server = Server::combined(
                Arc::new(move |request| {
                    if request["type"] != "response.create" {
                        return answer("unexpected HTTP");
                    }
                    let mut steps = vec![
                        Step::Send(json!({"type":event,"sequence_number":0,
                        "response":response("resp_1","in_progress","")})),
                        Step::Pause(150),
                    ];
                    steps.extend(answer("acknowledged"));
                    steps
                }),
                accept_all(),
                true,
            )
            .await;
            let observations = Arc::new(Mutex::new(Vec::new()));
            let observed = observations.clone();
            let client = OpenAIResponsesClient::new(server.config())
                .unwrap()
                .with_retry_config(RetryConfig::disabled())
                .with_websocket(WsOptions {
                    fallback_to_http: true,
                    first_event_timeout: Duration::from_millis(100),
                    read_timeout: Duration::from_millis(if completes { 500 } else { 50 }),
                    observe: Arc::new(move |event| observed.lock().unwrap().push(event)),
                    ..Default::default()
                });
            let mut stream = client
                .generate_content(
                    LlmRequest::new("test-model", vec![Content::new("user").with_text("first")]),
                    true,
                )
                .await
                .unwrap();
            let mut complete = false;
            let mut error = None;
            while let Some(response) = stream.next().await {
                match response {
                    Ok(response) => complete |= response.turn_complete,
                    Err(failure) => {
                        error = Some(failure);
                        break;
                    }
                }
            }
            assert_eq!(complete, completes);
            if completes {
                assert!(error.is_none());
            } else {
                let error = error.unwrap();
                assert_eq!(
                    (error.category, error.code),
                    (ErrorCategory::Timeout, "model.openai_responses.timeout")
                );
                assert!(!error.is_retryable());
            }
            assert_eq!(server.requests.lock().unwrap().len(), 1);
            assert!(
                observations
                    .lock()
                    .unwrap()
                    .iter()
                    .all(|event| matches!(event, TransportEvent::WebSocket { .. }))
            );
        }
    }
}

#[tokio::test]
async fn honors_environment_proxy_bypass() {
    const CHILD: &str = "ADK_WS_PROXY_FIXTURE";
    if let Ok(mode) = std::env::var(CHILD) {
        let server = Server::start(Arc::new(|_| answer("bypass")), accept_all()).await;
        let transport = WsTransport::lazy(&server.base, "key", Default::default());
        transport.preconnect(http::HeaderMap::new()).await.unwrap();
        let bypass = mode == "bypass";
        assert_eq!(transport.is_connected(), bypass);
        assert_eq!(server.handshakes(), usize::from(bypass));
        if bypass {
            let (output, error) = collect(&transport, "q1").await;
            assert!(error.is_none(), "{error:?}");
            assert_eq!(text(&output), "bypass");
        }
        return;
    }
    for mode in ["proxy", "bypass"] {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command.args(["--exact", "honors_environment_proxy_bypass", "--nocapture"]);
        for name in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
            "NO_PROXY",
            "no_proxy",
            "REQUEST_METHOD",
        ] {
            command.env_remove(name);
        }
        command.env(CHILD, mode).env("ALL_PROXY", "http://127.0.0.1:1");
        if mode == "bypass" {
            command.env("NO_PROXY", "127.0.0.1");
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{mode}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
