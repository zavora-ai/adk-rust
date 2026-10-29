//! The WhatsApp adapter (spec 3.5 §10 X12, on spec 3.3 §5.3): the official WhatsApp Business
//! Cloud API only — a signed webhook in, the Graph API out.

mod api;
mod wire;

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

use adk_core::{AdkError, ErrorCategory, ErrorComponent};
use adk_kernel::KernelError;
use adk_kernel::channels::{
    ChannelGateway, ChannelInbound, ChannelNotice, ChannelSink, DecisionCard, InboundBody,
    StopNotice,
};
use async_trait::async_trait;
use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use chrono::Utc;
use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::Sha256;
use tokio::sync::mpsc;

use super::shared::{
    ANSWER_NOT_TAKEN, AbortOnDrop, ChannelConnection, HANDLING, Handling, Kit, MESSAGE_NOT_TAKEN,
    PressedCard, Taker, Update, dispatch, kit_builder, notice_words, patient, stopped_words,
};
use crate::error::{Result, SurfaceError};
use api::GraphApi;

/// The channel the WhatsApp adapter serves, as the kernel names it: pairings, thread sessions
/// and `kernel.channel` audit records carry it (spec 3.5 §10 X12).
pub const WHATSAPP_CHANNEL: &str = "whatsapp";

/// WhatsApp as a person reads it, in "You said yes in WhatsApp at 07:31" (spec 3.3 §8.1).
pub const WHATSAPP: &str = "WhatsApp";

const DEFAULT_API_BASE: &str = "https://graph.facebook.com/v23.0";
/// The customer-service window: how long after a person's last message the business may
/// write freely, in seconds.
const WINDOW_SECS: i64 = 24 * 60 * 60;
/// How many message ids the webhook remembers, so a replayed delivery is taken once.
const SEEN_KEPT: usize = 4096;

/// Why the WhatsApp adapter could not talk to WhatsApp (spec 3.5 §10 X12).
///
/// Never carries the access token or the app secret. Converts into [`AdkError`] with a
/// `surface.whatsapp.*` code and provider `whatsapp`.
///
/// # Example
///
/// ```rust
/// use adk_surface::channels::WhatsAppError;
///
/// let error = WhatsAppError::OutsideWindow;
/// assert_eq!(error.code(), "surface.whatsapp.outside_window");
/// assert!(error.to_string().contains("24-hour"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum WhatsAppError {
    /// WhatsApp refused the access token.
    #[error(
        "WhatsApp refused the access token: check WHATSAPP_ACCESS_TOKEN, a system user's token \
         with `whatsapp_business_messaging`"
    )]
    Unauthorized,
    /// The person last wrote more than 24 hours ago: outside the customer-service window only
    /// an approved template is delivered, so the message was not sent.
    #[error(
        "outside WhatsApp's 24-hour customer-service window: the message was not sent; the \
         person is reached again once they write, or by an approved template \
         (`WhatsAppAdapterBuilder::template`)"
    )]
    OutsideWindow,
    /// WhatsApp's throughput or spam limit refused the send.
    #[error("WhatsApp limited the number's sending (error {code}): try again later")]
    RateLimited {
        /// The Graph API error code.
        code: i64,
    },
    /// WhatsApp refused the send, such as a number that is not on WhatsApp.
    #[error("WhatsApp refused the message (error {code}): {message}")]
    Refused {
        /// The Graph API error code.
        code: i64,
        /// WhatsApp's message.
        message: String,
    },
    /// WhatsApp could not be reached.
    #[error("WhatsApp could not be reached: {reason}; check the network and the API address")]
    Unreachable {
        /// What failed.
        reason: String,
    },
    /// WhatsApp's answer was not the Graph API's JSON.
    #[error("WhatsApp's answer could not be read: {reason}")]
    InvalidResponse {
        /// What was wrong with it.
        reason: String,
    },
}

