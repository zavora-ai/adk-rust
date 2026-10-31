//! A fake Discord on loopback (spec 3.5 §10 X12): the REST API keeps every call the adapter
//! makes — what left the machine — and the Gateway is a WebSocket the test drives: it says
//! hello, answers identify with `READY` and resume with `RESUMED`, acknowledges heartbeats,
//! dispatches events with sequence numbers, and can ask for a reconnect, invalidate the
//! session or stop acknowledging heartbeats.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

pub const TOKEN: &str = "MTA-fake-bot-token.for-tests";
pub const APPLICATION: &str = "app-1";

/// One REST call the adapter made.
#[derive(Debug, Clone)]
pub struct Call {
    pub method: String,
    pub route: String,
    pub body: Value,
    /// The id of the message a create made.
    pub id: Option<String>,
}

/// What the adapter sent on the Gateway.
#[derive(Debug, Clone)]
pub struct Sent {
    pub connection: usize,
    pub payload: Value,
}

#[derive(Default)]
struct Shared {
    gateway_url: Mutex<String>,
    calls: Mutex<Vec<Call>>,
    sent: Mutex<Vec<Sent>>,
    connections: AtomicUsize,
    socket: Mutex<Option<mpsc::UnboundedSender<Message>>>,
    pending: Mutex<VecDeque<Message>>,
    seq: AtomicU64,
    next_id: AtomicU64,
    /// `http://{addr}`, where attachments are served.
    base: Mutex<String>,
    /// The `Authorization` header of each attachment download, empty for none.
    downloads: Mutex<Vec<String>>,
    heartbeat_ms: AtomicU64,
    silent: AtomicBool,
    refusals: Mutex<VecDeque<(String, StatusCode, Value)>>,
}

