//! The Discord adapter (spec 3.5 §10 X12, on spec 3.3 §5.3): the Gateway in, REST out.

mod api;
mod wire;

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use adk_core::{AdkError, ErrorCategory, ErrorComponent};
use adk_kernel::KernelError;
use adk_kernel::channels::{
    ChannelGateway, ChannelInbound, ChannelNotice, ChannelSink, DecisionCard, InboundBody,
    InboundOutcome, StopNotice,
};
use async_trait::async_trait;
use chrono::Utc;
use futures::{SinkExt, StreamExt};
use reqwest::Method;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use super::shared::{
    ANSWER_NOT_TAKEN, AbortOnDrop, ChannelConnection, HANDLING, Handling, Kit, MESSAGE_NOT_TAKEN,
    PressedCard, REQUEST_TIMEOUT, Taker, Update, checked_base, dispatch, kit_builder, notice_words,
    patient, stopped_words,
};
use crate::error::{Result, SurfaceError};
use api::DiscordApi;
use wire::{Interaction, Press};

/// The channel the Discord adapter serves, as the kernel names it: pairings, thread sessions
/// and `kernel.channel` audit records carry it (spec 3.5 §10 X12).
pub const DISCORD_CHANNEL: &str = "discord";

/// Discord as a person reads it, in "You said yes in Discord at 07:31" (spec 3.3 §8.1).
pub const DISCORD: &str = "Discord";

const DEFAULT_API_BASE: &str = "https://discord.com/api/v10";
/// Direct messages (`DIRECT_MESSAGES`): their content needs no privileged intent.
const INTENTS: u64 = 1 << 12;
const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// Why the Discord adapter could not talk to Discord (spec 3.5 §10 X12).
///
/// Never carries the bot token. Converts into [`AdkError`] with a `surface.discord.*` code
/// and provider `discord`.
///
/// # Example
///
/// ```rust
/// use adk_surface::channels::DiscordError;
///
/// let error = DiscordError::Unauthorized;
/// assert_eq!(error.code(), "surface.discord.unauthorized");
/// assert!(error.to_string().contains("DISCORD_BOT_TOKEN"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum DiscordError {
    /// Discord refused the bot token.
    #[error(
        "Discord refused the bot token: check DISCORD_BOT_TOKEN, or reset it in the Developer \
         Portal (Bot → Reset Token)"
    )]
    Unauthorized,
    /// Discord asked the bot to slow down for longer than the adapter waits.
    #[error(
        "Discord asked to wait {retry_after_ms} ms before {route}: the bot is sending too much"
    )]
    RateLimited {
        /// The REST route.
        route: String,
        /// How long Discord asked to wait, in milliseconds.
        retry_after_ms: u64,
    },
    /// Discord refused the call, such as a user who does not accept direct messages.
    #[error("Discord refused {route} (HTTP {status}, code {code}): {message}")]
    Refused {
        /// The REST route.
        route: String,
        /// The HTTP status.
        status: u16,
        /// Discord's JSON error code.
        code: i64,
        /// Discord's message.
        message: String,
    },
    /// The Gateway closed the connection for good.
    #[error(
        "the Discord Gateway closed the connection ({code}): {reason}; check the bot's intents \
         in the Developer Portal"
    )]
    Gateway {
        /// The close code, such as 4014 for disallowed intents.
        code: u16,
        /// Discord's reason.
        reason: String,
    },
    /// Discord could not be reached.
    #[error(
        "Discord could not be reached for {route}: {reason}; check the network and the API address"
    )]
    Unreachable {
        /// The REST route, or `gateway`.
        route: String,
        /// What failed.
        reason: String,
    },
    /// Discord's answer was not the API's JSON.
    #[error("Discord's answer to {route} could not be read: {reason}")]
    InvalidResponse {
        /// The REST route.
        route: String,
        /// What was wrong with it.
        reason: String,
    },
}