impl WhatsAppError {
    /// The error code: `surface.whatsapp.unauthorized`, `.outside_window`, `.rate_limited`,
    /// `.refused`, `.unreachable` or `.invalid_response`.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unauthorized => "surface.whatsapp.unauthorized",
            Self::OutsideWindow => "surface.whatsapp.outside_window",
            Self::RateLimited { .. } => "surface.whatsapp.rate_limited",
            Self::Refused { .. } => "surface.whatsapp.refused",
            Self::Unreachable { .. } => "surface.whatsapp.unreachable",
            Self::InvalidResponse { .. } => "surface.whatsapp.invalid_response",
        }
    }
}

impl From<WhatsAppError> for AdkError {
    fn from(error: WhatsAppError) -> Self {
        let category = match &error {
            WhatsAppError::Unauthorized => ErrorCategory::Unauthorized,
            WhatsAppError::OutsideWindow => ErrorCategory::Forbidden,
            WhatsAppError::RateLimited { .. } => ErrorCategory::RateLimited,
            WhatsAppError::Refused { .. } => ErrorCategory::InvalidInput,
            WhatsAppError::Unreachable { .. } => ErrorCategory::Unavailable,
            WhatsAppError::InvalidResponse { .. } => ErrorCategory::Internal,
        };
        AdkError::new(ErrorComponent::Server, category, error.code(), error.to_string())
            .with_provider("whatsapp")
    }
}

impl From<WhatsAppError> for KernelError {
    fn from(error: WhatsAppError) -> Self {
        Self::Core(error.into())
    }
}

/// Builds a [`WhatsAppAdapter`].
pub struct WhatsAppAdapterBuilder {
    phone_number_id: String,
    token: String,
    app_secret: Option<String>,
    verify_token: Option<String>,
    template: Option<(String, String)>,
    api_base: String,
    kit: Kit,
}

impl fmt::Debug for WhatsAppAdapterBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WhatsAppAdapterBuilder")
            .field("phone_number_id", &self.phone_number_id)
            .field("api_base", &self.api_base)
            .finish_non_exhaustive()
    }
}

impl WhatsAppAdapterBuilder {
    /// The Meta app's secret, which signs every webhook body (`X-Hub-Signature-256`).
    /// Required.
    #[must_use]
    pub fn app_secret(mut self, secret: impl Into<String>) -> Self {
        self.app_secret = Some(secret.into());
        self
    }

    /// The token the webhook's verification handshake (`hub.verify_token`) must carry, as set
    /// in the app's webhook settings. Required.
    #[must_use]
    pub fn verify_token(mut self, token: impl Into<String>) -> Self {
        self.verify_token = Some(token.into());
        self
    }

    /// The approved message template, with no variables, sent once in place of what cannot
    /// be sent outside the 24-hour customer-service window — "You have a decision waiting.
    /// Reply to see it." — in `language`, such as `en_US`. Without it, nothing is sent
    /// outside the window.
    #[must_use]
    pub fn template(mut self, name: impl Into<String>, language: impl Into<String>) -> Self {
        self.template = Some((name.into(), language.into()));
        self
    }

    /// The Graph API's address with its version; defaults to
    /// `https://graph.facebook.com/v23.0`. Plain `http` is accepted on loopback only (a test
    /// double), since the token travels with every call.
    #[must_use]
    pub fn api_base(mut self, url: impl Into<String>) -> Self {
        self.api_base = url.into();
        self
    }

    kit_builder!("WhatsApp");

    /// The adapter.
    ///
    /// # Errors
    ///
    /// Returns `surface.invalid` for a malformed phone number id or token, a missing app
    /// secret or verify token, or an API address that is not `https` (or `http` on
    /// loopback).
    pub fn build(self) -> Result<WhatsAppAdapter> {
        let api = GraphApi::new(&self.api_base, &self.phone_number_id, &self.token)
            .map_err(SurfaceError::Invalid)?;
        let secret = self.app_secret.filter(|secret| !secret.is_empty()).ok_or_else(|| {
            SurfaceError::Invalid(
                "the WhatsApp webhook needs the app secret (WHATSAPP_APP_SECRET) to check every \
                 body's signature"
                    .into(),
            )
        })?;
        let verify_token =
            self.verify_token.filter(|token| !token.is_empty()).ok_or_else(|| {
                SurfaceError::Invalid(
                    "the WhatsApp webhook needs a verify token (WHATSAPP_VERIFY_TOKEN) for its \
                 verification handshake"
                        .into(),
                )
            })?;
        Ok(WhatsAppAdapter {
            inner: Arc::new(Inner {
                api,
                kit: self.kit,
                phone_number_id: self.phone_number_id,
                secret: secret.into_bytes(),
                verify_token,
                template: self.template,
                state: Mutex::default(),
            }),
        })
    }
}

