//! A fake Telegram Bot API on loopback (spec 3.3 §5.3): it keeps every call the adapter
//! makes — what left the machine — serves `getUpdates` from a queue the test fills, as a
//! person typing and pressing buttons would, and can refuse the next calls of a method.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, routing::post};
use serde_json::{Value, json};
use tokio::sync::Notify;

/// The fake bot's token.
pub const TOKEN: &str = "424242:AAFakeBotToken_for-tests";

/// One call the adapter made.
#[derive(Debug, Clone)]
pub struct Call {
    /// The Bot API method.
    pub method: String,
    /// The JSON body.
    pub body: Value,
    /// The id of the message a `sendMessage` created.
    pub message_id: Option<i64>,
}

struct Shared {
    token: String,
    calls: Mutex<Vec<Call>>,
    updates: Mutex<VecDeque<Value>>,
    arrived: Notify,
    next_update: AtomicI64,
    next_message: AtomicI64,
    next_callback: AtomicI64,
    refusals: Mutex<VecDeque<(String, StatusCode, Value)>>,
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
    pub async fn start() -> Self {
        Self::with_token(TOKEN).await
    }

    /// A fake API for the bot with `token`.
    pub async fn with_token(token: &str) -> Self {
        let shared = Arc::new(Shared {
            token: token.to_string(),
            calls: Mutex::default(),
            updates: Mutex::default(),
            arrived: Notify::new(),
            next_update: AtomicI64::new(1),
            next_message: AtomicI64::new(100),
            next_callback: AtomicI64::new(1),
            refusals: Mutex::default(),
        });
        let app = Router::new().route("/{bot}/{method}", post(call)).with_state(shared.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self { shared, addr, server }
    }

    /// The API's address, for `TelegramAdapterBuilder::api_base`.
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Every call so far.
    pub fn calls(&self) -> Vec<Call> {
        self.shared.calls.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// The calls of `method` so far.
    pub fn calls_of(&self, method: &str) -> Vec<Call> {
        self.calls().into_iter().filter(|call| call.method == method).collect()
    }

    /// Every call addressed to chat `chat`, a number or a text id, in order.
    pub fn to_chat(&self, chat: &Value) -> Vec<Call> {
        self.calls().into_iter().filter(|call| call.body.get("chat_id") == Some(chat)).collect()
    }

    /// The texts sent to `chat`.
    pub fn texts(&self, chat: i64) -> Vec<String> {
        self.to_chat(&json!(chat))
            .into_iter()
            .filter(|call| call.method == "sendMessage")
            .filter_map(|call| call.body["text"].as_str().map(str::to_string))
            .collect()
    }

    /// The last decision card sent to `chat`: its message id and body.
    pub fn last_card(&self, chat: &Value) -> Option<Call> {
        self.to_chat(chat).into_iter().rev().find(|call| {
            call.method == "sendMessage"
                && call.body["text"].as_str().is_some_and(|text| text.starts_with("Waiting for you"))
        })
    }

    /// Refuses the next `times` calls of `method` with `status` and `body`.
    pub fn refuse(&self, method: &str, times: usize, status: StatusCode, body: Value) {
        let mut refusals = self.shared.refusals.lock().unwrap_or_else(PoisonError::into_inner);
        for _ in 0..times {
            refusals.push_back((method.to_string(), status, body.clone()));
        }
    }

    fn next_update_id(&self) -> i64 {
        self.shared.next_update.fetch_add(1, Ordering::SeqCst)
    }

    /// An update of `user` typing `text` in their private chat `chat`, not yet delivered.
    pub fn text_update(&self, user: i64, chat: i64, text: &str) -> Value {
        let message_id = self.shared.next_message.fetch_add(1, Ordering::SeqCst);
        json!({
            "update_id": self.next_update_id(),
            "message": {
                "message_id": message_id,
                "from": { "id": user, "is_bot": false, "first_name": "Ana", "language_code": "en" },
                "chat": { "id": chat, "type": "private", "first_name": "Ana" },
                "date": 1_761_300_000,
                "text": text,
            },
        })
    }

    /// An update of `user` pressing the button labelled `label` on the last card sent to
    /// `chat`, or `None` when that card has no such button.
    pub fn press_update(&self, user: i64, chat: i64, label: &str) -> Option<Value> {
        let card = self.last_card(&json!(chat))?;
        let buttons = card.body.pointer("/reply_markup/inline_keyboard")?.as_array()?;
        let data = buttons.iter().flat_map(|row| row.as_array().into_iter().flatten()).find_map(
            |button| (button["text"] == label).then(|| button["callback_data"].clone()),
        )?;
        let callback = self.shared.next_callback.fetch_add(1, Ordering::SeqCst);
        Some(json!({
            "update_id": self.next_update_id(),
            "callback_query": {
                "id": format!("cb{callback}"),
                "from": { "id": user, "is_bot": false, "first_name": "Ana" },
                "message": {
                    "message_id": card.message_id,
                    "chat": { "id": chat, "type": "private" },
                    "date": 1_761_300_000,
                    "text": card.body["text"],
                },
                "chat_instance": "1",
                "data": data,
            },
        }))
    }

    /// Delivers `update` through `getUpdates`.
    pub fn deliver(&self, update: Value) {
        self.shared.updates.lock().unwrap_or_else(PoisonError::into_inner).push_back(update);
        self.shared.arrived.notify_waiters();
    }

    /// `user` types `text` in chat `chat`.
    pub fn say(&self, user: i64, chat: i64, text: &str) {
        let update = self.text_update(user, chat, text);
        self.deliver(update);
    }

    /// `user` presses `label` on the last card in `chat`; `false` when there is no such
    /// button.
    pub fn press(&self, user: i64, chat: i64, label: &str) -> bool {
        match self.press_update(user, chat, label) {
            Some(update) => {
                self.deliver(update);
                true
            }
            None => false,
        }
    }

    /// Whether every delivered update was taken and acknowledged.
    pub fn all_read(&self) -> bool {
        self.shared.updates.lock().unwrap_or_else(PoisonError::into_inner).is_empty()
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
    let refusal = {
        let mut refusals = shared.refusals.lock().unwrap_or_else(PoisonError::into_inner);
        let index = refusals.iter().position(|(refused, ..)| *refused == method);
        index.and_then(|index| refusals.remove(index))
    };
    if method != "getUpdates" {
        let message_id = (method == "sendMessage" && refusal.is_none())
            .then(|| shared.next_message.fetch_add(1, Ordering::SeqCst));
        let record = Call { method: method.clone(), body: body.clone(), message_id };
        shared.calls.lock().unwrap_or_else(PoisonError::into_inner).push(record);
        if let Some((_, status, refused)) = refusal {
            return (status, Json(refused)).into_response();
        }
        let result = match message_id {
            Some(id) => json!({
                "message_id": id,
                "chat": { "id": body["chat_id"], "type": "private" },
                "date": 1_761_300_000,
                "text": body["text"],
            }),
            None => json!(true),
        };
        return Json(json!({ "ok": true, "result": result })).into_response();
    }
    if let Some((_, status, refused)) = refusal {
        return (status, Json(refused)).into_response();
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