impl DiscordError {
    /// The error code: `surface.discord.unauthorized`, `.rate_limited`, `.refused`,
    /// `.gateway`, `.unreachable` or `.invalid_response`.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unauthorized => "surface.discord.unauthorized",
            Self::RateLimited { .. } => "surface.discord.rate_limited",
            Self::Refused { .. } => "surface.discord.refused",
            Self::Gateway { .. } => "surface.discord.gateway",
            Self::Unreachable { .. } => "surface.discord.unreachable",
            Self::InvalidResponse { .. } => "surface.discord.invalid_response",
        }
    }
}

impl From<DiscordError> for AdkError {
    fn from(error: DiscordError) -> Self {
        let category = match &error {
            DiscordError::Unauthorized => ErrorCategory::Unauthorized,
            DiscordError::RateLimited { .. } => ErrorCategory::RateLimited,
            DiscordError::Refused { status: 403, .. } => ErrorCategory::Forbidden,
            DiscordError::Refused { status: 404, .. } => ErrorCategory::NotFound,
            DiscordError::Refused { status: 400, .. } => ErrorCategory::InvalidInput,
            DiscordError::Refused { .. } | DiscordError::Unreachable { .. } => {
                ErrorCategory::Unavailable
            }
            DiscordError::Gateway { .. } => ErrorCategory::Forbidden,
            DiscordError::InvalidResponse { .. } => ErrorCategory::Internal,
        };
        AdkError::new(ErrorComponent::Server, category, error.code(), error.to_string())
            .with_provider("discord")
    }
}

impl From<DiscordError> for KernelError {
    fn from(error: DiscordError) -> Self {
        Self::Core(error.into())
    }
}

/// The Gateway connection a [`DiscordAdapter::start`] runs; dropping it stops it.
pub type DiscordConnection = ChannelConnection<DiscordError>;

/// Builds a [`DiscordAdapter`].
pub struct DiscordAdapterBuilder {
    token: String,
    api_base: String,
    kit: Kit,
}

impl fmt::Debug for DiscordAdapterBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DiscordAdapterBuilder")
            .field("api_base", &self.api_base)
            .finish_non_exhaustive()
    }
}

impl DiscordAdapterBuilder {
    /// The REST API's address; defaults to `https://discord.com/api/v10`. Plain `http` is
    /// accepted on loopback only (a test double), since the token travels with every call.
    #[must_use]
    pub fn api_base(mut self, url: impl Into<String>) -> Self {
        self.api_base = url.into();
        self
    }

    kit_builder!("Discord");

    /// The adapter.
    ///
    /// # Errors
    ///
    /// Returns `surface.invalid` for an empty or malformed token, or an API address that is
    /// not `https` (or `http` on loopback).
    pub fn build(self) -> Result<DiscordAdapter> {
        let api = DiscordApi::new(&self.api_base, &self.token).map_err(SurfaceError::Invalid)?;
        Ok(DiscordAdapter { inner: Arc::new(Inner { api, kit: self.kit }) })
    }
}

/// The Discord adapter (spec 3.5 §10 X12): a bot's direct messages as the person's chat
/// thread with their agents, over the kernel's channel contract (spec 3.3 §10 K1).
///
/// It is the gateway's [`ChannelSink`] for the channel [`DISCORD_CHANNEL`], and
/// [`start`](Self::start) reads the bot's events over the Gateway — a WebSocket the adapter
/// opens, so no inbound port — identifying, keeping the heartbeat, resuming the session after
/// a reconnect, and hands each to [`ChannelGateway::receive`].
///
/// | Direction | What |
/// |---|---|
/// | Out | A card: "Waiting for you · since 18:00", the call in plain words, the agent and the risk, a button for each answer the card offers and no others, and "Open in home" — never the call's arguments (S-I5). Every message escapes Discord's markdown and sends `allowed_mentions: {"parse": []}` (S-I9). A `429` is waited out per `retry_after`, never for a stranger |
/// | In | New messages from a person in a direct message with the bot (`MESSAGE_CREATE`), and pressed buttons (`INTERACTION_CREATE`), each interaction answered through its callback at once; servers, bots and edits are ignored. Typed text never answers a decision (S-I4) and loses the characters a chat app shows as nothing (S-I6) |
/// | Commands | `/stop`, declared as the bot's application command, and "stop all" stop everything; resuming is refused (S-I10) |
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use adk_kernel::channels::{ChannelDoorbell, ChannelGateway, InMemoryChannelBindingStore};
/// use adk_surface::channels::DiscordAdapter;
///
/// # async fn example(kernel: adk_kernel::Kernel, doorbell: Arc<ChannelDoorbell>) -> Result<(), Box<dyn std::error::Error>> {
/// let discord = DiscordAdapter::builder(std::env::var("DISCORD_BOT_TOKEN")?).build()?;
/// let gateway = ChannelGateway::builder(Arc::new(kernel), Arc::new(InMemoryChannelBindingStore::new()))
///     .sink(Arc::new(discord.clone()))
///     .home_url("http://127.0.0.1:8080/ui/")
///     .doorbell(doorbell)
///     .build();
/// gateway.start().await?;
/// let connection = discord.start(gateway);
/// connection.join().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct DiscordAdapter {
    inner: Arc<Inner>,
}

