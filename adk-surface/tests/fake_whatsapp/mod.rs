//! A fake WhatsApp Business Cloud API on loopback (spec 3.5 §10 X12): the Graph API keeps
//! every message the adapter sends — what left the machine — and can refuse the next sends or
//! every send to one person; the webhook side builds Meta's payloads, signs them with the app
//! secret as Meta does, and posts them to the adapter's router.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use hmac::{Hmac, Mac};
use serde_json::{Value, json};
use sha2::Sha256;
use tower::ServiceExt;

pub const PHONE_NUMBER_ID: &str = "1001";
pub const TOKEN: &str = "EAAG-fake-access-token-for-tests";
pub const APP_SECRET: &str = "fake-app-secret";
pub const VERIFY_TOKEN: &str = "fake-verify-token";

/// One message the adapter sent.
#[derive(Debug, Clone)]
pub struct Sent {
    pub body: Value,
    /// The id WhatsApp gave it.
    pub id: Option<String>,
}

#[derive(Default)]
struct Shared {
    sent: Mutex<Vec<Sent>>,
    refusals: Mutex<VecDeque<(StatusCode, Value)>>,
    refused_to: Mutex<Vec<(String, Value)>>,
    next_id: AtomicU64,
}

/// The fake Graph API, served until it is dropped.
pub struct FakeWhatsApp {
    shared: Arc<Shared>,
    addr: SocketAddr,
    server: tokio::task::JoinHandle<()>,
    next_message: AtomicU64,
}

impl Drop for FakeWhatsApp {
    fn drop(&mut self) {
        self.server.abort();
    }
}

/// `body`'s `X-Hub-Signature-256` with `secret`.
pub fn signature(secret: &str, body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(body);
    let digest: String =
        mac.finalize().into_bytes().iter().map(|byte| format!("{byte:02x}")).collect();
    format!("sha256={digest}")
}

/// Posts `body` to the webhook `router` with `signature`, as Meta does; returns the status.
pub async fn post_to(router: &Router, body: Vec<u8>, signature: Option<String>) -> StatusCode {
    let mut request = Request::post("/").header("content-type", "application/json");
    if let Some(signature) = signature {
        request = request.header("x-hub-signature-256", signature);
    }
    let request = request.body(Body::from(body)).unwrap();
    router.clone().oneshot(request).await.unwrap().status()
}

/// Asks the webhook `router` to verify itself with `query`; returns the status and body.
pub async fn verify(router: &Router, query: &str) -> (StatusCode, String) {
    let request = Request::get(format!("/?{query}")).body(Body::empty()).unwrap();
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 16).await.unwrap();
    (status, String::from_utf8_lossy(&bytes).to_string())
}

