//! A scripted OpenAI Responses WebSocket upstream on 127.0.0.1.
//!
//! Each connection answers its `response.create` messages one at a time, in order, as the
//! default lane of the real endpoint does: frames of a response still in flight are sent
//! before the next response starts. A handshake plan can refuse chosen handshakes with an
//! HTTP status before the upgrade.

use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;

/// One step of a scripted reply.
pub enum Step {
    Send(Value),
    Pause(u64),
    /// Send a Close frame and end the connection.
    Close,
}

/// The reply to one request: the connection index (0-based) and the request JSON.
pub type Script = Arc<dyn Fn(usize, &Value) -> Vec<Step> + Send + Sync>;

/// Refuses the n-th handshake (0-based) with the returned HTTP status.
pub type Handshakes = Arc<dyn Fn(usize) -> Option<u16> + Send + Sync>;

pub fn accept_all() -> Handshakes {
    Arc::new(|_| None)
}

pub struct WsUpstream {
    /// HTTP base URL; `WsTransport` derives `ws://…/v1/responses` from it.
    pub base: String,
    handshakes: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<Value>>>,
}

impl WsUpstream {
    pub async fn start(script: Script, plan: Handshakes) -> anyhow::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}/v1", listener.local_addr()?);
        let handshakes = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (count, log) = (handshakes.clone(), requests.clone());
        tokio::spawn(async move {
            let accepted = Arc::new(AtomicUsize::new(0));
            while let Ok((socket, _)) = listener.accept().await {
                let attempt = count.fetch_add(1, Ordering::SeqCst);
                let (script, log, accepted) = (script.clone(), log.clone(), accepted.clone());
                let refusal = plan(attempt);
                tokio::spawn(async move {
                    let _ = match refusal {
                        Some(status) => refuse(socket, status).await,
                        None => serve(socket, script, log, accepted).await,
                    };
                });
            }
        });
        Ok(Self { base, handshakes, requests })
    }

    /// Handshakes attempted, refused or accepted.
    pub fn handshakes(&self) -> usize {
        self.handshakes.load(Ordering::SeqCst)
    }

    /// Every request message received, in arrival order.
    pub fn requests(&self) -> Vec<Value> {
        self.requests.lock().map(|requests| requests.clone()).unwrap_or_default()
    }
}

async fn refuse(mut socket: TcpStream, status: u16) -> anyhow::Result<()> {
    let mut buffer = [0_u8; 4096];
    let mut received = Vec::new();
    while !received.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = socket.read(&mut buffer).await?;
        anyhow::ensure!(read > 0, "client closed during the handshake");
        received.extend_from_slice(&buffer[..read]);
    }
    let reply = format!(
        "HTTP/1.1 {status} Refused\r\ncontent-type: application/json\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{{}}"
    );
    socket.write_all(reply.as_bytes()).await?;
    socket.shutdown().await?;
    Ok(())
}

async fn serve(
    socket: TcpStream,
    script: Script,
    log: Arc<Mutex<Vec<Value>>>,
    accepted: Arc<AtomicUsize>,
) -> anyhow::Result<()> {
    let mut ws = tokio_tungstenite::accept_async(socket).await?;
    let connection = accepted.fetch_add(1, Ordering::SeqCst);
    while let Some(message) = ws.next().await {
        let Message::Text(text) = message? else { continue };
        let request: Value = serde_json::from_str(&text)?;
        if let Ok(mut log) = log.lock() {
            log.push(request.clone());
        }
        for step in script(connection, &request) {
            match step {
                Step::Send(event) => ws.send(Message::Text(event.to_string().into())).await?,
                Step::Pause(millis) => tokio::time::sleep(Duration::from_millis(millis)).await,
                Step::Close => {
                    ws.close(None).await?;
                    return Ok(());
                }
            }
        }
    }
    Ok(())
}

/// The user text of a scripted request, from either a string `input` or input items.
pub fn input_text(request: &Value) -> String {
    match &request["input"] {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

pub fn delta(text: &str) -> Value {
    json!({
        "type": "response.output_text.delta", "sequence_number": 2, "item_id": "msg_1",
        "output_index": 0, "content_index": 0, "delta": text, "logprobs": []
    })
}

pub fn text_done(text: &str) -> Value {
    json!({
        "type": "response.output_text.done", "sequence_number": 3, "item_id": "msg_1",
        "output_index": 0, "content_index": 0, "text": text, "logprobs": []
    })
}

fn message(text: &str, status: &str) -> Value {
    json!({
        "type": "message", "id": "msg_1", "role": "assistant", "status": status,
        "content": [{"type": "output_text", "text": text, "annotations": []}]
    })
}

fn response(id: &str, status: &str, output: Value) -> Value {
    json!({
        "id": id, "object": "response", "created_at": 0, "model": "scripted",
        "status": status, "output": output,
        "usage": {
            "input_tokens": 8, "input_tokens_details": {"cached_tokens": 0},
            "output_tokens": 4, "output_tokens_details": {"reasoning_tokens": 0},
            "total_tokens": 12
        }
    })
}

pub fn created(id: &str) -> Value {
    json!({"type": "response.created", "sequence_number": 0,
        "response": response(id, "in_progress", json!([]))})
}

pub fn completed(id: &str, text: &str) -> Value {
    json!({"type": "response.completed", "sequence_number": 9,
        "response": response(id, "completed", json!([message(text, "completed")]))})
}

/// A whole answer: created, deltas, the done event, and the completed response.
pub fn answer(id: &str, pieces: &[&str]) -> Vec<Step> {
    let text: String = pieces.concat();
    let mut steps = vec![Step::Send(created(id))];
    steps.extend(pieces.iter().map(|piece| Step::Send(delta(piece))));
    steps.push(Step::Send(text_done(&text)));
    steps.push(Step::Send(completed(id, &text)));
    steps
}

pub fn incomplete(id: &str, text: &str, reason: &str) -> Value {
    let mut body = response(id, "incomplete", json!([message(text, "incomplete")]));
    body["incomplete_details"] = json!({"reason": reason});
    json!({"type": "response.incomplete", "sequence_number": 9, "response": body})
}

pub fn failed(id: &str) -> Value {
    let mut body = response(id, "failed", json!([]));
    body["error"] = json!({"code": "server_error", "message": "The server had an error."});
    json!({"type": "response.failed", "sequence_number": 9, "response": body})
}

pub fn rate_limited() -> Value {
    json!({
        "type": "error", "status": 429,
        "error": {"type": "requests", "code": "rate_limit_exceeded",
            "message": "Rate limit reached for requests."}
    })
}

/// A completed event whose output does not match the Responses schema.
pub fn malformed_completed(id: &str) -> Value {
    let mut body = response(id, "completed", json!([]));
    body["output"] =
        json!([{"type": "message", "id": "msg_1", "role": "assistant", "content": "not-a-list"}]);
    json!({"type": "response.completed", "sequence_number": 9, "response": body})
}