struct Inner {
    api: DiscordApi,
    kit: Kit,
}

impl fmt::Debug for DiscordAdapter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DiscordAdapter").field("channel", &DISCORD_CHANNEL).finish_non_exhaustive()
    }
}

/// A Gateway session the adapter resumes after a reconnect.
#[derive(Debug, Clone)]
struct Session {
    id: String,
    resume_url: String,
    seq: Option<u64>,
}

/// Why a Gateway connection ended.
enum Ended {
    /// Connect again and resume the session.
    Resume,
    /// The session is gone: connect again and identify.
    Fresh,
    /// The connection failed; connect again after a wait and resume.
    Failed(String),
}

/// What an event carries beyond its inbound value.
pub(crate) struct Event {
    press: Option<Press>,
    interaction: Option<Interaction>,
    files: Vec<crate::channels::media::FileRef>,
}

tokio::task_local! {
    /// The `/stop` command being handled, whose deferred reply the first message to its
    /// thread becomes.
    static DEFERRED: Arc<Deferred>;
}

struct Deferred {
    interaction: Interaction,
    thread: String,
    used: AtomicBool,
}

impl DiscordAdapter {
    /// An adapter for the bot with `token`, from the Developer Portal.
    pub fn builder(token: impl Into<String>) -> DiscordAdapterBuilder {
        DiscordAdapterBuilder {
            token: token.into(),
            api_base: DEFAULT_API_BASE.to_string(),
            kit: Kit::default(),
        }
    }

    /// What the adapter makes of one Gateway dispatch, `{"op": 0, "t": …, "d": …}` (spec 3.3
    /// S-I4, S-I6): a typed message, a pressed answer, `/stop`, or `None` for anything the
    /// gateway is not given.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_kernel::channels::InboundBody;
    /// use adk_surface::channels::DiscordAdapter;
    ///
    /// let discord = DiscordAdapter::builder("example-token").build().unwrap();
    /// let dispatch = serde_json::json!({
    ///     "op": 0, "s": 4, "t": "MESSAGE_CREATE",
    ///     "d": {
    ///         "id": "1300000000000000001", "channel_id": "1300000000000000002", "type": 0,
    ///         "content": "stop all",
    ///         "author": { "id": "1300000000000000003", "username": "ana" },
    ///     },
    /// });
    /// let inbound = discord.inbound(&dispatch).unwrap();
    /// assert_eq!(inbound.body, InboundBody::Text { text: "stop all".into() });
    /// assert_eq!(inbound.thread, "1300000000000000002");
    /// ```
    pub fn inbound(&self, dispatch: &Value) -> Option<ChannelInbound> {
        wire::parse(dispatch).inbound
    }

