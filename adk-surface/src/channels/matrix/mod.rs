//! The Matrix adapter (spec 3.5 §10 X12, on spec 3.3 §5.3): `/sync` long polling in,
//! `m.room.message` out, reactions as answers.

mod api;
mod wire;

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use adk_core::{AdkError, ErrorCategory, ErrorComponent};
use adk_kernel::KernelError;
use adk_kernel::channels::{
    ChannelGateway, ChannelInbound, ChannelNotice, ChannelSink, DecisionCard, InboundBody,
    StopNotice,
};
use async_trait::async_trait;
use chrono::Utc;
use reqwest::Method;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::shared::{
    ANSWER_NOT_TAKEN, AbortOnDrop, ChannelConnection, HANDLING, Handling, Kit, MESSAGE_NOT_TAKEN,
    PressedCard, Taker, Update, dispatch, kit_builder, notice_words, paired_binding, patient,
    stopped_words,
};
use crate::error::{Result, SurfaceError};
use api::{CALL, MatrixApi};
use wire::CardRef;

/// The channel the Matrix adapter serves, as the kernel names it: pairings, thread sessions
/// and `kernel.channel` audit records carry it (spec 3.5 §10 X12).
pub const MATRIX_CHANNEL: &str = "matrix";

/// Matrix as a person reads it, in "You said yes in Matrix at 07:31" (spec 3.3 §8.1).
pub const MATRIX: &str = "Matrix";

/// How long one `/sync` waits for an event.
const DEFAULT_POLL_TIMEOUT: Duration = Duration::from_secs(25);
const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// How many cards the adapter remembers; an older card is read back from the homeserver.
const CARDS_KEPT: usize = 1024;
/// The events `/sync` returns: a room's timeline and state, nothing about presence or
/// account data.
const FILTER: &str = r#"{"presence":{"types":[]},"account_data":{"types":[]},"room":{"ephemeral":{"types":[]},"account_data":{"types":[]},"timeline":{"limit":50}}}"#;

/// Why the Matrix adapter could not talk to the homeserver (spec 3.5 §10 X12).
///
/// Never carries the access token. Converts into [`AdkError`] with a `surface.matrix.*` code
/// and provider `matrix`.
///
/// # Example
///
/// ```rust
/// use adk_surface::channels::MatrixError;
///
/// let error = MatrixError::Unauthorized;
/// assert_eq!(error.code(), "surface.matrix.unauthorized");
/// assert!(error.to_string().contains("MATRIX_ACCESS_TOKEN"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum MatrixError {
    /// The homeserver refused the access token.
    #[error(
        "the Matrix homeserver refused the access token: check MATRIX_ACCESS_TOKEN, or log the \
         bot account in again for a new one"
    )]
    Unauthorized,
    /// The homeserver asked the account to slow down for longer than the adapter waits.
    #[error(
        "the Matrix homeserver asked to wait {retry_after_ms} ms before {endpoint}: the account is sending too much"
    )]
    RateLimited {
        /// The endpoint, such as `rooms`.
        endpoint: String,
        /// How long the homeserver asked to wait, in milliseconds.
        retry_after_ms: u64,
    },
    /// The homeserver refused the call, such as a room the account is not in.
    #[error("the Matrix homeserver refused {endpoint} (HTTP {status}, {errcode}): {error}")]
    Refused {
        /// The endpoint, such as `rooms`.
        endpoint: String,
        /// The HTTP status.
        status: u16,
        /// The Matrix error code, such as `M_FORBIDDEN`.
        errcode: String,
        /// The homeserver's description.
        error: String,
    },
    /// The homeserver could not be reached.
    #[error(
        "the Matrix homeserver could not be reached for {endpoint}: {reason}; check the \
         network and MATRIX_HOMESERVER"
    )]
    Unreachable {
        /// The endpoint.
        endpoint: String,
        /// What failed.
        reason: String,
    },
    /// The homeserver's answer was not the API's JSON.
    #[error("the Matrix homeserver's answer to {endpoint} could not be read: {reason}")]
    InvalidResponse {
        /// The endpoint.
        endpoint: String,
        /// What was wrong with it.
        reason: String,
    },
}

