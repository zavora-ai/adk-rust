//! A fake Telegram Bot API on loopback (spec 3.3 §5.3), from the one `adk-surface`'s chat tests
//! run against: it keeps every call the kernel's Telegram adapter makes — what would leave the
//! machine — with the time it arrived, and serves `getUpdates` from a queue the scenario fills,
//! as a person typing and pressing buttons would.
//!
//! It lives in the scenario's process, so it outlives the kernel's restarts the way Telegram's
//! servers do.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, routing::post};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use tokio::sync::Notify;

/// The fake bot's token; any well-formed token works against the fake.
pub const TOKEN: &str = "424242:AAFakeBotToken_for-scenarios";

/// One call the adapter made.
#[derive(Debug, Clone)]
pub struct Call {
    /// The Bot API method.
    pub method: String,
    /// The JSON body.
    pub body: Value,
    /// The id of the message a `sendMessage` created.
    pub message_id: Option<i64>,
    /// When it arrived.
    pub at: DateTime<Utc>,
}

impl Call {
    /// The text it sends, if any.
    pub fn text(&self) -> &str {
        self.body["text"].as_str().unwrap_or_default()
    }

    /// Whether it puts something new in front of the person: a message or a photo, not an
    /// edit, an acknowledgement or a read.
    pub fn notifies(&self) -> bool {
        matches!(self.method.as_str(), "sendMessage" | "sendPhoto" | "sendDocument")
    }
}

struct Shared {
    token: String,
    calls: Mutex<Vec<Call>>,
    updates: Mutex<VecDeque<Value>>,
    arrived: Notify,
    next_update: AtomicI64,
    next_message: AtomicI64,
    next_callback: AtomicI64,
}

/// The fake API, served until it is dropped.
pub struct FakeTelegram {
    shared: Arc<Shared>,
    addr: SocketAddr,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for FakeTelegram {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl FakeTelegram {
    /// A fake API for the bot with [`TOKEN`].
    pub async fn start() -> anyhow::Result<Self> {
        let shared = Arc::new(Shared {
            token: TOKEN.to_string(),
            calls: Mutex::default(),
            updates: Mutex::default(),
            arrived: Notify::new(),
            next_update: AtomicI64::new(1),
            next_message: AtomicI64::new(100),
            next_callback: AtomicI64::new(1),
        });
        let app = Router::new().route("/{bot}/{method}", post(call)).with_state(shared.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(Self { shared, addr, server })
    }

    /// The API's address, for `TELEGRAM_API_BASE`.
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Every call so far.
    pub fn calls(&self) -> Vec<Call> {
        self.shared.calls.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// The calls addressed to `chat`, in order.
    pub fn to_chat(&self, chat: i64) -> Vec<Call> {
        self.calls()
            .into_iter()
            .filter(|call| call.body.get("chat_id") == Some(&json!(chat)))
            .collect()
    }

    /// The messages sent to `chat`.
    pub fn messages(&self, chat: i64) -> Vec<Call> {
        self.to_chat(chat).into_iter().filter(|call| call.method == "sendMessage").collect()
    }

    /// The decision cards sent to `chat`: messages with buttons.
    pub fn cards(&self, chat: i64) -> Vec<Call> {
        self.messages(chat)
            .into_iter()
            .filter(|call| call.body.pointer("/reply_markup/inline_keyboard").is_some())
            .collect()
    }

    /// The calls that put something in front of anyone between `from` and `to`.
    pub fn notified_between(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<Call> {
        self.calls()
            .into_iter()
            .filter(|call| call.notifies() && call.at >= from && call.at < to)
            .collect()
    }

    fn next_update_id(&self) -> i64 {
        self.shared.next_update.fetch_add(1, Ordering::SeqCst)
    }

    /// `user` types `text` in their private chat, which has the same id.
    pub fn say(&self, user: i64, name: &str, text: &str) {
        let message_id = self.shared.next_message.fetch_add(1, Ordering::SeqCst);
        let update = json!({
            "update_id": self.next_update_id(),
            "message": {
                "message_id": message_id,
                "from": { "id": user, "is_bot": false, "first_name": name, "language_code": "en" },
                "chat": { "id": user, "type": "private", "first_name": name },
                "date": Utc::now().timestamp(),
                "text": text,
            },
        });
        self.deliver(update);
    }

    /// `user` presses the button labelled `label` on the card `card`; `false` when the card
    /// has no such button.
    pub fn press(&self, user: i64, card: &Call, label: &str) -> bool {
        let Some(buttons) =
            card.body.pointer("/reply_markup/inline_keyboard").and_then(Value::as_array)
        else {
            return false;
        };
        let data = buttons
            .iter()
            .flat_map(|row| row.as_array().into_iter().flatten())
            .find_map(|button| (button["text"] == label).then(|| button["callback_data"].clone()));
        let Some(data) = data else { return false };
        let callback = self.shared.next_callback.fetch_add(1, Ordering::SeqCst);
        let update = json!({
            "update_id": self.next_update_id(),
            "callback_query": {
                "id": format!("cb{callback}"),
                "from": { "id": user, "is_bot": false, "first_name": "Ana" },
                "message": {
                    "message_id": card.message_id,
                    "chat": { "id": user, "type": "private" },
                    "date": card.at.timestamp(),
                    "text": card.body["text"],
                },
                "chat_instance": "1",
                "data": data,
            },
        });
        self.deliver(update);
        true
    }

    fn deliver(&self, update: Value) {
        self.shared.updates.lock().unwrap_or_else(PoisonError::into_inner).push_back(update);
        self.shared.arrived.notify_waiters();
    }
}

async fn call(
    State(shared): State<Arc<Shared>>,
    Path((bot, method)): Path<(String, String)>,
    body: Option<Json<Value>>,
) -> Response {
    if bot != format!("bot{}", shared.token) {
        let body = json!({ "ok": false, "error_code": 401, "description": "Unauthorized" });
        return (StatusCode::UNAUTHORIZED, Json(body)).into_response();
    }
    let body = body.map(|Json(body)| body).unwrap_or(Value::Null);
    if method != "getUpdates" {
        let message_id =
            (method == "sendMessage").then(|| shared.next_message.fetch_add(1, Ordering::SeqCst));
        let record =
            Call { method: method.clone(), body: body.clone(), message_id, at: Utc::now() };
        shared.calls.lock().unwrap_or_else(PoisonError::into_inner).push(record);
        let result = match message_id {
            Some(id) => json!({
                "message_id": id,
                "chat": { "id": body["chat_id"], "type": "private" },
                "date": Utc::now().timestamp(),
                "text": body["text"],
            }),
            None => json!(true),
        };
        return Json(json!({ "ok": true, "result": result })).into_response();
    }
    // getUpdates: an offset acknowledges every earlier update.
    let offset = body.get("offset").and_then(Value::as_i64);
    let wait = Duration::from_secs(body.get("timeout").and_then(Value::as_u64).unwrap_or(0));
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let arrived = shared.arrived.notified();
        let ready: Vec<Value> = {
            let mut updates = shared.updates.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(offset) = offset {
                updates.retain(|update| update["update_id"].as_i64().unwrap_or(0) >= offset);
            }
            updates.iter().cloned().collect()
        };
        if !ready.is_empty() || tokio::time::Instant::now() >= deadline {
            return Json(json!({ "ok": true, "result": ready })).into_response();
        }
        let _ = tokio::time::timeout_at(deadline, arrived).await;
    }
}
