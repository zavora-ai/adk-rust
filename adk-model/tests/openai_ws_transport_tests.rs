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

enum Step {
    Send(Value),
    Pause(u64),
    /// Waits until a request with this input has arrived on any connection.
    WaitFor(&'static str),
    Close,
}

type Script = Arc<dyn Fn(&Value) -> Vec<Step> + Send + Sync>;
type Plan = Arc<dyn Fn(usize) -> Option<u16> + Send + Sync>;

/// A WebSocket server that answers each connection's requests one at a time, in order.
struct Server {
    base: String,
    handshakes: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<Value>>>,
}

impl Server {
    async fn start(script: Script, plan: Plan) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let handshakes = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (count, log) = (handshakes.clone(), requests.clone());
        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                let connection = count.fetch_add(1, Ordering::SeqCst);
                let refusal = plan(connection);
                let (script, log) = (script.clone(), log.clone());
                tokio::spawn(async move {
                    match refusal {
                        Some(status) => refuse(socket, status).await,
                        None => serve(socket, connection, script, log).await,
                    }
                });
            }
        });
        Self { base, handshakes, requests }
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

/// Serves one connection; each logged request carries the connection's number in `__conn`.
async fn serve(socket: TcpStream, connection: usize, script: Script, log: Arc<Mutex<Vec<Value>>>) {
    let Ok(mut ws) = tokio_tungstenite::accept_async(socket).await else { return };
    while let Some(Ok(message)) = ws.next().await {
        let Message::Text(text) = message else { continue };
        let request: Value = serde_json::from_str(text.as_str()).unwrap();
        let mut logged = request.clone();
        logged["__conn"] = json!(connection);
        log.lock().unwrap().push(logged);
        for step in script(&request) {
            let sent = match step {
                Step::Send(event) => ws.send(Message::Text(event.to_string().into())).await,
                Step::Pause(millis) => {
                    tokio::time::sleep(Duration::from_millis(millis)).await;
                    Ok(())
                }
                Step::WaitFor(other) => {
                    while !log.lock().unwrap().iter().any(|request| input(request) == other) {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    Ok(())
                }
                Step::Close => {
                    let _ = ws.close(None).await;
                    return;
                }
            };
            if sent.is_err() {
                return;
            }
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
async fn concurrent_requests_use_their_own_connections() {
    // Each answer waits for the other request: on one shared connection neither would
    // ever be sent, and `collect` would panic on the hang.
    let script: Script = Arc::new(|request| {
        let other = if input(request) == "q1" { "q2" } else { "q1" };
        let reply = format!("answer-{}", input(request));
        vec![Step::WaitFor(other), Step::Send(delta(&reply)), Step::Send(completed("resp", &reply))]
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
        ("answer-q1".to_string(), "answer-q2".to_string(), true, 2)
    );
}

#[tokio::test]
async fn a_full_pool_fails_without_sending_after_the_acquire_timeout() {
    let script: Script = Arc::new(|_| {
        vec![Step::Pause(1_000), Step::Send(delta("late")), Step::Send(completed("resp", "late"))]
    });
    let server = Server::start(script, accept_all()).await;
    let transport = WsTransport::connect(&server.config())
        .await
        .unwrap()
        .with_max_connections(1)
        .with_acquire_timeout(Duration::from_millis(100));
    let mut open = transport.send_request(request("q1")).await.unwrap();
    let (_, error) = collect(&transport, "q2").await;
    let error = error.expect("the second request must not wait past the acquire timeout");
    assert_eq!(
        (error.category, error.code, error.is_retryable()),
        (ErrorCategory::Unavailable, "model.openai_responses.ws_pool_timeout", true)
    );
    assert!(server.requests.lock().unwrap().iter().all(|request| input(request) != "q2"));
    while open.next().await.is_some() {}
}

#[tokio::test]
async fn a_continuation_takes_the_connection_of_its_previous_response() {
    let script: Script = Arc::new(|request| {
        let id = format!("resp_{}", input(request));
        let mut steps = vec![];
        if let Some(other) = match input(request) {
            "q1" => Some("q2"),
            "q2" => Some("q1"),
            _ => None,
        } {
            steps.push(Step::WaitFor(other));
        }
        steps.push(Step::Send(completed(&id, "ok")));
        steps
    });
    let server = Server::start(script, accept_all()).await;
    let transport = WsTransport::new(&server.config()).unwrap();
    tokio::join!(collect(&transport, "q1"), collect(&transport, "q2"));

    // Each continuation names a different earlier response, so no fixed choice of idle
    // connection satisfies both.
    for (text, previous) in [("q3", "resp_q1"), ("q4", "resp_q2")] {
        let mut continuation = request(text);
        continuation.previous_response_id = Some(previous.to_string());
        continuation.store = Some(false);
        let mut stream = transport.send_request(continuation).await.unwrap();
        while stream.next().await.is_some() {}
    }

    let requests = server.requests.lock().unwrap();
    let connection = |text: &str| {
        requests
            .iter()
            .find(|request| input(request) == text)
            .map(|request| request["__conn"].clone())
    };
    assert_eq!(
        (connection("q3"), connection("q4"), server.handshakes()),
        (connection("q1"), connection("q2"), 2)
    );
}

#[tokio::test]
async fn a_silent_server_fails_the_stream_at_the_read_timeout() {
    let server = Server::start(Arc::new(|_| vec![Step::Pause(5_000)]), accept_all()).await;
    let transport = WsTransport::connect(&server.config())
        .await
        .unwrap()
        .with_read_timeout(Duration::from_millis(100));
    let (_, error) = collect(&transport, "q1").await;
    let error = error.expect("a silent server must fail the stream");
    assert_eq!(
        (error.category, error.code, error.is_retryable()),
        (ErrorCategory::Timeout, "model.openai_responses.ws_timeout", false)
    );
    assert!(!transport.is_connected());
}

#[tokio::test]
async fn a_request_made_while_holding_the_final_chunk_does_not_wait_on_itself() {
    let server = Server::start(Arc::new(|request| answer(input(request))), accept_all()).await;
    let transport = WsTransport::connect(&server.config())
        .await
        .unwrap()
        .with_max_connections(1)
        .with_acquire_timeout(Duration::from_secs(2));
    let mut stream = transport.send_request(request("q1")).await.unwrap();
    let mut last = None;
    while let Some(item) = stream.next().await {
        let item = item.unwrap();
        if !item.partial {
            last = Some(item);
            break;
        }
    }
    assert!(last.is_some());
    // The stream is still alive, as when an after-model callback calls the same model.
    let (responses, error) = collect(&transport, "q2").await;
    assert!(error.is_none(), "{error:?}");
    assert_eq!((text(&responses).as_str(), server.handshakes()), ("q2", 1));
    drop(stream);
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
    assert!(!transport.is_connected());
}

#[tokio::test]
async fn failed_and_error_events_are_errors() {
    let failed = {
        let mut body = response("resp_1", "failed", "");
        body["error"] = json!({"code": "server_error", "message": "boom"});
        json!({"type": "response.failed", "sequence_number": 9, "response": body})
    };
    let (_, error) = single(Arc::new(move |_| vec![Step::Send(failed.clone())])).await;
    assert_eq!(
        error.map(|e| (e.category, e.code)),
        Some((ErrorCategory::Unavailable, "model.openai_responses.response_failed"))
    );

    let (_, error) = single(Arc::new(|_| {
        vec![Step::Send(json!({"type": "error", "status": 429,
            "error": {"type": "requests", "code": "rate_limit_exceeded", "message": "slow down"}}))]
    }))
    .await;
    assert_eq!(
        error.map(|e| (e.category, e.code)),
        Some((ErrorCategory::RateLimited, "model.openai_responses.ws_error"))
    );
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

mod client {
    use super::*;
    use adk_core::{Content, Llm, LlmRequest};
    use adk_model::openai::OpenAIResponsesClient;
    use futures::TryStreamExt;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// An HTTP Responses endpoint that streams one answer.
    async fn http_answering(text: &str) -> MockServer {
        let server = MockServer::start().await;
        let body = format!("data: {}\n\ndata: {}\n\n", delta(text), completed("resp_http", text));
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
            .mount(&server)
            .await;
        server
    }

    fn client(http: &MockServer, ws: &Server, fallback: bool) -> OpenAIResponsesClient {
        let transport = WsTransport::new(&ws.config()).unwrap().with_retry_config(no_retry_delay());
        OpenAIResponsesClient::new(
            OpenAIResponsesConfig::new("sk-test", "test-model").with_base_url(http.uri()),
        )
        .unwrap()
        .with_websocket(transport)
        .with_websocket_fallback(fallback)
    }

    async fn generate(client: &OpenAIResponsesClient) -> Result<Vec<LlmResponse>, AdkError> {
        let request = LlmRequest::new("test-model", vec![Content::new("user").with_text("q1")]);
        tokio::time::timeout(Duration::from_secs(5), async {
            client.generate_content(request, true).await?.try_collect::<Vec<_>>().await
        })
        .await
        .expect("the stream must end")
    }

    async fn http_requests(http: &MockServer) -> usize {
        http.received_requests().await.unwrap().len()
    }

    #[tokio::test]
    async fn streamed_generations_go_over_the_websocket() {
        let http = http_answering("from-http").await;
        let ws = Server::start(Arc::new(|_| answer("from-ws")), accept_all()).await;
        let responses = generate(&client(&http, &ws, true)).await.unwrap();
        assert_eq!((text(&responses).as_str(), http_requests(&http).await), ("from-ws", 0));
    }

    #[tokio::test]
    async fn a_refused_handshake_falls_back_to_http_once() {
        let http = http_answering("from-http").await;
        let ws = Server::start(Arc::new(|_| answer("from-ws")), Arc::new(|_| Some(404))).await;
        let responses = generate(&client(&http, &ws, true)).await.unwrap();
        assert_eq!(
            (text(&responses).as_str(), http_requests(&http).await, ws.handshakes()),
            ("from-http", 1, 1)
        );
    }

    #[tokio::test]
    async fn without_fallback_a_refused_handshake_is_the_error() {
        let http = http_answering("from-http").await;
        let ws = Server::start(Arc::new(|_| answer("from-ws")), Arc::new(|_| Some(404))).await;
        let error = generate(&client(&http, &ws, false)).await.unwrap_err();
        assert_eq!(
            (error.category, error.code, http_requests(&http).await),
            (ErrorCategory::NotFound, "model.openai_responses.ws_handshake", 0)
        );
    }

    #[tokio::test]
    async fn a_request_the_server_received_never_falls_back() {
        let http = http_answering("from-http").await;
        let ws = Server::start(Arc::new(|_| vec![Step::Close]), accept_all()).await;
        let error = generate(&client(&http, &ws, true)).await.unwrap_err();
        assert_eq!(
            (error.code, http_requests(&http).await, ws.requests.lock().unwrap().len()),
            ("model.openai_responses.unfinished_stream", 0, 1)
        );
    }
}
