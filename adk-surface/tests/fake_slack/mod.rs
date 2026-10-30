//! A fake Slack on loopback (spec 3.5 §10 X12): the Web API keeps every call the adapter
//! makes — what left the machine — and Socket Mode is a WebSocket the test drives, as Slack
//! delivers events, presses and commands. It records every acknowledgement, can ask the
//! adapter to reconnect, and can refuse the next calls of a method.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{
    Json, Router,
    routing::{get, post},
};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

pub const APP_TOKEN: &str = "xapp-1-A0FAKE-for-tests";
pub const BOT_TOKEN: &str = "xoxb-1-fake-bot-for-tests";

/// One Web API call the adapter made.
#[derive(Debug, Clone)]
pub struct Call {
    pub method: String,
    pub body: Value,
    /// The message timestamp a `chat.postMessage` created.
    pub ts: Option<String>,
}

/// A refused call: the method, the status, the body and a `Retry-After` in seconds.
type Refusal = (String, StatusCode, Value, Option<u64>);

#[derive(Default)]
struct Shared {
    socket_url: Mutex<String>,
    calls: Mutex<Vec<Call>>,
    acks: Mutex<Vec<String>>,
    connections: AtomicUsize,
    socket: Mutex<Option<mpsc::UnboundedSender<String>>>,
    pending: Mutex<VecDeque<String>>,
    refusals: Mutex<VecDeque<Refusal>>,
    next_ts: AtomicU64,
    next_envelope: AtomicU64,
    /// `http://{addr}`, where files are served.
    base: Mutex<String>,
    /// How many files were downloaded, and with which tokens.
    downloads: Mutex<Vec<String>>,
}