/// The fake Discord, served until it is dropped.
pub struct FakeDiscord {
    shared: Arc<Shared>,
    addr: SocketAddr,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Drop for FakeDiscord {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl FakeDiscord {
    pub async fn start() -> Self {
        let shared = Arc::new(Shared::default());
        shared.next_id.store(9000, Ordering::SeqCst);
        shared.heartbeat_ms.store(45_000, Ordering::SeqCst);
        let app = Router::new().fallback(rest).with_state(shared.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        *shared.base.lock().unwrap() = format!("http://{addr}");
        let web = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let gateways = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        *shared.gateway_url.lock().unwrap() = format!("ws://{}", gateways.local_addr().unwrap());
        let gateway_shared = shared.clone();
        let gateway = tokio::spawn(async move {
            while let Ok((stream, _)) = gateways.accept().await {
                tokio::spawn(serve_gateway(gateway_shared.clone(), stream));
            }
        });
        Self { shared, addr, tasks: vec![web, gateway] }
    }

    /// The REST API's address, for `DiscordAdapterBuilder::api_base`.
    pub fn url(&self) -> String {
        format!("http://{}/api/v10", self.addr)
    }

    /// How often the Gateway asks for a heartbeat, from the next connection on.
    pub fn heartbeat_every(&self, millis: u64) {
        self.shared.heartbeat_ms.store(millis, Ordering::SeqCst);
    }

    /// Stops acknowledging heartbeats, as a Gateway that went away does.
    pub fn go_silent(&self, silent: bool) {
        self.shared.silent.store(silent, Ordering::SeqCst);
    }

    pub fn calls(&self) -> Vec<Call> {
        self.shared.calls.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    pub fn calls_to(&self, method: &str, prefix: &str) -> Vec<Call> {
        self.calls()
            .into_iter()
            .filter(|call| call.method == method && call.route.starts_with(prefix))
            .collect()
    }

    /// The messages created in `channel`, in order.
    pub fn messages(&self, channel: &str) -> Vec<Call> {
        self.calls_to("POST", &format!("/channels/{channel}/messages"))
    }

    pub fn contents(&self, channel: &str) -> Vec<String> {
        self.messages(channel)
            .into_iter()
            .filter_map(|call| call.body["content"].as_str().map(str::to_string))
            .collect()
    }

    /// The last decision card in `channel`.
    pub fn last_card(&self, channel: &str) -> Option<Call> {
        self.messages(channel).into_iter().rev().find(|call| {
            call.body["content"].as_str().is_some_and(|text| text.starts_with("Waiting for you"))
        })
    }

    /// What the adapter sent on the Gateway.
    pub fn sent(&self) -> Vec<Sent> {
        self.shared.sent.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// The adapter's payloads with `op`.
    pub fn sent_op(&self, op: u64) -> Vec<Sent> {
        self.sent().into_iter().filter(|sent| sent.payload["op"] == op).collect()
    }

    pub fn connections(&self) -> usize {
        self.shared.connections.load(Ordering::SeqCst)
    }

    /// The last sequence number dispatched.
    pub fn seq(&self) -> u64 {
        self.shared.seq.load(Ordering::SeqCst)
    }

    /// Refuses the next `times` calls whose `METHOD route` starts with `prefix`.
    pub fn refuse(&self, prefix: &str, times: usize, status: StatusCode, body: Value) {
        let mut refusals = self.shared.refusals.lock().unwrap_or_else(PoisonError::into_inner);
        for _ in 0..times {
            refusals.push_back((prefix.to_string(), status, body.clone()));
        }
    }

    fn send(&self, message: Message) {
        let socket = self.shared.socket.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if let Some(socket) = socket
            && socket.send(message.clone()).is_ok()
        {
            return;
        }
        self.shared.pending.lock().unwrap_or_else(PoisonError::into_inner).push_back(message);
    }

    /// Dispatches event `t` with `d`.
    pub fn dispatch(&self, t: &str, d: Value) {
        let s = self.shared.seq.fetch_add(1, Ordering::SeqCst) + 1;
        self.send(Message::Text(json!({ "op": 0, "s": s, "t": t, "d": d }).to_string().into()));
    }

    /// Asks the adapter to reconnect (op 7).
    pub fn ask_reconnect(&self) {
        self.send(Message::Text(json!({ "op": 7, "d": null }).to_string().into()));
    }

    /// Invalidates the session (op 9), resumable or not.
    pub fn invalidate(&self, resumable: bool) {
        self.send(Message::Text(json!({ "op": 9, "d": resumable }).to_string().into()));
    }

    /// Closes the connection with `code`.
    pub fn close(&self, code: u16) {
        use tokio_tungstenite::tungstenite::protocol::CloseFrame;
        self.send(Message::Close(Some(CloseFrame {
            code: code.into(),
            reason: "closed by the test".into(),
        })));
    }

    fn next_id(&self) -> String {
        self.shared.next_id.fetch_add(1, Ordering::SeqCst).to_string()
    }

    /// `MESSAGE_CREATE` of `user` typing `text` in their direct message `channel`.
    pub fn message_event(&self, user: &str, channel: &str, text: &str) -> Value {
        json!({
            "id": self.next_id(), "channel_id": channel, "type": 0, "content": text,
            "author": { "id": user, "username": "ana", "global_name": "Ana" },
        })
    }

    /// `user` sends `files` (name and content type) with `text` in their direct message.
    pub fn send_files(&self, user: &str, channel: &str, text: &str, files: &[(&str, &str)]) {
        let mut d = self.message_event(user, channel, text);
        let base = self.shared.base.lock().unwrap().clone();
        let attachments: Vec<Value> = files
            .iter()
            .map(|(name, content_type)| {
                json!({ "id": self.next_id(), "filename": name, "content_type": content_type,
                        "size": PNG.len(), "url": format!("{base}/attachments/{name}") })
            })
            .collect();
        d["attachments"] = json!(attachments);
        self.dispatch("MESSAGE_CREATE", d);
    }

    /// The `Authorization` header of each attachment download, empty for none.
    pub fn downloads(&self) -> Vec<String> {
        self.shared.downloads.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    pub fn say(&self, user: &str, channel: &str, text: &str) {
        let d = self.message_event(user, channel, text);
        self.dispatch("MESSAGE_CREATE", d);
    }

    /// `INTERACTION_CREATE` of `user` pressing `label` on the last card in `channel`, or
    /// `None` when it has no such button.
    pub fn press_event(&self, user: &str, channel: &str, label: &str) -> Option<Value> {
        let card = self.last_card(channel)?;
        let row = card.body["components"].as_array()?.first()?;
        let button =
            row["components"].as_array()?.iter().find(|button| button["label"] == label)?;
        let custom_id = button.get("custom_id")?.clone();
        Some(json!({
            "id": self.next_id(), "application_id": APPLICATION, "type": 3,
            "token": format!("interaction-token-{}", self.next_id()), "channel_id": channel,
            "user": { "id": user, "username": "ana" },
            "message": { "id": card.id, "channel_id": channel, "content": card.body["content"], "components": card.body["components"] },
            "data": { "custom_id": custom_id, "component_type": 2 },
        }))
    }

    pub fn press(&self, user: &str, channel: &str, label: &str) -> bool {
        match self.press_event(user, channel, label) {
            Some(d) => {
                self.dispatch("INTERACTION_CREATE", d);
                true
            }
            None => false,
        }
    }

    /// `/stop` by `user` in their direct message `channel`; returns the interaction's token.
    pub fn stop_command(&self, user: &str, channel: &str) -> String {
        let token = format!("interaction-token-{}", self.next_id());
        let d = json!({
            "id": self.next_id(), "application_id": APPLICATION, "type": 2, "token": token,
            "channel_id": channel, "user": { "id": user, "username": "ana" },
            "data": { "id": "cmd-1", "name": "stop", "type": 1 },
        });
        self.dispatch("INTERACTION_CREATE", d);
        token
    }
}

async fn serve_gateway(shared: Arc<Shared>, stream: tokio::net::TcpStream) {
    let Ok(mut socket) = tokio_tungstenite::accept_async(stream).await else { return };
    let connection = shared.connections.fetch_add(1, Ordering::SeqCst) + 1;
    let (sender, mut receiver) = mpsc::unbounded_channel::<Message>();
    *shared.socket.lock().unwrap_or_else(PoisonError::into_inner) = Some(sender);
    let interval = shared.heartbeat_ms.load(Ordering::SeqCst);
    let hello = json!({ "op": 10, "d": { "heartbeat_interval": interval } }).to_string();
    if socket.send(Message::Text(hello.into())).await.is_err() {
        return;
    }
    loop {
        tokio::select! {
            out = receiver.recv() => match out {
                Some(message) => {
                    let closing = matches!(message, Message::Close(_));
                    if socket.send(message).await.is_err() || closing {
                        return;
                    }
                }
                None => return,
            },
            frame = socket.next() => {
                let Some(Ok(Message::Text(text))) = frame else {
                    if matches!(frame, Some(Ok(_))) { continue; }
                    return;
                };
                let Ok(payload) = serde_json::from_str::<Value>(text.as_str()) else { continue };
                shared.sent.lock().unwrap_or_else(PoisonError::into_inner).push(Sent { connection, payload: payload.clone() });
                let reply = match payload["op"].as_u64() {
                    Some(1) if !shared.silent.load(Ordering::SeqCst) => Some(json!({ "op": 11 })),
                    Some(2) => {
                        let s = shared.seq.fetch_add(1, Ordering::SeqCst) + 1;
                        let url = shared.gateway_url.lock().unwrap().clone();
                        Some(json!({ "op": 0, "s": s, "t": "READY", "d": {
                            "v": 10, "session_id": format!("session-{connection}"),
                            "resume_gateway_url": url,
                            "user": { "id": "bot-1", "username": "agents", "bot": true },
                            "application": { "id": APPLICATION },
                        }}))
                    }
                    Some(6) => {
                        let s = shared.seq.fetch_add(1, Ordering::SeqCst) + 1;
                        Some(json!({ "op": 0, "s": s, "t": "RESUMED", "d": null }))
                    }
                    _ => None,
                };
                if let Some(reply) = reply
                    && socket.send(Message::Text(reply.to_string().into())).await.is_err()
                {
                    return;
                }
                // What the test dispatched while no connection was ready.
                if matches!(payload["op"].as_u64(), Some(2 | 6)) {
                    let pending: Vec<Message> = shared.pending.lock().unwrap_or_else(PoisonError::into_inner).drain(..).collect();
                    for message in pending {
                        if socket.send(message).await.is_err() {
                            return;
                        }
                    }
                }
            }
        }
    }
}

async fn rest(
    State(shared): State<Arc<Shared>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Option<Json<Value>>,
) -> Response {
    // Discord's CDN serves attachments to anyone who has the address.
    if method == Method::GET && uri.path().starts_with("/attachments/") {
        let auth = headers.get("authorization").and_then(|value| value.to_str().ok());
        shared
            .downloads
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(auth.unwrap_or_default().to_string());
        return ([("content-type", "image/png")], PNG.to_vec()).into_response();
    }
    let route = uri.path().trim_start_matches("/api/v10").to_string();
    let auth =
        headers.get("authorization").and_then(|value| value.to_str().ok()).unwrap_or_default();
    if auth != format!("Bot {TOKEN}") {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "message": "401: Unauthorized", "code": 0 })),
        )
            .into_response();
    }
    if route == "/gateway/bot" {
        let url = shared.gateway_url.lock().unwrap().clone();
        return Json(json!({ "url": url, "shards": 1 })).into_response();
    }
    let body = body.map(|Json(body)| body).unwrap_or(Value::Null);
    let key = format!("{method} {route}");
    let refusal = {
        let mut refusals = shared.refusals.lock().unwrap_or_else(PoisonError::into_inner);
        let index = refusals.iter().position(|(prefix, ..)| key.starts_with(prefix.as_str()));
        index.and_then(|index| refusals.remove(index))
    };
    let creates = method == Method::POST && route.ends_with("/messages") && refusal.is_none();
    let id = creates.then(|| shared.next_id.fetch_add(1, Ordering::SeqCst).to_string());
    shared.calls.lock().unwrap_or_else(PoisonError::into_inner).push(Call {
        method: method.to_string(),
        route: route.clone(),
        body: body.clone(),
        id: id.clone(),
    });
    if let Some((_, status, refused)) = refusal {
        return (status, Json(refused)).into_response();
    }
    match id {
        Some(id) => Json(json!({ "id": id, "content": body["content"] })).into_response(),
        None if method == Method::DELETE || route.ends_with("/callback") => {
            StatusCode::NO_CONTENT.into_response()
        }
        None => Json(json!({})).into_response(),
    }
}

/// A 1×1 PNG, the photo every file is.
pub const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x64, 0x60, 0xf8, 0x5f,
    0x0f, 0x00, 0x02, 0x87, 0x01, 0x80, 0xeb, 0x47, 0xba, 0x92, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];