    /// Connects to the Gateway and hands each event to `gateway`, which must have this adapter
    /// as its sink, until [`DiscordConnection::stop`].
    ///
    /// Each interaction is answered through its callback as it arrives. A paired account's
    /// events go to the gateway first, each account's in the order sent (S-I6, S-I10); one
    /// the gateway fails on is taken again only when it is a paired account's and the failure
    /// may clear. When Discord asks for a reconnect, the heartbeat goes unanswered or the
    /// socket fails, the adapter connects again and resumes the session, identifying anew
    /// when Discord says it cannot be resumed. A refused token ends the connection with
    /// [`DiscordError::Unauthorized`], and disallowed intents with [`DiscordError::Gateway`].
    pub fn start(&self, gateway: ChannelGateway) -> DiscordConnection {
        let adapter = self.clone();
        ChannelConnection::spawn(async move { adapter.run(gateway).await })
    }

    async fn run(&self, gateway: ChannelGateway) -> std::result::Result<(), DiscordError> {
        let (sender, task) = dispatch(Arc::new(self.clone()), gateway);
        let _dispatcher = AbortOnDrop(task);
        let mut session: Option<Session> = None;
        let mut backoff = MIN_BACKOFF;
        loop {
            let url = match &session {
                Some(session) => session.resume_url.clone(),
                None => match self.inner.api.gateway_url().await {
                    Ok(url) => url,
                    Err(DiscordError::Unauthorized) => return Err(DiscordError::Unauthorized),
                    Err(error) => {
                        tracing::warn!(error = %error, "the discord gateway could not be found; retrying");
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(MAX_BACKOFF);
                        continue;
                    }
                },
            };
            match self.serve(&url, &mut session, &sender).await? {
                Ended::Resume => backoff = MIN_BACKOFF,
                Ended::Fresh => {
                    session = None;
                    tokio::time::sleep(MIN_BACKOFF).await;
                }
                Ended::Failed(reason) => {
                    tracing::warn!(reason = %reason, "the discord gateway connection ended; connecting again");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                }
            }
        }
    }