impl MatrixError {
    /// The error code: `surface.matrix.unauthorized`, `.rate_limited`, `.refused`,
    /// `.unreachable` or `.invalid_response`.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unauthorized => "surface.matrix.unauthorized",
            Self::RateLimited { .. } => "surface.matrix.rate_limited",
            Self::Refused { .. } => "surface.matrix.refused",
            Self::Unreachable { .. } => "surface.matrix.unreachable",
            Self::InvalidResponse { .. } => "surface.matrix.invalid_response",
        }
    }
}

impl From<MatrixError> for AdkError {
    fn from(error: MatrixError) -> Self {
        let category = match &error {
            MatrixError::Unauthorized => ErrorCategory::Unauthorized,
            MatrixError::RateLimited { .. } => ErrorCategory::RateLimited,
            MatrixError::Refused { status: 403, .. } => ErrorCategory::Forbidden,
            MatrixError::Refused { status: 404, .. } => ErrorCategory::NotFound,
            MatrixError::Refused { status: 400, .. } => ErrorCategory::InvalidInput,
            MatrixError::Refused { .. } | MatrixError::Unreachable { .. } => {
                ErrorCategory::Unavailable
            }
            MatrixError::InvalidResponse { .. } => ErrorCategory::Internal,
        };
        AdkError::new(ErrorComponent::Server, category, error.code(), error.to_string())
            .with_provider("matrix")
    }
}

impl From<MatrixError> for KernelError {
    fn from(error: MatrixError) -> Self {
        Self::Core(error.into())
    }
}

/// The `/sync` long polling a [`MatrixAdapter::start`] runs; dropping it stops it.
pub type MatrixSync = ChannelConnection<MatrixError>;

/// Builds a [`MatrixAdapter`].
pub struct MatrixAdapterBuilder {
    homeserver: String,
    token: String,
    user_id: Option<String>,
    poll_timeout: Duration,
    kit: Kit,
}

impl fmt::Debug for MatrixAdapterBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MatrixAdapterBuilder")
            .field("homeserver", &self.homeserver)
            .field("poll_timeout", &self.poll_timeout)
            .finish_non_exhaustive()
    }
}

impl MatrixAdapterBuilder {
    /// The account's user id, such as `@agents:example.org`; asked of the homeserver
    /// (`/account/whoami`) when the adapter starts when not set.
    #[must_use]
    pub fn user_id(mut self, user_id: impl Into<String>) -> Self {
        self.user_id = Some(user_id.into());
        self
    }

    /// How long one `/sync` waits for an event, at least a second; defaults to 25 s.
    #[must_use]
    pub fn poll_timeout(mut self, timeout: Duration) -> Self {
        self.poll_timeout = timeout.max(Duration::from_secs(1));
        self
    }

    kit_builder!("Matrix");

    /// The adapter.
    ///
    /// # Errors
    ///
    /// Returns `surface.invalid` for an empty or malformed token, or a homeserver address
    /// that is not `https` (or `http` on loopback).
    pub fn build(self) -> Result<MatrixAdapter> {
        let api = MatrixApi::new(&self.homeserver, &self.token).map_err(SurfaceError::Invalid)?;
        Ok(MatrixAdapter {
            inner: Arc::new(Inner {
                api,
                kit: self.kit,
                poll_timeout: self.poll_timeout,
                me: Mutex::new(self.user_id),
                rooms: Mutex::default(),
            }),
        })
    }
}