impl FakeWhatsApp {
    pub async fn start() -> Self {
        let shared = Arc::new(Shared::default());
        let app = Router::new()
            .route("/v23.0/{phone}/messages", post(messages))
            .with_state(shared.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self { shared, addr, server, next_message: AtomicU64::new(1) }
    }

    /// The Graph API's address with its version, for `WhatsAppAdapterBuilder::api_base`.
    pub fn url(&self) -> String {
        format!("http://{}/v23.0", self.addr)
    }

    pub fn sent(&self) -> Vec<Sent> {
        self.shared.sent.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// The messages sent to `to`, in order.
    pub fn sent_to(&self, to: &str) -> Vec<Sent> {
        self.sent().into_iter().filter(|sent| sent.body["to"] == to).collect()
    }

    /// The text each message to `to` shows: a text's body or an interactive message's body.
    pub fn texts(&self, to: &str) -> Vec<String> {
        self.sent_to(to)
            .into_iter()
            .filter_map(|sent| {
                sent.body["text"]["body"]
                    .as_str()
                    .or_else(|| sent.body["interactive"]["body"]["text"].as_str())
                    .map(str::to_string)
            })
            .collect()
    }

    /// The last decision card sent to `to`.
    pub fn last_card(&self, to: &str) -> Option<Sent> {
        self.sent_to(to).into_iter().rev().find(|sent| {
            let text = sent.body["interactive"]["body"]["text"]
                .as_str()
                .or_else(|| sent.body["text"]["body"].as_str());
            text.is_some_and(|text| text.starts_with("Waiting for you"))
        })
    }

    /// Refuses the next `times` sends with `status` and Graph's `error`.
    pub fn refuse(&self, times: usize, status: StatusCode, error: Value) {
        let mut refusals = self.shared.refusals.lock().unwrap_or_else(PoisonError::into_inner);
        for _ in 0..times {
            refusals.push_back((status, json!({ "error": error.clone() })));
        }
    }

    /// Refuses every send to `to` with Graph's `error`.
    pub fn refuse_to(&self, to: &str, error: Value) {
        self.shared
            .refused_to
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((to.to_string(), json!({ "error": error })));
    }

    fn message_id(&self) -> String {
        format!("wamid.in{}", self.next_message.fetch_add(1, Ordering::SeqCst))
    }

    /// A payload carrying `message` from `from`, whose WhatsApp id replies go to is `thread`.
    pub fn payload(thread: &str, message: Value) -> Value {
        json!({ "object": "whatsapp_business_account", "entry": [{ "id": "waba-1", "changes": [{
            "field": "messages",
            "value": {
                "messaging_product": "whatsapp",
                "metadata": { "display_phone_number": "15550001001", "phone_number_id": PHONE_NUMBER_ID },
                "contacts": [{ "profile": { "name": "Ana" }, "wa_id": thread }],
                "messages": [message],
            },
        }]}]})
    }

    /// `from` typing `text`, sent at `timestamp`.
    pub fn text_at(&self, from: &str, thread: &str, text: &str, timestamp: i64) -> Value {
        let message = json!({ "from": from, "id": self.message_id(), "timestamp": timestamp.to_string(), "type": "text", "text": { "body": text } });
        Self::payload(thread, message)
    }

    /// `from` typing `text` now.
    pub fn text(&self, from: &str, thread: &str, text: &str) -> Value {
        self.text_at(from, thread, text, chrono::Utc::now().timestamp())
    }

    /// `from` pressing the reply button titled `title` on the last card sent to `thread`, or
    /// `None` when it has no such button.
    pub fn press(&self, from: &str, thread: &str, title: &str) -> Option<Value> {
        let card = self.last_card(thread)?;
        let buttons = card.body["interactive"]["action"]["buttons"].as_array()?;
        let button = buttons.iter().find(|button| button["reply"]["title"] == title)?;
        let message = json!({
            "from": from, "id": self.message_id(), "timestamp": chrono::Utc::now().timestamp().to_string(),
            "type": "interactive", "context": { "from": "15550001001", "id": card.id },
            "interactive": { "type": "button_reply", "button_reply": button["reply"].clone() },
        });
        Some(Self::payload(thread, message))
    }
}

async fn messages(
    State(shared): State<Arc<Shared>>,
    Path(phone): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let auth =
        headers.get("authorization").and_then(|value| value.to_str().ok()).unwrap_or_default();
    if auth != format!("Bearer {TOKEN}") || phone != PHONE_NUMBER_ID {
        let error = json!({ "error": { "message": "Invalid OAuth access token.", "type": "OAuthException", "code": 190 } });
        return (StatusCode::UNAUTHORIZED, Json(error)).into_response();
    }
    let refusal =
        shared.refusals.lock().unwrap_or_else(PoisonError::into_inner).pop_front().or_else(|| {
            let refused = shared.refused_to.lock().unwrap_or_else(PoisonError::into_inner);
            refused
                .iter()
                .find(|(to, _)| body["to"] == *to)
                .map(|(_, error)| (StatusCode::BAD_REQUEST, error.clone()))
        });
    let id = refusal
        .is_none()
        .then(|| format!("wamid.out{}", shared.next_id.fetch_add(1, Ordering::SeqCst)));
    shared
        .sent
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(Sent { body: body.clone(), id: id.clone() });
    if let Some((status, error)) = refusal {
        return (status, Json(error)).into_response();
    }
    Json(json!({ "messaging_product": "whatsapp", "contacts": [{ "input": body["to"], "wa_id": body["to"] }], "messages": [{ "id": id }] })).into_response()
}