    /// Reads one Gateway connection until it ends.
    async fn serve(
        &self,
        url: &str,
        session: &mut Option<Session>,
        sender: &mpsc::UnboundedSender<Update<Event>>,
    ) -> std::result::Result<Ended, DiscordError> {
        let url = format!("{}/?v=10&encoding=json", url.trim_end_matches('/'));
        if let Err(reason) = checked_base(&url, "the Discord Gateway address", true) {
            return Ok(Ended::Failed(reason));
        }
        adk_core::ensure_crypto_provider();
        let connect = tokio_tungstenite::connect_async(url.as_str());
        let mut socket = match tokio::time::timeout(REQUEST_TIMEOUT, connect).await {
            Ok(Ok((socket, _))) => socket,
            Ok(Err(error)) => return Ok(Ended::Failed(error.to_string())),
            Err(_) => return Ok(Ended::Failed("the gateway did not open in time".into())),
        };
        // Hello first: how often to beat.
        let hello = tokio::time::timeout(REQUEST_TIMEOUT, socket.next()).await;
        let interval = match hello {
            Ok(Some(Ok(Message::Text(text)))) => serde_json::from_str::<Value>(text.as_str())
                .ok()
                .filter(|hello| hello["op"] == 10)
                .and_then(|hello| hello["d"]["heartbeat_interval"].as_u64()),
            _ => None,
        };
        let Some(interval) = interval.filter(|interval| *interval > 0) else {
            return Ok(Ended::Failed("the gateway did not say hello".into()));
        };
        let token = self.inner.api.token();
        let mut seq = session.as_ref().and_then(|session| session.seq);
        let opening = match session.as_ref() {
            Some(session) => {
                json!({ "op": 6, "d": { "token": token, "session_id": session.id, "seq": seq } })
            }
            None => json!({ "op": 2, "d": {
                "token": token,
                "intents": INTENTS,
                "properties": { "os": std::env::consts::OS, "browser": "adk-rust", "device": "adk-rust" },
            }}),
        };
        if let Err(error) = socket.send(Message::Text(opening.to_string().into())).await {
            return Ok(Ended::Failed(error.to_string()));
        }
        // The first beat comes after a random part of the interval, as Discord asks.
        let jitter = u64::try_from(uuid::Uuid::new_v4().as_u128() % 1000).unwrap_or(500);
        let period = Duration::from_millis(interval);
        let first = tokio::time::Instant::now() + period.mul_f64(jitter as f64 / 1000.0);
        let mut beat = tokio::time::interval_at(first, period);
        let mut acked = true;
        loop {
            let frame = tokio::select! {
                _ = beat.tick() => {
                    if !acked {
                        return Ok(Ended::Failed("the gateway did not answer the heartbeat".into()));
                    }
                    acked = false;
                    let heartbeat = json!({ "op": 1, "d": seq }).to_string();
                    if let Err(error) = socket.send(Message::Text(heartbeat.into())).await {
                        return Ok(Ended::Failed(error.to_string()));
                    }
                    continue;
                }
                frame = socket.next() => frame,
            };
            let text = match frame {
                Some(Ok(Message::Text(text))) => text,
                Some(Ok(Message::Close(frame))) => {
                    let (code, reason) = frame
                        .map(|frame| (u16::from(frame.code), frame.reason.to_string()))
                        .unwrap_or((1000, String::new()));
                    return match code {
                        4004 => Err(DiscordError::Unauthorized),
                        4010..=4014 => Err(DiscordError::Gateway { code, reason }),
                        4007 | 4009 => Ok(Ended::Fresh),
                        _ => Ok(Ended::Failed(format!("closed ({code})"))),
                    };
                }
                None => return Ok(Ended::Failed("the gateway closed".into())),
                Some(Ok(_)) => continue,
                Some(Err(error)) => return Ok(Ended::Failed(error.to_string())),
            };
            let Ok(payload) = serde_json::from_str::<Value>(text.as_str()) else { continue };
            match payload["op"].as_u64() {
                Some(0) => {
                    if let Some(s) = payload["s"].as_u64() {
                        seq = Some(s);
                        if let Some(session) = session.as_mut() {
                            session.seq = seq;
                        }
                    }
                    match payload["t"].as_str() {
                        Some("READY") => {
                            let d = &payload["d"];
                            *session = Some(Session {
                                id: d["session_id"].as_str().unwrap_or_default().to_string(),
                                resume_url: d["resume_gateway_url"]
                                    .as_str()
                                    .unwrap_or(&url)
                                    .to_string(),
                                seq,
                            });
                            if let Some(application) = d["application"]["id"].as_str() {
                                self.declare_commands(application);
                            }
                        }
                        Some("RESUMED") => tracing::debug!("the discord session resumed"),
                        _ => self.dispatched(&payload, sender).await,
                    }
                }
                Some(1) => {
                    let heartbeat = json!({ "op": 1, "d": seq }).to_string();
                    if let Err(error) = socket.send(Message::Text(heartbeat.into())).await {
                        return Ok(Ended::Failed(error.to_string()));
                    }
                }
                Some(7) => return Ok(Ended::Resume),
                Some(9) if payload["d"].as_bool() == Some(true) => return Ok(Ended::Resume),
                Some(9) => return Ok(Ended::Fresh),
                Some(11) => acked = true,
                _ => {}
            }
        }
    }

    /// Answers a dispatch's interaction through its callback at once, then hands what it
    /// carries over.
    async fn dispatched(&self, payload: &Value, sender: &mpsc::UnboundedSender<Update<Event>>) {
        let parsed = wire::parse(payload);
        if let Some(interaction) = &parsed.interaction {
            let taken = parsed.inbound.is_some();
            let answer = match (interaction.command, taken) {
                // The deferred reply becomes the stop notice.
                (true, true) => json!({ "type": 5 }),
                (true, false) => json!({ "type": 4, "data": {
                    "content": "This works in a direct message with the bot.",
                    "flags": 64,
                    "allowed_mentions": { "parse": [] },
                }}),
                (false, _) => json!({ "type": 6 }),
            };
            let route = format!("/interactions/{}/{}/callback", interaction.id, interaction.token);
            if let Err(error) =
                self.inner.api.request(Method::POST, &route, Some(&answer), false).await
            {
                tracing::debug!(error = %error, "a discord interaction could not be answered");
            }
        }
        if parsed.inbound.is_some() {
            let event =
                Event { press: parsed.press, interaction: parsed.interaction, files: parsed.files };
            let _ = sender.send(Update { inbound: parsed.inbound, press: Some(event) });
        }
    }