/// The Matrix adapter (spec 3.5 §10 X12): a Matrix account's direct chats as the person's
/// chat thread with their agents, over the kernel's channel contract (spec 3.3 §10 K1).
///
/// It is the gateway's [`ChannelSink`] for the channel [`MATRIX_CHANNEL`], and
/// [`start`](Self::start) reads the account's rooms by `/sync` long polling with the `since`
/// token — no inbound port — and hands each event to [`ChannelGateway::receive`].
///
/// | Direction | What |
/// |---|---|
/// | Out | A card as `m.room.message`: "Waiting for you · since 18:00", the call in plain words, the agent and the risk, its answers as reaction keys — "To answer, react 👍 Allow once · 👎 Deny" — with the adapter's own reactions on it to tap, and "Open in home"; never the call's arguments (S-I5). Every `formatted_body` escapes what it shows; agent replies have a plain `body` only (S-I9) |
/// | In | New text messages in a direct chat the account was invited to and joined, and reactions. Matrix has no buttons: only an `m.reaction` annotation from the account paired from that room, on one of the adapter's cards, with one of the keys the card offers, answers it; typed text never does (S-I4). Typed text loses the characters a chat app shows as nothing (S-I6) |
/// | Rooms | Unencrypted direct chats only: an encrypted room is told once to use an unencrypted direct chat, and nothing in it reaches an agent. Rooms with more than two members are ignored. Events from before the adapter started are not read |
/// | Commands | "stop all" and `/stop` stop everything; resuming is refused (S-I10) |
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use adk_kernel::channels::{ChannelDoorbell, ChannelGateway, InMemoryChannelBindingStore};
/// use adk_surface::channels::MatrixAdapter;
///
/// # async fn example(kernel: adk_kernel::Kernel, doorbell: Arc<ChannelDoorbell>) -> Result<(), Box<dyn std::error::Error>> {
/// let matrix = MatrixAdapter::builder(std::env::var("MATRIX_HOMESERVER")?, std::env::var("MATRIX_ACCESS_TOKEN")?).build()?;
/// let gateway = ChannelGateway::builder(Arc::new(kernel), Arc::new(InMemoryChannelBindingStore::new()))
///     .sink(Arc::new(matrix.clone()))
///     .home_url("http://127.0.0.1:8080/ui/")
///     .doorbell(doorbell)
///     .build();
/// gateway.start().await?;
/// let sync = matrix.start(gateway);
/// sync.join().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct MatrixAdapter {
    inner: Arc<Inner>,
}

struct Inner {
    api: MatrixApi,
    kit: Kit,
    poll_timeout: Duration,
    /// The account's own user id.
    me: Mutex<Option<String>>,
    rooms: Mutex<Rooms>,
}

/// What an event carries beyond its inbound value.
pub(crate) struct Carried {
    /// The card a reaction annotates.
    card: Option<String>,
    /// The image an `m.image` event names.
    files: Vec<crate::channels::media::FileRef>,
}

/// What the adapter knows of its rooms and cards.
#[derive(Default)]
struct Rooms {
    /// Rooms whose events are encrypted.
    encrypted: HashSet<String>,
    /// Encrypted rooms already told to use an unencrypted chat.
    told: HashSet<String>,
    /// Rooms with more than two members.
    groups: HashSet<String>,
    /// The cards sent, by event id, newest last.
    cards: HashMap<String, CardRef>,
    order: VecDeque<String>,
}

impl Rooms {
    fn keep(&mut self, event: String, card: CardRef) {
        if self.cards.insert(event.clone(), card).is_none() {
            self.order.push_back(event);
        }
        while self.order.len() > CARDS_KEPT {
            if let Some(oldest) = self.order.pop_front() {
                self.cards.remove(&oldest);
            }
        }
    }
}

impl fmt::Debug for MatrixAdapter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MatrixAdapter").field("channel", &MATRIX_CHANNEL).finish_non_exhaustive()
    }
}

impl MatrixAdapter {
    /// An adapter for the account with access token `token` on `homeserver`, such as
    /// `https://matrix.example.org`.
    pub fn builder(
        homeserver: impl Into<String>,
        token: impl Into<String>,
    ) -> MatrixAdapterBuilder {
        MatrixAdapterBuilder {
            homeserver: homeserver.into(),
            token: token.into(),
            user_id: None,
            poll_timeout: DEFAULT_POLL_TIMEOUT,
            kit: Kit::default(),
        }
    }