/// The WhatsApp adapter (spec 3.5 §10 X12): a WhatsApp Business number as the person's chat
/// thread with their agents, over the kernel's channel contract (spec 3.3 §10 K1), on the
/// official WhatsApp Business Cloud API only.
///
/// It is the gateway's [`ChannelSink`] for the channel [`WHATSAPP_CHANNEL`].
/// [`start`](Self::start) returns the webhook as an Axum router the host mounts on a public
/// HTTPS address, such as `/channels/whatsapp`: WhatsApp has no other way in.
///
/// | Direction | What |
/// |---|---|
/// | In | `GET` answers the verification handshake when `hub.verify_token` matches. `POST` is refused (`401`) before it is read unless `X-Hub-Signature-256` is the HMAC-SHA256 of the raw body with the app secret, compared in constant time. Text messages and pressed reply buttons from the configured number's customers reach the gateway; a message delivered again is taken once. Typed text never answers a decision (S-I4) and loses the characters a chat app shows as nothing (S-I6) |
/// | Out | A card as an interactive message: the chat screen's lines with WhatsApp's `*_~` markers escaped (S-I9), a reply button for each answer the card offers and no others — at most three, titles of twenty characters at most — and "Open in home" as its address; never the call's arguments (S-I5) |
/// | Window | Outside the 24-hour customer-service window nothing is sent: the send fails with [`WhatsAppError::OutsideWindow`], so the gateway knows, and an approved [`template`](WhatsAppAdapterBuilder::template), when set, is sent once in its place until the person writes again |
/// | Commands | "stop all" and `/stop` stop everything; resuming is refused (S-I10) |
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use adk_kernel::channels::{ChannelDoorbell, ChannelGateway, InMemoryChannelBindingStore};
/// use adk_surface::channels::WhatsAppAdapter;
///
/// # async fn example(kernel: adk_kernel::Kernel, doorbell: Arc<ChannelDoorbell>) -> Result<(), Box<dyn std::error::Error>> {
/// let whatsapp = WhatsAppAdapter::builder(std::env::var("WHATSAPP_PHONE_NUMBER_ID")?, std::env::var("WHATSAPP_ACCESS_TOKEN")?)
///     .app_secret(std::env::var("WHATSAPP_APP_SECRET")?)
///     .verify_token(std::env::var("WHATSAPP_VERIFY_TOKEN")?)
///     .build()?;
/// let gateway = ChannelGateway::builder(Arc::new(kernel), Arc::new(InMemoryChannelBindingStore::new()))
///     .sink(Arc::new(whatsapp.clone()))
///     .home_url("https://home.example/ui/")
///     .doorbell(doorbell)
///     .build();
/// gateway.start().await?;
/// let webhook = whatsapp.start(gateway);
/// let app = axum::Router::new().nest("/channels/whatsapp", webhook.router());
/// # let _ = app;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct WhatsAppAdapter {
    inner: Arc<Inner>,
}

struct Inner {
    api: GraphApi,
    kit: Kit,
    phone_number_id: String,
    secret: Vec<u8>,
    verify_token: String,
    template: Option<(String, String)>,
    state: Mutex<WindowState>,
}

/// What the adapter knows of each conversation's window, and the messages it took.
#[derive(Default)]
struct WindowState {
    /// When each person last wrote, in seconds since the epoch; `0` once WhatsApp said the
    /// window closed.
    last_written: HashMap<String, i64>,
    /// Conversations sent the template since the person last wrote.
    nudged: HashSet<String>,
    seen: HashSet<String>,
    order: VecDeque<String>,
}