/// The fake Slack, served until it is dropped.
pub struct FakeSlack {
    shared: Arc<Shared>,
    addr: SocketAddr,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Drop for FakeSlack {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl FakeSlack {
    pub async fn start() -> Self {
        let shared = Arc::new(Shared::default());
        shared.next_ts.store(1000, Ordering::SeqCst);
        let app = Router::new()
            .route("/api/{method}", post(web_api))
            .route("/files/{name}", get(file))
            .with_state(shared.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        *shared.base.lock().unwrap() = format!("http://{addr}");
        let web = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let sockets = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        *shared.socket_url.lock().unwrap() = format!("ws://{}/link", sockets.local_addr().unwrap());
        let socket_shared = shared.clone();
        let socket = tokio::spawn(async move {
            while let Ok((stream, _)) = sockets.accept().await {
                let shared = socket_shared.clone();
                tokio::spawn(serve_socket(shared, stream));
            }
        });
        Self { shared, addr, tasks: vec![web, socket] }
    }

    /// The Web API's address, for `SlackAdapterBuilder::api_base`.
    pub fn url(&self) -> String {
        format!("http://{}/api", self.addr)
    }

    pub fn calls(&self) -> Vec<Call> {
        self.shared.calls.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    pub fn calls_of(&self, method: &str) -> Vec<Call> {
        self.calls().into_iter().filter(|call| call.method == method).collect()
    }

    /// Every message call to conversation `channel`, in order.
    pub fn to_channel(&self, channel: &str) -> Vec<Call> {
        self.calls().into_iter().filter(|call| call.body["channel"] == channel).collect()
    }

    /// The texts posted to `channel`.
    pub fn texts(&self, channel: &str) -> Vec<String> {
        self.to_channel(channel)
            .into_iter()
            .filter(|call| call.method == "chat.postMessage")
            .filter_map(|call| call.body["text"].as_str().map(str::to_string))
            .collect()
    }

    /// The last decision card posted to `channel`.
    pub fn last_card(&self, channel: &str) -> Option<Call> {
        self.to_channel(channel).into_iter().rev().find(|call| {
            call.method == "chat.postMessage"
                && call.body["text"]
                    .as_str()
                    .is_some_and(|text| text.starts_with("Waiting for you"))
        })
    }

    /// The envelope ids the adapter acknowledged.
    pub fn acks(&self) -> Vec<String> {
        self.shared.acks.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// How many sockets the adapter opened.
    pub fn connections(&self) -> usize {
        self.shared.connections.load(Ordering::SeqCst)
    }

    /// Refuses the next `times` calls of `method` with `status` and `body`, and a
    /// `Retry-After` header when set.
    pub fn refuse(
        &self,
        method: &str,
        times: usize,
        status: StatusCode,
        body: Value,
        retry_after: Option<u64>,
    ) {
        let mut refusals = self.shared.refusals.lock().unwrap_or_else(PoisonError::into_inner);
        for _ in 0..times {
            refusals.push_back((method.to_string(), status, body.clone(), retry_after));
        }
    }

    /// Delivers `envelope` over the socket, now or once the adapter connects; returns its id.
    pub fn deliver(&self, mut envelope: Value) -> String {
        let id = format!("env-{}", self.shared.next_envelope.fetch_add(1, Ordering::SeqCst));
        envelope["envelope_id"] = json!(id);
        self.send(envelope.to_string());
        id
    }

    fn send(&self, frame: String) {
        let socket = self.shared.socket.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if let Some(socket) = socket
            && socket.send(frame.clone()).is_ok()
        {
            return;
        }
        self.shared.pending.lock().unwrap_or_else(PoisonError::into_inner).push_back(frame);
    }

    /// Asks the adapter for a new connection, as Slack does every few hours.
    pub fn disconnect(&self) {
        self.send(json!({ "type": "disconnect", "reason": "refresh_requested" }).to_string());
    }

    /// `user` types `text` in their direct message `channel`.
    pub fn message(user: &str, channel: &str, text: &str, ts: &str) -> Value {
        json!({
            "type": "events_api",
            "accepts_response_payload": false,
            "payload": {
                "type": "event_callback",
                "event": {
                    "type": "message", "channel_type": "im", "user": user, "channel": channel,
                    "text": text, "ts": ts, "client_msg_id": format!("c-{ts}"),
                },
            },
        })
    }

    pub fn say(&self, user: &str, channel: &str, text: &str) -> String {
        let ts = format!("{}.000100", self.shared.next_ts.fetch_add(1, Ordering::SeqCst));
        self.deliver(Self::message(user, channel, text, &ts))
    }

    /// An envelope of `user` pressing `label` on the last card in `channel`, or `None` when it
    /// has no such button.
    pub fn press_envelope(&self, user: &str, channel: &str, label: &str) -> Option<Value> {
        let card = self.last_card(channel)?;
        let actions =
            card.body["blocks"].as_array()?.iter().find(|block| block["type"] == "actions")?;
        let button = actions["elements"]
            .as_array()?
            .iter()
            .find(|button| button["text"]["text"] == label)?;
        let at = self.shared.next_ts.fetch_add(1, Ordering::SeqCst);
        Some(json!({
            "type": "interactive",
            "payload": {
                "type": "block_actions",
                "user": { "id": user, "name": "ana" },
                "channel": { "id": channel },
                "container": { "type": "message", "message_ts": card.ts, "channel_id": channel },
                "message": { "ts": card.ts, "text": card.body["text"], "blocks": card.body["blocks"] },
                "actions": [{
                    "type": "button", "action_id": button["action_id"], "block_id": "adk-card",
                    "value": button["value"], "action_ts": format!("{at}.5"),
                }],
                "trigger_id": format!("trigger-{at}"),
            },
        }))
    }

    pub fn press(&self, user: &str, channel: &str, label: &str) -> bool {
        match self.press_envelope(user, channel, label) {
            Some(envelope) => {
                self.deliver(envelope);
                true
            }
            None => false,
        }
    }

    /// `user` shares `files` (name and type) with `text` in their direct message `channel`.
    pub fn share(&self, user: &str, channel: &str, text: &str, files: &[(&str, &str)]) -> String {
        let ts = format!("{}.000300", self.shared.next_ts.fetch_add(1, Ordering::SeqCst));
        let mut envelope = Self::message(user, channel, text, &ts);
        let base = self.shared.base.lock().unwrap().clone();
        let files: Vec<Value> = files
            .iter()
            .map(|(name, mimetype)| {
                json!({ "id": format!("F{name}"), "name": name, "mimetype": mimetype, "size": PNG.len(),
                        "url_private_download": format!("{base}/files/{name}") })
            })
            .collect();
        envelope["payload"]["event"]["subtype"] = json!("file_share");
        envelope["payload"]["event"]["files"] = json!(files);
        self.deliver(envelope)
    }

    /// The `Authorization` header of each file download.
    pub fn downloads(&self) -> Vec<String> {
        self.shared.downloads.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// `/stop` typed by `user` in their direct message `channel`.
    pub fn stop_command(&self, user: &str, channel: &str) -> String {
        let at = self.shared.next_ts.fetch_add(1, Ordering::SeqCst);
        self.deliver(json!({
            "type": "slash_commands",
            "payload": {
                "command": "/stop", "text": "", "user_id": user, "channel_id": channel,
                "channel_name": "directmessage", "trigger_id": format!("trigger-{at}"),
            },
        }))
    }
}

async fn serve_socket(shared: Arc<Shared>, stream: tokio::net::TcpStream) {
    let Ok(mut socket) = tokio_tungstenite::accept_async(stream).await else { return };
    shared.connections.fetch_add(1, Ordering::SeqCst);
    let (sender, mut receiver) = mpsc::unbounded_channel::<String>();
    let pending: Vec<String> = {
        let mut pending = shared.pending.lock().unwrap_or_else(PoisonError::into_inner);
        pending.drain(..).collect()
    };
    *shared.socket.lock().unwrap_or_else(PoisonError::into_inner) = Some(sender);
    let hello = json!({ "type": "hello", "num_connections": 1 }).to_string();
    if socket.send(Message::Text(hello.into())).await.is_err() {
        return;
    }
    for frame in pending {
        let _ = socket.send(Message::Text(frame.into())).await;
    }
    loop {
        tokio::select! {
            out = receiver.recv() => match out {
                Some(frame) => {
                    if socket.send(Message::Text(frame.into())).await.is_err() {
                        return;
                    }
                }
                None => return,
            },
            frame = socket.next() => match frame {
                Some(Ok(Message::Text(text))) => {
                    if let Ok(ack) = serde_json::from_str::<Value>(text.as_str())
                        && let Some(id) = ack["envelope_id"].as_str()
                    {
                        shared.acks.lock().unwrap_or_else(PoisonError::into_inner).push(id.to_string());
                    }
                }
                Some(Ok(_)) => {}
                _ => return,
            },
        }
    }
}

async fn web_api(
    State(shared): State<Arc<Shared>>,
    Path(method): Path<String>,
    headers: HeaderMap,
    body: Option<Json<Value>>,
) -> Response {
    let auth =
        headers.get("authorization").and_then(|value| value.to_str().ok()).unwrap_or_default();
    let token = if method == "apps.connections.open" { APP_TOKEN } else { BOT_TOKEN };
    if auth != format!("Bearer {token}") {
        return Json(json!({ "ok": false, "error": "invalid_auth" })).into_response();
    }
    let body = body.map(|Json(body)| body).unwrap_or(Value::Null);
    let refusal = {
        let mut refusals = shared.refusals.lock().unwrap_or_else(PoisonError::into_inner);
        let index = refusals.iter().position(|(refused, ..)| *refused == method);
        index.and_then(|index| refusals.remove(index))
    };
    if method == "apps.connections.open" {
        if let Some((_, status, refused, _)) = refusal {
            return (status, Json(refused)).into_response();
        }
        let url = shared.socket_url.lock().unwrap().clone();
        return Json(json!({ "ok": true, "url": url })).into_response();
    }
    let ts = (method == "chat.postMessage" && refusal.is_none())
        .then(|| format!("{}.000200", shared.next_ts.fetch_add(1, Ordering::SeqCst)));
    shared.calls.lock().unwrap_or_else(PoisonError::into_inner).push(Call {
        method: method.clone(),
        body: body.clone(),
        ts: ts.clone(),
    });
    if let Some((_, status, refused, retry_after)) = refusal {
        let mut response = (status, Json(refused)).into_response();
        if let Some(seconds) = retry_after {
            response.headers_mut().insert("retry-after", seconds.to_string().parse().unwrap());
        }
        return response;
    }
    Json(json!({ "ok": true, "channel": body["channel"], "ts": ts })).into_response()
}

/// A 1×1 PNG, the photo every file is.
pub const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x64, 0x60, 0xf8, 0x5f,
    0x0f, 0x00, 0x02, 0x87, 0x01, 0x80, 0xeb, 0x47, 0xba, 0x92, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

async fn file(State(shared): State<Arc<Shared>>, headers: HeaderMap) -> Response {
    let auth =
        headers.get("authorization").and_then(|value| value.to_str().ok()).unwrap_or_default();
    shared.downloads.lock().unwrap_or_else(PoisonError::into_inner).push(auth.to_string());
    if auth != format!("Bearer {BOT_TOKEN}") {
        return StatusCode::FORBIDDEN.into_response();
    }
    ([("content-type", "image/png")], PNG.to_vec()).into_response()
}