    fn rooms(&self) -> std::sync::MutexGuard<'_, Rooms> {
        self.inner.rooms.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn me(&self) -> Option<String> {
        self.inner.me.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// What the adapter makes of one timeline event of `room` (spec 3.3 S-I4, S-I6): a typed
    /// message, a reaction that answers one of its cards, or `None` for anything the gateway
    /// is not given. A card the adapter no longer remembers is read back from the
    /// homeserver, and counts only when the adapter's own account sent it.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_kernel::channels::InboundBody;
    /// use adk_surface::channels::MatrixAdapter;
    ///
    /// # #[tokio::main(flavor = "current_thread")]
    /// # async fn main() {
    /// let matrix = MatrixAdapter::builder("https://matrix.example.org", "syt_example").build().unwrap();
    /// let event = serde_json::json!({
    ///     "type": "m.room.message",
    ///     "sender": "@ana:example.org",
    ///     "event_id": "$1",
    ///     "content": { "msgtype": "m.text", "body": "stop all" },
    /// });
    /// let inbound = matrix.inbound("!dm:example.org", &event).await.unwrap();
    /// assert_eq!(inbound.body, InboundBody::Text { text: "stop all".into() });
    /// assert_eq!(inbound.thread, "!dm:example.org");
    /// # }
    /// ```
    pub async fn inbound(&self, room: &str, event: &Value) -> Option<ChannelInbound> {
        if event["type"] == "m.reaction" {
            let (card, _) = wire::reaction(event)?;
            let card = self.card(room, &card).await?;
            return wire::answer(room, event, &card);
        }
        wire::typed(room, event, self.me().as_deref())
    }

    /// What the card `event` in `room` answers: remembered, or read back when the adapter's
    /// own account sent it.
    async fn card(&self, room: &str, event: &str) -> Option<CardRef> {
        if let Some(card) = self.rooms().cards.get(event).cloned() {
            return Some(card);
        }
        let me = self.me()?;
        let path = ["rooms", room, "event", event];
        let fetched = self.inner.api.call(Method::GET, &path, &[], None, CALL, true).await;
        let fetched = match fetched {
            Ok(fetched) => fetched,
            Err(error) => {
                tracing::debug!(error = %error, "a reacted-to matrix event could not be read");
                return None;
            }
        };
        if fetched["sender"].as_str() != Some(me.as_str()) {
            return None;
        }
        let card = wire::card_ref(&fetched["content"])?;
        self.rooms().keep(event.to_string(), card.clone());
        Some(card)
    }

    /// Reads the account's rooms by `/sync` long polling and hands each event to `gateway`,
    /// which must have this adapter as its sink, until [`MatrixSync::stop`].
    ///
    /// The first `/sync` only takes the position to read from, so nothing sent before the
    /// adapter started is read. The account joins a room it is invited to as a direct chat
    /// and no other. A paired account's events go to the gateway first, each account's in the
    /// order sent (S-I6, S-I10); one the gateway fails on is taken again only when it is a
    /// paired account's and the failure may clear. Transport failures are retried with
    /// backoff; a refused token ends the polling with [`MatrixError::Unauthorized`].
    pub fn start(&self, gateway: ChannelGateway) -> MatrixSync {
        let adapter = self.clone();
        ChannelConnection::spawn(async move { adapter.run(gateway).await })
    }

    async fn run(&self, gateway: ChannelGateway) -> std::result::Result<(), MatrixError> {
        let mut backoff = MIN_BACKOFF;
        let me = loop {
            if let Some(me) = self.me() {
                break me;
            }
            match self
                .inner
                .api
                .call(Method::GET, &["account", "whoami"], &[], None, CALL, true)
                .await
            {
                Ok(whoami) => {
                    if let Some(me) = whoami["user_id"].as_str() {
                        *self.inner.me.lock().unwrap_or_else(PoisonError::into_inner) =
                            Some(me.to_string());
                    }
                }
                Err(MatrixError::Unauthorized) => return Err(MatrixError::Unauthorized),
                Err(error) => {
                    tracing::warn!(error = %error, "the matrix account could not be read; retrying");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                }
            }
        };
        let (sender, task) = dispatch(Arc::new(self.clone()), gateway);
        let _dispatcher = AbortOnDrop(task);
        let mut since: Option<String> = None;
        let timeout = self.inner.poll_timeout.as_millis().to_string();
        loop {
            let mut query = vec![("timeout", timeout.as_str()), ("filter", FILTER)];
            if let Some(since) = &since {
                query.push(("since", since.as_str()));
            }
            let wait = self.inner.poll_timeout + CALL;
            let batch = match self
                .inner
                .api
                .call(Method::GET, &["sync"], &query, None, wait, false)
                .await
            {
                Ok(batch) => {
                    backoff = MIN_BACKOFF;
                    batch
                }
                Err(MatrixError::Unauthorized) => return Err(MatrixError::Unauthorized),
                Err(MatrixError::RateLimited { retry_after_ms, .. }) => {
                    tokio::time::sleep(Duration::from_millis(retry_after_ms.max(100))).await;
                    continue;
                }
                Err(error) => {
                    tracing::warn!(error = %error, "reading matrix rooms failed; retrying");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                    continue;
                }
            };
            let first = since.is_none();
            if let Some(next) = batch["next_batch"].as_str() {
                since = Some(next.to_string());
            }
            self.take_batch(&batch, first, &me, &sender).await;
        }
    }

    /// Joins the direct chats the account was invited to, notes encrypted rooms and rooms of
    /// more than two, and hands every new event of the others over.
    async fn take_batch(
        &self,
        batch: &Value,
        first: bool,
        me: &str,
        sender: &mpsc::UnboundedSender<Update<Carried>>,
    ) {
        let rooms = &batch["rooms"];
        for (room, invite) in rooms["invite"].as_object().into_iter().flatten() {
            let state = invite["invite_state"]["events"].as_array().cloned().unwrap_or_default();
            let direct = state.iter().any(|event| {
                event["type"] == "m.room.member"
                    && event["state_key"] == me
                    && event["content"]["is_direct"] == true
            });
            if !direct {
                tracing::debug!("a matrix invite to a room that is not a direct chat was left");
                continue;
            }
            if state.iter().any(|event| event["type"] == "m.room.encryption") {
                self.rooms().encrypted.insert(room.clone());
            }
            if let Err(error) = self
                .inner
                .api
                .call(Method::POST, &["join", room], &[], Some(&json!({})), CALL, true)
                .await
            {
                tracing::warn!(error = %error, "a matrix direct chat could not be joined");
            }
        }
        for (room, joined) in rooms["join"].as_object().into_iter().flatten() {
            let timeline = joined["timeline"]["events"].as_array().cloned().unwrap_or_default();
            let state = joined["state"]["events"].as_array().cloned().unwrap_or_default();
            let encrypted = {
                let mut rooms = self.rooms();
                if state.iter().chain(&timeline).any(|event| event["type"] == "m.room.encryption") {
                    rooms.encrypted.insert(room.clone());
                }
                match joined["summary"]["m.joined_member_count"].as_u64() {
                    Some(count) if count > 2 => {
                        rooms.groups.insert(room.clone());
                    }
                    Some(_) => {
                        rooms.groups.remove(room);
                    }
                    None => {}
                }
                rooms.encrypted.contains(room)
            };
            if first {
                continue;
            }
            let others: Vec<&Value> =
                timeline.iter().filter(|event| event["sender"].as_str() != Some(me)).collect();
            if encrypted {
                let spoke = others.iter().any(|event| {
                    matches!(event["type"].as_str(), Some("m.room.encrypted" | "m.room.message"))
                });
                if spoke && self.rooms().told.insert(room.clone()) {
                    let notice = json!({ "msgtype": "m.notice", "body": wire::ENCRYPTED_ROOM });
                    if let Err(error) = self.send(room, "m.room.message", &notice).await {
                        tracing::debug!(error = %error, "an encrypted matrix room could not be told");
                    }
                }
                continue;
            }
            if self.rooms().groups.contains(room) {
                continue;
            }
            for event in others {
                let Some(inbound) = self.inbound(room, event).await else { continue };
                let card = wire::reaction(event).map(|(card, _)| card);
                let files = wire::image(event).into_iter().collect();
                let carried = Carried { card, files };
                if sender.send(Update { inbound: Some(inbound), press: Some(carried) }).is_err() {
                    return;
                }
            }
        }
    }

    /// Sends `content` as an event of `kind` to `room`; returns its event id.
    async fn send(&self, room: &str, kind: &str, content: &Value) -> adk_kernel::Result<String> {
        let txn = uuid::Uuid::new_v4().simple().to_string();
        // A rate limit on a reply to an account that is not paired is not waited out.
        let patient = patient(MATRIX_CHANNEL, room);
        let path = ["rooms", room, "send", kind, txn.as_str()];
        let sent =
            self.inner.api.call(Method::PUT, &path, &[], Some(content), CALL, patient).await?;
        Ok(sent["event_id"].as_str().unwrap_or_default().to_string())
    }
}

#[async_trait]
impl Taker for MatrixAdapter {
    /// The card a reaction annotates.
    type Press = Carried;