impl WindowState {
    /// Notes message `id`; `false` when it was taken before.
    fn first_time(&mut self, id: &str) -> bool {
        if !self.seen.insert(id.to_string()) {
            return false;
        }
        self.order.push_back(id.to_string());
        while self.order.len() > SEEN_KEPT {
            if let Some(oldest) = self.order.pop_front() {
                self.seen.remove(&oldest);
            }
        }
        true
    }

    /// Whether the window of `thread` is open at `now`: `None` when the adapter has not seen
    /// the person write since it started, and WhatsApp decides.
    fn open(&self, thread: &str, now: i64) -> Option<bool> {
        self.last_written.get(thread).map(|at| now - at < WINDOW_SECS)
    }
}

impl fmt::Debug for WhatsAppAdapter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WhatsAppAdapter")
            .field("channel", &WHATSAPP_CHANNEL)
            .field("phone_number_id", &self.inner.phone_number_id)
            .finish_non_exhaustive()
    }
}

/// The webhook a [`WhatsAppAdapter::start`] serves: the router the host mounts, and the
/// handing over of what it receives; dropping it stops the handing over.
#[derive(Debug)]
pub struct WhatsAppWebhook {
    router: axum::Router,
    connection: ChannelConnection<WhatsAppError>,
}

impl WhatsAppWebhook {
    /// The webhook's routes, `GET` and `POST` on `/`: mount it with `Router::nest` at the
    /// path given to WhatsApp, such as `/channels/whatsapp`, on a public HTTPS address.
    pub fn router(&self) -> axum::Router {
        self.router.clone()
    }

    /// Stops handing messages over; the router then answers `503`, so WhatsApp delivers them
    /// again later.
    pub fn stop(&self) {
        self.connection.stop();
    }

    /// Whether the handing over stopped.
    pub fn is_finished(&self) -> bool {
        self.connection.is_finished()
    }

    /// Waits for the handing over to stop: once stopped, or once every router is dropped.
    ///
    /// # Errors
    ///
    /// Never fails today; the error type leaves room for the transport.
    pub async fn join(self) -> std::result::Result<(), WhatsAppError> {
        drop(self.router);
        self.connection.join().await
    }
}

/// What the webhook's handlers share.
struct Hook {
    adapter: WhatsAppAdapter,
    sender: mpsc::UnboundedSender<Update<Carried>>,
}

/// What a message carries beyond its inbound value.
pub(crate) struct Carried {
    /// The card a pressed button was on.
    card: Option<String>,
    /// The image it carried: its media id and declared type.
    media: Option<(String, Option<String>)>,
}

/// Compares `a` and `b` in time that does not depend on where they differ.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
}

/// `text` as bytes from hex, or `None`.
fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(text.get(index..index + 2)?, 16).ok())
        .collect()
}

impl WhatsAppAdapter {
    /// An adapter for the business number `phone_number_id` with the access token `token`,
    /// both from the app's WhatsApp settings; set its [`app_secret`](WhatsAppAdapterBuilder::app_secret)
    /// and [`verify_token`](WhatsAppAdapterBuilder::verify_token) too.
    pub fn builder(
        phone_number_id: impl Into<String>,
        token: impl Into<String>,
    ) -> WhatsAppAdapterBuilder {
        WhatsAppAdapterBuilder {
            phone_number_id: phone_number_id.into(),
            token: token.into(),
            app_secret: None,
            verify_token: None,
            template: None,
            api_base: DEFAULT_API_BASE.to_string(),
            kit: Kit::default(),
        }
    }

    /// What the adapter makes of one webhook payload (spec 3.3 S-I4, S-I6): each typed message
    /// and pressed answer for its number, in order. The payload's signature is checked by the
    /// webhook before this; this reads only.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_kernel::channels::InboundBody;
    /// use adk_surface::channels::WhatsAppAdapter;
    ///
    /// let whatsapp = WhatsAppAdapter::builder("1001", "EAAG-example")
    ///     .app_secret("app-secret")
    ///     .verify_token("verify-me")
    ///     .build()
    ///     .unwrap();
    /// let payload = serde_json::json!({
    ///     "object": "whatsapp_business_account",
    ///     "entry": [{ "id": "1", "changes": [{ "field": "messages", "value": {
    ///         "metadata": { "phone_number_id": "1001" },
    ///         "contacts": [{ "profile": { "name": "Ana" }, "wa_id": "254700000001" }],
    ///         "messages": [{ "from": "254700000001", "id": "wamid.1", "timestamp": "1761300000",
    ///                        "type": "text", "text": { "body": "stop all" } }],
    ///     }}]}],
    /// });
    /// let inbound = whatsapp.inbound(&payload);
    /// assert_eq!(inbound[0].body, InboundBody::Text { text: "stop all".into() });
    /// assert_eq!(inbound[0].thread, "254700000001");
    /// ```
    pub fn inbound(&self, payload: &Value) -> Vec<ChannelInbound> {
        wire::parse(payload, &self.inner.phone_number_id)
            .into_iter()
            .filter_map(|received| received.inbound)
            .collect()
    }

