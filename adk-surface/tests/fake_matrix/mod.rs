//! A fake Matrix homeserver on loopback (spec 3.5 §10 X12): it keeps every event the adapter
//! sends — what left the machine — answers `/sync` long polls from what the test queues, as
//! people writing, reacting and inviting would, serves the adapter's own events back, and can
//! rate-limit the next sends.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde_json::{Map, Value, json};
use tokio::sync::Notify;

pub const TOKEN: &str = "syt_fake_access_token_for_tests";
pub const BOT: &str = "@agents:fake.example";

/// One event the adapter sent.
#[derive(Debug, Clone)]
pub struct Sent {
    pub room: String,
    pub kind: String,
    pub content: Value,
    pub event_id: String,
}

/// What the next `/sync` carries for one room.
#[derive(Debug, Clone)]
enum Queued {
    Join { room: String, timeline: Vec<Value>, state: Vec<Value>, members: Option<u64> },
    Invite { room: String, state: Vec<Value> },
}

#[derive(Default)]
struct Shared {
    sent: Mutex<Vec<Sent>>,
    joins: Mutex<Vec<String>>,
    queue: Mutex<VecDeque<Queued>>,
    arrived: Notify,
    batch: AtomicU64,
    next_event: AtomicU64,
    waiting_syncs: AtomicUsize,
    refusals: Mutex<VecDeque<(StatusCode, Value)>>,
    /// The homeserver predates authenticated media.
    legacy_only: std::sync::atomic::AtomicBool,
    /// Each media download: the endpoint, `v1` or `legacy`.
    downloads: Mutex<Vec<String>>,
}