    fn channel_name(&self) -> &'static str {
        MATRIX_CHANNEL
    }

    /// Hands one event to the gateway; a reaction counts only from the account paired from
    /// the card's room.
    async fn take(
        &self,
        gateway: &ChannelGateway,
        update: &Update<Carried>,
        paired: bool,
    ) -> adk_kernel::Result<()> {
        let Some(mut inbound) = update.inbound.clone() else { return Ok(()) };
        let carried = update.press.as_ref();
        // A paired account's images, for a host that takes them; a stranger's are never
        // downloaded (spec 3.5 §10 X12).
        let files = carried.map_or(&[][..], |carried| carried.files.as_slice());
        if paired && !files.is_empty() {
            let api = &self.inner.api;
            let attachments =
                crate::channels::media::fetch_all(files, |file| api.download(file)).await;
            inbound = inbound.with_attachments(attachments);
        }
        let card = match &inbound.body {
            InboundBody::Answer { decision, .. } => {
                match paired_binding(gateway, MATRIX_CHANNEL, &inbound.account).await {
                    Ok(Some(binding)) if binding.thread == inbound.thread => {}
                    Ok(_) => {
                        tracing::debug!(
                            "a matrix reaction from an account not paired from its room was ignored"
                        );
                        return Ok(());
                    }
                    Err(()) => {
                        return Err(KernelError::Core(AdkError::new(
                            ErrorComponent::Server,
                            ErrorCategory::Unavailable,
                            "surface.matrix.pairing_unread",
                            "the account's pairing could not be read",
                        )));
                    }
                }
                let card = carried.and_then(|carried| carried.card.clone());
                Some(PressedCard { card, decision: decision.clone() })
            }
            _ => None,
        };
        let handling = Handling::of(MATRIX_CHANNEL, gateway, &inbound, paired, card);
        HANDLING.scope(handling, gateway.receive(inbound)).await.map(drop)
    }