    /// Whether `signature`, an `X-Hub-Signature-256` header, is the HMAC-SHA256 of `body` with
    /// the app secret, compared in constant time.
    fn signed(&self, body: &[u8], signature: &str) -> bool {
        let Some(digest) = signature.strip_prefix("sha256=").and_then(unhex) else { return false };
        let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(&self.inner.secret) else { return false };
        mac.update(body);
        mac.verify_slice(&digest).is_ok()
    }

    /// Serves the webhook, handing each message to `gateway`, which must have this adapter as
    /// its sink, until [`WhatsAppWebhook::stop`].
    ///
    /// A message is answered `200` once it is queued, and taken by the gateway after: a paired
    /// account's messages first, each account's in the order sent (S-I6, S-I10); one the
    /// gateway fails on is taken again only when it is a paired account's and the failure may
    /// clear. A message whose id was taken before is acknowledged and not taken again, so
    /// a replayed delivery starts nothing.
    pub fn start(&self, gateway: ChannelGateway) -> WhatsAppWebhook {
        let (sender, task) = dispatch(Arc::new(self.clone()), gateway);
        let hook = Arc::new(Hook { adapter: self.clone(), sender });
        let router = axum::Router::new().route("/", get(verify).post(receive)).with_state(hook);
        let connection = ChannelConnection::spawn(async move {
            let mut dispatcher = AbortOnDrop(task);
            let _ = (&mut dispatcher.0).await;
            Ok(())
        });
        WhatsAppWebhook { router, connection }
    }

    /// Queues what `payload` carries; `false` once the handing over stopped.
    fn take_payload(
        &self,
        payload: &Value,
        sender: &mpsc::UnboundedSender<Update<Carried>>,
    ) -> bool {
        let now = Utc::now().timestamp();
        for received in wire::parse(payload, &self.inner.phone_number_id) {
            {
                let mut state = self.inner.state.lock().unwrap_or_else(PoisonError::into_inner);
                if !state.first_time(&received.id) {
                    tracing::debug!("a whatsapp message delivered again was not taken again");
                    continue;
                }
                // Every message from the person opens the window, whatever it carries.
                let at = received.at.unwrap_or(now).min(now);
                let last = state.last_written.entry(received.thread.clone()).or_insert(at);
                *last = (*last).max(at);
                state.nudged.remove(&received.thread);
            }
            if received.inbound.is_some()
                && sender
                    .send(Update {
                        inbound: received.inbound,
                        press: Some(Carried { card: received.card, media: received.media }),
                    })
                    .is_err()
            {
                return false;
            }
        }
        true
    }

    /// Sends `body` to `thread` when its window may be open; returns the message's id.
    async fn send(&self, thread: &str, body: &Value) -> adk_kernel::Result<String> {
        let now = Utc::now().timestamp();
        let open =
            self.inner.state.lock().unwrap_or_else(PoisonError::into_inner).open(thread, now);
        if open == Some(false) {
            self.nudge(thread).await;
            return Err(WhatsAppError::OutsideWindow.into());
        }
        // A throughput limit on a reply to an account that is not paired is not waited out.
        match self.inner.api.send(body, patient(WHATSAPP_CHANNEL, thread)).await {
            Err(WhatsAppError::OutsideWindow) => {
                self.inner
                    .state
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .last_written
                    .insert(thread.to_string(), 0);
                self.nudge(thread).await;
                Err(WhatsAppError::OutsideWindow.into())
            }
            sent => Ok(sent?),
        }
    }