    /// Declares `/stop` as the bot's application command, as Telegram's adapter lists it.
    fn declare_commands(&self, application: &str) {
        let adapter = self.clone();
        let route = format!("/applications/{application}/commands");
        tokio::spawn(async move {
            let commands = wire::commands();
            if let Err(error) =
                adapter.inner.api.request(Method::PUT, &route, Some(&commands), true).await
            {
                tracing::warn!(error = %error, "the bot's /stop command could not be declared");
            }
        });
    }

    /// Takes the answer buttons off an answered card, so it is not pressed again.
    async fn clear_buttons(&self, press: &Press) {
        let Some((channel, message)) = &press.card else { return };
        if !id_like(channel) || !id_like(message) {
            return;
        }
        let route = format!("/channels/{channel}/messages/{message}");
        let body = wire::answered(press);
        if let Err(error) = self.inner.api.request(Method::PATCH, &route, Some(&body), true).await {
            tracing::debug!(error = %error, "an answered card's buttons could not be removed");
        }
    }

    async fn post(&self, thread: &str, body: Value) -> adk_kernel::Result<()> {
        // A `/stop` in hand: its deferred reply says what happened.
        if let Ok(deferred) = DEFERRED.try_with(Arc::clone)
            && deferred.thread == thread
            && !deferred.used.swap(true, Ordering::SeqCst)
        {
            let Interaction { application, token, .. } = &deferred.interaction;
            let route = format!("/webhooks/{application}/{token}/messages/@original");
            self.inner.api.request(Method::PATCH, &route, Some(&body), true).await?;
            return Ok(());
        }
        if !id_like(thread) {
            return Err(DiscordError::Refused {
                route: "/channels".into(),
                status: 400,
                code: 0,
                message: "the thread is not a Discord channel id".into(),
            }
            .into());
        }
        // A rate limit on a reply to an account that is not paired is not waited out.
        let patient = patient(DISCORD_CHANNEL, thread);
        let route = format!("/channels/{thread}/messages");
        self.inner.api.request(Method::POST, &route, Some(&body), patient).await?;
        Ok(())
    }
}

/// Whether `id` can stand in a route: Discord's snowflakes, and the names the contract
/// suite gives its threads.
fn id_like(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

#[async_trait]
impl Taker for DiscordAdapter {
    type Press = Event;

    fn channel_name(&self) -> &'static str {
        DISCORD_CHANNEL
    }

    /// Hands one event to the gateway; an answered card loses its answer buttons, and a
    /// `/stop` whose deferred reply said nothing has it removed.
    async fn take(
        &self,
        gateway: &ChannelGateway,
        update: &Update<Event>,
        paired: bool,
    ) -> adk_kernel::Result<()> {
        let Some(mut inbound) = update.inbound.clone() else { return Ok(()) };
        let event = update.press.as_ref();
        // A paired account's images, for a host that takes them; a stranger's are never
        // downloaded (spec 3.5 §10 X12).
        let files = event.map_or(&[][..], |event| event.files.as_slice());
        if paired && !files.is_empty() {
            let api = &self.inner.api;
            let attachments =
                crate::channels::media::fetch_all(files, |file| api.download(file)).await;
            inbound = inbound.with_attachments(attachments);
        }
        let press = event.and_then(|event| event.press.as_ref());
        let card = match (&inbound.body, press) {
            (InboundBody::Answer { decision, .. }, Some(press)) => Some(PressedCard {
                card: press.card.as_ref().map(|(_, message)| message.clone()),
                decision: decision.clone(),
            }),
            _ => None,
        };
        let command = event.and_then(|event| event.interaction.clone()).filter(|i| i.command);
        let handling = Handling::of(DISCORD_CHANNEL, gateway, &inbound, paired, card);
        let thread = inbound.thread.clone();
        let receive = HANDLING.scope(handling, gateway.receive(inbound));
        let outcome = match command {
            Some(interaction) => {
                let deferred =
                    Arc::new(Deferred { interaction, thread, used: AtomicBool::new(false) });
                let outcome = DEFERRED.scope(deferred.clone(), receive).await;
                if !deferred.used.load(Ordering::SeqCst) {
                    let Interaction { application, token, .. } = &deferred.interaction;
                    let route = format!("/webhooks/{application}/{token}/messages/@original");
                    let _ = self.inner.api.request(Method::DELETE, &route, None, false).await;
                }
                outcome
            }
            None => receive.await,
        };
        let Some(press) = press else { return outcome.map(drop) };
        if matches!(
            outcome?,
            InboundOutcome::Decided { .. }
                | InboundOutcome::AlreadyDecided { .. }
                | InboundOutcome::DecisionChanged { .. }
                | InboundOutcome::DecisionRefused { .. }
        ) {
            self.clear_buttons(press).await;
        }
        Ok(())
    }

    async fn not_taken(&self, update: &Update<Event>) {
        let Some(inbound) = &update.inbound else { return };
        let answered = update.press.as_ref().is_some_and(|event| event.press.is_some());
        let text = if answered { ANSWER_NOT_TAKEN } else { MESSAGE_NOT_TAKEN };
        for body in wire::plain(text) {
            if let Err(error) = self.post(&inbound.thread, body).await {
                tracing::debug!(error = %error, "a person could not be told their message was not taken");
            }
        }
    }
}