    async fn not_taken(&self, update: &Update<Carried>) {
        let Some(inbound) = &update.inbound else { return };
        let pressed = update.press.as_ref().is_some_and(|carried| carried.card.is_some());
        let text = if pressed { ANSWER_NOT_TAKEN } else { MESSAGE_NOT_TAKEN };
        for content in wire::plain(text) {
            if let Err(error) = self.send(&inbound.thread, "m.room.message", &content).await {
                tracing::debug!(error = %error, "a person could not be told their message was not taken");
            }
        }
    }
}

#[async_trait]
impl ChannelSink for MatrixAdapter {
    fn channel(&self) -> &str {
        MATRIX_CHANNEL
    }

    async fn send_message(&self, thread: &str, text: &str) -> adk_kernel::Result<()> {
        for content in wire::plain(text) {
            self.send(thread, "m.room.message", &content).await?;
        }
        Ok(())
    }

    /// Sends the card, remembers what it answers, and puts the adapter's own reaction with
    /// each answer key on it, so the person has only to tap.
    async fn send_decision(&self, thread: &str, card: &DecisionCard) -> adk_kernel::Result<()> {
        let (heading, card) = self.inner.kit.prepare(card).await;
        let (content, answers) = wire::card(&card, &heading, self.inner.kit.offset);
        let event = self.send(thread, "m.room.message", &content).await?;
        let Some(answers) = answers.filter(|_| !event.is_empty()) else { return Ok(()) };
        let keys: Vec<String> = answers.answers.iter().map(|(key, _)| key.clone()).collect();
        self.rooms().keep(event.clone(), answers);
        for key in keys {
            if let Err(error) = self.send(thread, "m.reaction", &wire::offer(&event, &key)).await {
                tracing::debug!(error = %error, "a card's answer key could not be offered");
            }
        }
        Ok(())
    }