    /// Sends the approved template to `thread` once until the person writes again.
    async fn nudge(&self, thread: &str) {
        let Some((name, language)) = &self.inner.template else { return };
        let first = self
            .inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .nudged
            .insert(thread.to_string());
        if !first {
            return;
        }
        if let Err(error) = self.inner.api.send(&wire::template(thread, name, language), true).await
        {
            tracing::warn!(error = %error, "the whatsapp template could not be sent");
            self.inner.state.lock().unwrap_or_else(PoisonError::into_inner).nudged.remove(thread);
        }
    }
}

/// The verification handshake: `hub.challenge` back when `hub.verify_token` matches.
async fn verify(
    State(hook): State<Arc<Hook>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let subscribe = query.get("hub.mode").map(String::as_str) == Some("subscribe");
    let matches = query
        .get("hub.verify_token")
        .is_some_and(|token| same(token.as_bytes(), hook.adapter.inner.verify_token.as_bytes()));
    match query.get("hub.challenge") {
        Some(challenge) if subscribe && matches => {
            (StatusCode::OK, challenge.clone()).into_response()
        }
        _ => StatusCode::FORBIDDEN.into_response(),
    }
}

/// A delivery: refused before it is read unless it is signed with the app secret.
async fn receive(State(hook): State<Arc<Hook>>, headers: HeaderMap, body: Bytes) -> StatusCode {
    let signature = headers.get("x-hub-signature-256").and_then(|value| value.to_str().ok());
    if !signature.is_some_and(|signature| hook.adapter.signed(&body, signature)) {
        tracing::warn!("a whatsapp webhook delivery without a valid signature was refused");
        return StatusCode::UNAUTHORIZED;
    }
    let Ok(payload) = serde_json::from_slice::<Value>(&body) else {
        return StatusCode::BAD_REQUEST;
    };
    if hook.adapter.take_payload(&payload, &hook.sender) {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

#[async_trait]
impl Taker for WhatsAppAdapter {
    /// The card a pressed button was on, and the image a message carried.
    type Press = Carried;

    fn channel_name(&self) -> &'static str {
        WHATSAPP_CHANNEL
    }

    async fn take(
        &self,
        gateway: &ChannelGateway,
        update: &Update<Carried>,
        paired: bool,
    ) -> adk_kernel::Result<()> {
        let Some(mut inbound) = update.inbound.clone() else { return Ok(()) };
        let carried = update.press.as_ref();
        // A paired account's image, for a host that takes it; a stranger's is never
        // downloaded (spec 3.5 §10 X12).
        if paired
            && let Some((id, declared)) = carried.and_then(|carried| carried.media.as_ref())
            && let Some(attachment) = self.inner.api.download(id, declared.as_deref()).await
        {
            inbound = inbound.with_attachments([attachment]);
        }
        let card = match &inbound.body {
            InboundBody::Answer { decision, .. } => Some(PressedCard {
                card: carried.and_then(|carried| carried.card.clone()),
                decision: decision.clone(),
            }),
            _ => None,
        };
        let handling = Handling::of(WHATSAPP_CHANNEL, gateway, &inbound, paired, card);
        HANDLING.scope(handling, gateway.receive(inbound)).await.map(drop)
    }

    async fn not_taken(&self, update: &Update<Carried>) {
        let Some(inbound) = &update.inbound else { return };
        let card = update.press.as_ref().and_then(|carried| carried.card.as_deref());
        let (text, reply_to) = match card {
            Some(card) => (ANSWER_NOT_TAKEN, Some(card)),
            None => (MESSAGE_NOT_TAKEN, Some(inbound.message_id.as_str())),
        };
        let body = wire::text(&inbound.thread, text, reply_to);
        if let Err(error) = self.send(&inbound.thread, &body).await {
            tracing::debug!(error = %error, "a person could not be told their message was not taken");
        }
    }
}

#[async_trait]
impl ChannelSink for WhatsAppAdapter {
    fn channel(&self) -> &str {
        WHATSAPP_CHANNEL
    }

    async fn send_message(&self, thread: &str, text: &str) -> adk_kernel::Result<()> {
        for body in wire::plain(thread, text) {
            self.send(thread, &body).await?;
        }
        Ok(())
    }

    async fn send_decision(&self, thread: &str, card: &DecisionCard) -> adk_kernel::Result<()> {
        let (heading, card) = self.inner.kit.prepare(card).await;
        self.send(thread, &wire::card(thread, &card, &heading, self.inner.kit.offset))
            .await
            .map(drop)
    }

    async fn send_stopped(&self, thread: &str, notice: &StopNotice) -> adk_kernel::Result<()> {
        self.send(thread, &wire::message(thread, &stopped_words(notice), None)).await.map(drop)
    }

    /// A notice about a pressed card records the answer with where it was given before the
    /// confirmation is sent, as a reply to the card (spec 3.3 §8.1, §11).
    async fn send_notice(&self, thread: &str, notice: &ChannelNotice) -> adk_kernel::Result<()> {
        let card = self.inner.kit.before_notice(WHATSAPP_CHANNEL, WHATSAPP, thread, notice).await;
        let card = card.filter(|card| !card.is_empty());
        let words = notice_words(notice, self.inner.kit.offset, Utc::now(), card.is_some());
        self.send(thread, &wire::message(thread, &words, card.as_deref())).await.map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adapter() -> WhatsAppAdapter {
        WhatsAppAdapter::builder("1001", "EAAG-secret")
            .app_secret("app-secret")
            .verify_token("verify-me")
            .build()
            .unwrap()
    }

    #[test]
    fn a_body_is_taken_only_with_the_app_secrets_signature() {
        let adapter = adapter();
        let body = br#"{"object":"whatsapp_business_account"}"#;
        let mut mac = Hmac::<Sha256>::new_from_slice(b"app-secret").unwrap();
        mac.update(body);
        let hex: String = mac.finalize().into_bytes().iter().map(|b| format!("{b:02x}")).collect();
        assert!(adapter.signed(body, &format!("sha256={hex}")));
        assert!(!adapter.signed(b"{}", &format!("sha256={hex}")), "another body");
        assert!(!adapter.signed(body, &hex), "no scheme");
        assert!(!adapter.signed(body, "sha256=zz"), "not hex");
        assert!(!adapter.signed(body, &format!("sha256={}", "0".repeat(64))));
        assert!(same(b"abc", b"abc") && !same(b"abc", b"abd") && !same(b"abc", b"ab"));
    }

    #[test]
    fn the_builder_needs_the_webhooks_secrets_and_never_echoes_them() {
        let missing =
            WhatsAppAdapter::builder("1001", "EAAG-secret").verify_token("v").build().unwrap_err();
        assert!(missing.to_string().contains("WHATSAPP_APP_SECRET"), "{missing}");
        let missing =
            WhatsAppAdapter::builder("1001", "EAAG-secret").app_secret("s").build().unwrap_err();
        assert!(missing.to_string().contains("WHATSAPP_VERIFY_TOKEN"), "{missing}");
        let bad = WhatsAppAdapter::builder("+1 555", "EAAG-secret")
            .app_secret("s")
            .verify_token("v")
            .build()
            .unwrap_err();
        assert!(!bad.to_string().contains("secret"), "{bad}");
        assert!(!format!("{:?}", adapter()).contains("secret"));
        assert_eq!(ChannelSink::channel(&adapter()), "whatsapp");
    }

    #[test]
    fn the_window_is_open_for_a_day_after_the_person_writes() {
        let mut state = WindowState::default();
        assert_eq!(state.open("t", 1000), None, "unknown: WhatsApp decides");
        state.last_written.insert("t".into(), 1000);
        assert_eq!(state.open("t", 1000 + WINDOW_SECS - 1), Some(true));
        assert_eq!(state.open("t", 1000 + WINDOW_SECS), Some(false));
        assert!(state.first_time("wamid.1") && !state.first_time("wamid.1"));
        for index in 0..SEEN_KEPT + 1 {
            state.first_time(&format!("wamid.x{index}"));
        }
        assert!(state.first_time("wamid.1"), "forgotten after {SEEN_KEPT}");
    }
}