#[async_trait]
impl ChannelSink for DiscordAdapter {
    fn channel(&self) -> &str {
        DISCORD_CHANNEL
    }

    async fn send_message(&self, thread: &str, text: &str) -> adk_kernel::Result<()> {
        for body in wire::plain(text) {
            self.post(thread, body).await?;
        }
        Ok(())
    }

    async fn send_decision(&self, thread: &str, card: &DecisionCard) -> adk_kernel::Result<()> {
        let (heading, card) = self.inner.kit.prepare(card).await;
        self.post(thread, wire::card(&card, &heading, self.inner.kit.offset)).await
    }

    async fn send_stopped(&self, thread: &str, notice: &StopNotice) -> adk_kernel::Result<()> {
        self.post(thread, wire::message(&stopped_words(notice))).await
    }

    /// A notice about a pressed card records the answer with where it was given before the
    /// confirmation is sent, as a reply to the card (spec 3.3 §8.1, §11).
    async fn send_notice(&self, thread: &str, notice: &ChannelNotice) -> adk_kernel::Result<()> {
        let card = self.inner.kit.before_notice(DISCORD_CHANNEL, DISCORD, thread, notice).await;
        let words = notice_words(notice, self.inner.kit.offset, Utc::now(), card.is_some());
        let mut body = wire::message(&words);
        if let Some(card) = card {
            body["message_reference"] = json!({ "message_id": card, "fail_if_not_exists": false });
        }
        self.post(thread, body).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_say_what_to_do_and_keep_their_category() {
        let error: AdkError = DiscordError::Unauthorized.into();
        assert_eq!(
            (error.code, error.category),
            ("surface.discord.unauthorized", ErrorCategory::Unauthorized)
        );
        let closed = DiscordError::Gateway { code: 4014, reason: "Disallowed intent(s).".into() };
        assert!(closed.to_string().contains("intents"));
        let limited =
            DiscordError::RateLimited { route: "/channels".into(), retry_after_ms: 90_000 };
        assert!(AdkError::from(limited).is_retryable());
    }

    #[test]
    fn the_builder_refuses_a_bad_token_or_address_without_echoing_the_token() {
        let error = DiscordAdapter::builder("secret-token")
            .api_base("http://example.com")
            .build()
            .unwrap_err();
        assert!(!error.to_string().contains("secret"), "{error}");
        assert!(DiscordAdapter::builder("").build().is_err());
        let adapter = DiscordAdapter::builder("secret-token").build().unwrap();
        assert!(!format!("{adapter:?}").contains("secret"));
        assert_eq!(ChannelSink::channel(&adapter), "discord");
        assert!(
            id_like("1300000000000000002") && id_like("t-ana") && !id_like("../x") && !id_like("")
        );
    }
}