    async fn send_stopped(&self, thread: &str, notice: &StopNotice) -> adk_kernel::Result<()> {
        self.send(thread, "m.room.message", &wire::message(&stopped_words(notice))).await.map(drop)
    }

    /// A notice about an answered card records the answer with where it was given before the
    /// confirmation is sent, as a reply to the card (spec 3.3 §8.1, §11).
    async fn send_notice(&self, thread: &str, notice: &ChannelNotice) -> adk_kernel::Result<()> {
        let card = self.inner.kit.before_notice(MATRIX_CHANNEL, MATRIX, thread, notice).await;
        let words = notice_words(notice, self.inner.kit.offset, Utc::now(), card.is_some());
        let mut content = wire::message(&words);
        if let Some(card) = card.filter(|card| !card.is_empty()) {
            content["m.relates_to"] = json!({ "m.in_reply_to": { "event_id": card } });
        }
        self.send(thread, "m.room.message", &content).await.map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_say_what_to_do_and_keep_their_category() {
        let error: AdkError = MatrixError::Unauthorized.into();
        assert_eq!(
            (error.code, error.category),
            ("surface.matrix.unauthorized", ErrorCategory::Unauthorized)
        );
        let forbidden = MatrixError::Refused {
            endpoint: "rooms".into(),
            status: 403,
            errcode: "M_FORBIDDEN".into(),
            error: "not in room".into(),
        };
        assert_eq!(AdkError::from(forbidden).category, ErrorCategory::Forbidden);
    }

    #[test]
    fn the_builder_refuses_a_bad_token_or_address_without_echoing_the_token() {
        let error =
            MatrixAdapter::builder("http://matrix.example.org", "syt_secret").build().unwrap_err();
        assert!(!error.to_string().contains("secret"), "{error}");
        assert!(MatrixAdapter::builder("https://matrix.example.org", "").build().is_err());
        let adapter =
            MatrixAdapter::builder("https://matrix.example.org", "syt_secret").build().unwrap();
        assert!(!format!("{adapter:?}").contains("secret"));
        assert_eq!(ChannelSink::channel(&adapter), "matrix");
    }

    #[test]
    fn the_adapter_remembers_its_newest_cards() {
        let mut rooms = Rooms::default();
        let card = CardRef { decision: "d".into(), version: 1, answers: Vec::new() };
        for index in 0..CARDS_KEPT + 3 {
            rooms.keep(format!("$e{index}"), card.clone());
        }
        assert_eq!(rooms.cards.len(), CARDS_KEPT);
        assert!(
            !rooms.cards.contains_key("$e0")
                && rooms.cards.contains_key(&format!("$e{}", CARDS_KEPT + 2))
        );
    }
}