/// The fake homeserver, served until it is dropped.
pub struct FakeMatrix {
    shared: Arc<Shared>,
    addr: SocketAddr,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for FakeMatrix {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl FakeMatrix {
    pub async fn start() -> Self {
        let shared = Arc::new(Shared::default());
        let app = Router::new()
            .route("/_matrix/client/v3/account/whoami", get(whoami))
            .route("/_matrix/client/v3/sync", get(sync))
            .route("/_matrix/client/v3/rooms/{room}/send/{kind}/{txn}", put(send))
            .route("/_matrix/client/v3/rooms/{room}/event/{event}", get(event))
            .route("/_matrix/client/v3/join/{room}", post(join))
            .route("/_matrix/client/v1/media/download/{server}/{id}", get(media_v1))
            .route("/_matrix/media/v3/download/{server}/{id}", get(media_legacy))
            .with_state(shared.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self { shared, addr, server }
    }

    /// The homeserver's address, for `MatrixAdapter::builder`.
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn sent(&self) -> Vec<Sent> {
        self.shared.sent.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// The events sent to `room`, in order.
    pub fn sent_to(&self, room: &str) -> Vec<Sent> {
        self.sent().into_iter().filter(|sent| sent.room == room).collect()
    }

    /// The bodies of the messages sent to `room`.
    pub fn bodies(&self, room: &str) -> Vec<String> {
        self.sent_to(room)
            .into_iter()
            .filter(|sent| sent.kind == "m.room.message")
            .filter_map(|sent| sent.content["body"].as_str().map(str::to_string))
            .collect()
    }

    /// The last card sent to `room`.
    pub fn last_card(&self, room: &str) -> Option<Sent> {
        self.sent_to(room).into_iter().rev().find(|sent| {
            sent.kind == "m.room.message"
                && sent.content["body"]
                    .as_str()
                    .is_some_and(|body| body.starts_with("Waiting for you"))
        })
    }

    pub fn joins(&self) -> Vec<String> {
        self.shared.joins.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Whether a `/sync` with a `since` token has started: the adapter reads new events.
    pub fn reading(&self) -> bool {
        self.syncs() > 0
    }

    /// How many `/sync` calls with a `since` token have started.
    pub fn syncs(&self) -> usize {
        self.shared.waiting_syncs.load(Ordering::SeqCst)
    }

    /// Rate-limits the next `times` sends.
    pub fn limit_sends(&self, times: usize, retry_after_ms: u64) {
        let mut refusals = self.shared.refusals.lock().unwrap_or_else(PoisonError::into_inner);
        for _ in 0..times {
            refusals.push_back((
                StatusCode::TOO_MANY_REQUESTS,
                json!({ "errcode": "M_LIMIT_EXCEEDED", "error": "Too many requests", "retry_after_ms": retry_after_ms }),
            ));
        }
    }

    fn queue(&self, queued: Queued) {
        self.shared.queue.lock().unwrap_or_else(PoisonError::into_inner).push_back(queued);
        self.shared.arrived.notify_waiters();
    }

    fn event_id(&self) -> String {
        format!("$in{}", self.shared.next_event.fetch_add(1, Ordering::SeqCst))
    }

    /// `sender`'s text message in `room`, not yet delivered.
    pub fn text_event(&self, sender: &str, body: &str) -> Value {
        json!({
            "type": "m.room.message", "sender": sender, "event_id": self.event_id(),
            "origin_server_ts": 1_761_300_000_000u64, "content": { "msgtype": "m.text", "body": body },
        })
    }

    /// Makes the homeserver one that predates authenticated media.
    pub fn legacy_media_only(&self) {
        self.shared.legacy_only.store(true, Ordering::SeqCst);
    }

    /// Each media download: `v1` or `legacy`.
    pub fn downloads(&self) -> Vec<String> {
        self.shared.downloads.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// `sender`'s image `name` of `mimetype` at `mxc`, captioned `caption`, not yet delivered.
    pub fn image_event(
        &self,
        sender: &str,
        mxc: &str,
        mimetype: &str,
        caption: Option<&str>,
    ) -> Value {
        let mut content = json!({ "msgtype": "m.image", "body": "rye.png", "url": mxc,
                                  "info": { "mimetype": mimetype, "size": PNG.len() } });
        if let Some(caption) = caption {
            content["body"] = json!(caption);
            content["filename"] = json!("rye.png");
        }
        json!({ "type": "m.room.message", "sender": sender, "event_id": self.event_id(),
                "origin_server_ts": 1_761_300_000_000u64, "content": content })
    }

    /// `sender`'s reaction with `key` on `event`, not yet delivered.
    pub fn reaction_event(&self, sender: &str, event: &str, key: &str) -> Value {
        json!({
            "type": "m.reaction", "sender": sender, "event_id": self.event_id(),
            "origin_server_ts": 1_761_300_000_000u64,
            "content": { "m.relates_to": { "rel_type": "m.annotation", "event_id": event, "key": key } },
        })
    }

    /// The reaction key the last card in `room` offers for the answer labelled `label`.
    pub fn key_for(&self, room: &str, label: &str) -> Option<(String, String)> {
        let card = self.last_card(room)?;
        let answer = match label {
            "Allow once" => "allowOnce",
            _ => "deny",
        };
        let answers = card.content["rs.adk.card"]["answers"].as_object()?;
        let key = answers.iter().find(|(_, value)| *value == answer)?.0.clone();
        Some((card.event_id, key))
    }

    /// Delivers `events` in `room`'s timeline through the next `/sync`.
    pub fn deliver(&self, room: &str, events: Vec<Value>) {
        self.queue(Queued::Join {
            room: room.to_string(),
            timeline: events,
            state: Vec::new(),
            members: None,
        });
    }

    /// Delivers `events` with `state` and a member count.
    pub fn deliver_with(
        &self,
        room: &str,
        events: Vec<Value>,
        state: Vec<Value>,
        members: Option<u64>,
    ) {
        self.queue(Queued::Join { room: room.to_string(), timeline: events, state, members });
    }

    pub fn say(&self, sender: &str, room: &str, text: &str) {
        let event = self.text_event(sender, text);
        self.deliver(room, vec![event]);
    }

    /// `sender` reacts on the last card in `room` with the key for `label`; `false` when the
    /// card offers no such answer.
    pub fn react(&self, sender: &str, room: &str, label: &str) -> bool {
        let Some((card, key)) = self.key_for(room, label) else { return false };
        let event = self.reaction_event(sender, &card, &key);
        self.deliver(room, vec![event]);
        true
    }

    /// An invite of the adapter's account to `room` from `sender`.
    pub fn invite(&self, room: &str, sender: &str, direct: bool, encrypted: bool) {
        let mut state = vec![json!({
            "type": "m.room.member", "state_key": BOT, "sender": sender,
            "content": { "membership": "invite", "is_direct": direct },
        })];
        if encrypted {
            state.push(json!({ "type": "m.room.encryption", "state_key": "", "sender": sender, "content": { "algorithm": "m.megolm.v1.aes-sha2" } }));
        }
        self.queue(Queued::Invite { room: room.to_string(), state });
    }
}

fn authorized(headers: &HeaderMap) -> bool {
    headers.get("authorization").and_then(|value| value.to_str().ok())
        == Some(&format!("Bearer {TOKEN}"))
}

fn unknown_token() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "errcode": "M_UNKNOWN_TOKEN", "error": "Invalid access token" })),
    )
        .into_response()
}

async fn whoami(headers: HeaderMap) -> Response {
    if !authorized(&headers) {
        return unknown_token();
    }
    Json(json!({ "user_id": BOT, "device_id": "FAKE" })).into_response()
}

async fn sync(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    Query(query): Query<Map<String, Value>>,
) -> Response {
    if !authorized(&headers) {
        return unknown_token();
    }
    let since = query.get("since").is_some();
    let wait =
        query.get("timeout").and_then(Value::as_str).and_then(|t| t.parse().ok()).unwrap_or(0);
    let deadline = tokio::time::Instant::now() + Duration::from_millis(wait);
    if since {
        shared.waiting_syncs.fetch_add(1, Ordering::SeqCst);
    }
    loop {
        let arrived = shared.arrived.notified();
        let ready: Vec<Queued> =
            shared.queue.lock().unwrap_or_else(PoisonError::into_inner).drain(..).collect();
        if !ready.is_empty() || !since || tokio::time::Instant::now() >= deadline {
            let (mut joined, mut invited) = (Map::new(), Map::new());
            for queued in ready {
                match queued {
                    Queued::Join { room, timeline, state, members } => {
                        let entry = joined.entry(room).or_insert_with(
                            || json!({ "timeline": { "events": [] }, "state": { "events": [] } }),
                        );
                        entry["timeline"]["events"].as_array_mut().unwrap().extend(timeline);
                        entry["state"]["events"].as_array_mut().unwrap().extend(state);
                        if let Some(members) = members {
                            entry["summary"] = json!({ "m.joined_member_count": members });
                        }
                    }
                    Queued::Invite { room, state } => {
                        invited.insert(room, json!({ "invite_state": { "events": state } }));
                    }
                }
            }
            let batch = shared.batch.fetch_add(1, Ordering::SeqCst) + 1;
            return Json(json!({ "next_batch": format!("s{batch}"), "rooms": { "join": joined, "invite": invited } })).into_response();
        }
        let _ = tokio::time::timeout_at(deadline, arrived).await;
    }
}

async fn send(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    Path((room, kind, _txn)): Path<(String, String, String)>,
    Json(content): Json<Value>,
) -> Response {
    if !authorized(&headers) {
        return unknown_token();
    }
    let refusal = shared.refusals.lock().unwrap_or_else(PoisonError::into_inner).pop_front();
    let event_id = format!("$out{}", shared.next_event.fetch_add(1, Ordering::SeqCst));
    shared.sent.lock().unwrap_or_else(PoisonError::into_inner).push(Sent {
        room,
        kind,
        content,
        event_id: if refusal.is_some() { String::new() } else { event_id.clone() },
    });
    if let Some((status, body)) = refusal {
        return (status, Json(body)).into_response();
    }
    Json(json!({ "event_id": event_id })).into_response()
}

async fn event(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    Path((room, id)): Path<(String, String)>,
) -> Response {
    if !authorized(&headers) {
        return unknown_token();
    }
    let sent = shared
        .sent
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .find(|sent| sent.room == room && sent.event_id == id)
        .cloned();
    match sent {
        Some(sent) => Json(json!({ "type": sent.kind, "sender": BOT, "event_id": sent.event_id, "room_id": room, "content": sent.content })).into_response(),
        None => (StatusCode::NOT_FOUND, Json(json!({ "errcode": "M_NOT_FOUND", "error": "Event not found" }))).into_response(),
    }
}

async fn join(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    Path(room): Path<String>,
) -> Response {
    if !authorized(&headers) {
        return unknown_token();
    }
    shared.joins.lock().unwrap_or_else(PoisonError::into_inner).push(room.clone());
    Json(json!({ "room_id": room })).into_response()
}

/// A 1×1 PNG, the photo every file is.
pub const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x64, 0x60, 0xf8, 0x5f,
    0x0f, 0x00, 0x02, 0x87, 0x01, 0x80, 0xeb, 0x47, 0xba, 0x92, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

async fn media_v1(State(shared): State<Arc<Shared>>, headers: HeaderMap) -> Response {
    if !authorized(&headers) {
        return unknown_token();
    }
    if shared.legacy_only.load(Ordering::SeqCst) {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "errcode": "M_UNRECOGNIZED", "error": "Unrecognized request" })),
        )
            .into_response();
    }
    shared.downloads.lock().unwrap_or_else(PoisonError::into_inner).push("v1".into());
    ([("content-type", "image/png")], PNG.to_vec()).into_response()
}

async fn media_legacy(State(shared): State<Arc<Shared>>) -> Response {
    shared.downloads.lock().unwrap_or_else(PoisonError::into_inner).push("legacy".into());
    ([("content-type", "image/png")], PNG.to_vec()).into_response()
}
