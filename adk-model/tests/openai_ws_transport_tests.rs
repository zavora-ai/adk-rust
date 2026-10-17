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
                let refusal = plan(count.fetch_add(1, Ordering::SeqCst));
                let (script, log) = (script.clone(), log.clone());
                tokio::spawn(async move {
                    match refusal {
                        Some(status) => refuse(socket, status).await,
                        None => serve(socket, script, log).await,
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

async fn serve(socket: TcpStream, script: Script, log: Arc<Mutex<Vec<Value>>>) {
    let Ok(mut ws) = tokio_tungstenite::accept_async(socket).await else { return };
    while let Some(Ok(message)) = ws.next().await {
        let Message::Text(text) = message else { continue };
        let request: Value = serde_json::from_str(text.as_str()).unwrap();
        log.lock().unwrap().push(request.clone());
        for step in script(&request) {
            let sent = match step {
                Step::Send(event) => ws.send(Message::Text(event.to_string().into())).await,
                Step::Pause(millis) => {
                    tokio::time::sleep(Duration::from_millis(millis)).await;
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
