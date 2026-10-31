//! The Slack adapter (spec 3.5 §10 X12, on spec 3.3 §5.3): Socket Mode in, the Web API out.

mod api;
mod wire;

use std::fmt;
use std::sync::Arc;
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
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use super::shared::{
    ANSWER_NOT_TAKEN, AbortOnDrop, ChannelConnection, HANDLING, Handling, Kit, MESSAGE_NOT_TAKEN,
    PressedCard, REQUEST_TIMEOUT, Taker, Update, checked_base, dispatch, kit_builder, notice_words,
    patient, stopped_words,
};
use crate::error::{Result, SurfaceError};
use api::SlackApi;
use wire::Press;

/// The channel the Slack adapter serves, as the kernel names it: pairings, thread sessions
/// and `kernel.channel` audit records carry it (spec 3.5 §10 X12).
pub const SLACK_CHANNEL: &str = "slack";

/// Slack as a person reads it, in "You said yes in Slack at 07:31" (spec 3.3 §8.1).
pub const SLACK: &str = "Slack";

const DEFAULT_API_BASE: &str = "https://slack.com/api";
const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// How often the adapter pings the socket, and how long it waits for any frame before it
/// connects again.
const PING_EVERY: Duration = Duration::from_secs(30);
const SILENT_FOR: Duration = Duration::from_secs(90);

/// Why the Slack adapter could not talk to Slack (spec 3.5 §10 X12).
///
/// Never carries a token. Converts into [`AdkError`] with a `surface.slack.*` code and
/// provider `slack`.
///
/// # Example
///
/// ```rust
/// use adk_surface::channels::SlackError;
///
/// let error = SlackError::Unauthorized;
/// assert_eq!(error.code(), "surface.slack.unauthorized");
/// assert!(error.to_string().contains("SLACK_APP_TOKEN"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SlackError {
    /// Slack refused a token.
    #[error(
        "Slack refused a token: check SLACK_APP_TOKEN (an app-level token with \
         `connections:write`) and SLACK_BOT_TOKEN (the bot token, `xoxb-`)"
    )]
    Unauthorized,
    /// Slack asked the app to slow down for longer than the adapter waits.
    #[error("Slack asked to wait {retry_after} s before {method}: the app is sending too much")]
    RateLimited {
        /// The Web API method.
        method: String,
        /// How long Slack asked to wait, in seconds.
        retry_after: u64,
    },
    /// Slack refused the call, such as a conversation the app is not in.
    #[error("Slack refused {method}: {error}")]
    Refused {
        /// The Web API method.
        method: String,
        /// Slack's error, such as `channel_not_found`.
        error: String,
    },
    /// Slack could not be reached.
    #[error(
        "Slack could not be reached for {method}: {reason}; check the network and the API address"
    )]
    Unreachable {
        /// The Web API method, or `socket`.
        method: String,
        /// What failed.
        reason: String,
    },
    /// Slack's answer was not the Web API's JSON.
    #[error("Slack's answer to {method} could not be read: {reason}")]
    InvalidResponse {
        /// The Web API method.
        method: String,
        /// What was wrong with it.
        reason: String,
    },
}

impl SlackError {
    /// The error code: `surface.slack.unauthorized`, `.rate_limited`, `.refused`,
    /// `.unreachable` or `.invalid_response`.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unauthorized => "surface.slack.unauthorized",
            Self::RateLimited { .. } => "surface.slack.rate_limited",
            Self::Refused { .. } => "surface.slack.refused",
            Self::Unreachable { .. } => "surface.slack.unreachable",
            Self::InvalidResponse { .. } => "surface.slack.invalid_response",
        }
    }
}

impl From<SlackError> for AdkError {
    fn from(error: SlackError) -> Self {
        let category = match &error {
            SlackError::Unauthorized => ErrorCategory::Unauthorized,
            SlackError::RateLimited { .. } => ErrorCategory::RateLimited,
            SlackError::Refused { error, .. } => match error.as_str() {
                "channel_not_found" | "not_in_channel" | "is_archived" | "cannot_dm_bot"
                | "restricted_action" | "missing_scope" => ErrorCategory::Forbidden,
                "invalid_arguments" | "invalid_blocks" | "msg_too_long" | "no_text" => {
                    ErrorCategory::InvalidInput
                }
                _ => ErrorCategory::Unavailable,
            },
            SlackError::Unreachable { .. } => ErrorCategory::Unavailable,
            SlackError::InvalidResponse { .. } => ErrorCategory::Internal,
        };
        AdkError::new(ErrorComponent::Server, category, error.code(), error.to_string())
            .with_provider("slack")
    }
}

impl From<SlackError> for KernelError {
    fn from(error: SlackError) -> Self {
        Self::Core(error.into())
    }
}

/// The Socket Mode connection a [`SlackAdapter::start`] runs; dropping it stops it.
pub type SlackConnection = ChannelConnection<SlackError>;

/// Builds a [`SlackAdapter`].
pub struct SlackAdapterBuilder {
    app_token: String,
    bot_token: String,
    api_base: String,
    kit: Kit,
}

impl fmt::Debug for SlackAdapterBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SlackAdapterBuilder")
            .field("api_base", &self.api_base)
            .finish_non_exhaustive()
    }
}

impl SlackAdapterBuilder {
    /// The Web API's address; defaults to `https://slack.com/api`. Plain `http` is accepted
    /// on loopback only (a test double), since the tokens travel with every call.
    #[must_use]
    pub fn api_base(mut self, url: impl Into<String>) -> Self {
        self.api_base = url.into();
        self
    }

    kit_builder!("Slack");

    /// The adapter.
    ///
    /// # Errors
    ///
    /// Returns `surface.invalid` for an empty or malformed token, or an API address that is
    /// not `https` (or `http` on loopback).
    pub fn build(self) -> Result<SlackAdapter> {
        let api = SlackApi::new(&self.api_base, &self.app_token, &self.bot_token)
            .map_err(SurfaceError::Invalid)?;
        Ok(SlackAdapter { inner: Arc::new(Inner { api, kit: self.kit }) })
    }
}

/// The Slack adapter (spec 3.5 §10 X12): a Slack app's direct messages as the person's chat
/// thread with their agents, over the kernel's channel contract (spec 3.3 §10 K1).
///
/// It is the gateway's [`ChannelSink`] for the channel [`SLACK_CHANNEL`], and
/// [`start`](Self::start) reads the app's events over Socket Mode — a WebSocket the adapter
/// opens, so no inbound port — and hands each to [`ChannelGateway::receive`]. Every envelope
/// is acknowledged with its `envelope_id` as it arrives.
///
/// | Direction | What |
/// |---|---|
/// | Out | A card as Block Kit: "Waiting for you · since 18:00", the call in plain words, the agent and the risk as plain text, a button for each answer the card offers and no others, and "Open in home" — never the call's arguments (S-I5). Agent replies with `&`, `<` and `>` escaped and `mrkdwn` off (S-I9) |
/// | In | New messages from a person in a direct message with the app, and pressed buttons (`block_actions`); channels, bots, edits and files are ignored. Typed text never answers a decision (S-I4) and loses the characters a chat app shows as nothing (S-I6) |
/// | Commands | `/stop` in the direct message, once the app declares it, and "stop all" stop everything; resuming is refused (S-I10) |
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use adk_kernel::channels::{ChannelDoorbell, ChannelGateway, InMemoryChannelBindingStore};
/// use adk_surface::channels::SlackAdapter;
///
/// # async fn example(kernel: adk_kernel::Kernel, doorbell: Arc<ChannelDoorbell>) -> Result<(), Box<dyn std::error::Error>> {
/// let slack = SlackAdapter::builder(std::env::var("SLACK_APP_TOKEN")?, std::env::var("SLACK_BOT_TOKEN")?).build()?;
/// let gateway = ChannelGateway::builder(Arc::new(kernel), Arc::new(InMemoryChannelBindingStore::new()))
///     .sink(Arc::new(slack.clone()))
///     .home_url("http://127.0.0.1:8080/ui/")
///     .doorbell(doorbell)
///     .build();
/// gateway.start().await?;
/// let connection = slack.start(gateway);
/// connection.join().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct SlackAdapter {
    inner: Arc<Inner>,
}

struct Inner {
    api: SlackApi,
    kit: Kit,
}

impl fmt::Debug for SlackAdapter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SlackAdapter").field("channel", &SLACK_CHANNEL).finish_non_exhaustive()
    }
}

/// What an envelope carries beyond its inbound value.
pub(crate) struct Carried {
    /// A pressed button's card.
    press: Option<Press>,
    /// The files a message shared.
    files: Vec<crate::channels::media::FileRef>,
}

/// Why a socket ended.
enum Ended {
    /// Slack asked for a new connection.
    Reconnect,
    /// The connection failed.
    Failed(String),
}

impl SlackAdapter {
    /// An adapter for the app with `app_token` (`xapp-…`, with `connections:write`) and
    /// `bot_token` (`xoxb-…`, with `chat:write` and `im:history`).
    pub fn builder(
        app_token: impl Into<String>,
        bot_token: impl Into<String>,
    ) -> SlackAdapterBuilder {
        SlackAdapterBuilder {
            app_token: app_token.into(),
            bot_token: bot_token.into(),
            api_base: DEFAULT_API_BASE.to_string(),
            kit: Kit::default(),
        }
    }

    /// What the adapter makes of one Socket Mode envelope (spec 3.3 S-I4, S-I6): a typed
    /// message, a pressed answer, `/stop`, or `None` for anything the gateway is not given.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_kernel::channels::InboundBody;
    /// use adk_surface::channels::SlackAdapter;
    ///
    /// let slack = SlackAdapter::builder("xapp-1-example", "xoxb-1-example").build().unwrap();
    /// let envelope = serde_json::json!({
    ///     "envelope_id": "e1",
    ///     "type": "events_api",
    ///     "payload": { "event": {
    ///         "type": "message", "channel_type": "im", "user": "U4217",
    ///         "channel": "D4217", "ts": "1761300000.000100", "text": "stop all",
    ///     }},
    /// });
    /// let inbound = slack.inbound(&envelope).unwrap();
    /// assert_eq!(inbound.body, InboundBody::Text { text: "stop all".into() });
    /// assert_eq!((inbound.account.as_str(), inbound.thread.as_str()), ("U4217", "D4217"));
    /// ```
    pub fn inbound(&self, envelope: &Value) -> Option<ChannelInbound> {
        wire::parse(envelope).inbound
    }

    /// Opens the app's Socket Mode connection and hands each event to `gateway`, which must
    /// have this adapter as its sink, until [`SlackConnection::stop`].
    ///
    /// Every envelope is acknowledged as it arrives. A paired account's events go to the
    /// gateway first, each account's in the order sent (S-I6, S-I10); one the gateway fails
    /// on is taken again only when it is a paired account's and the failure may clear.
    /// When Slack asks for a new connection (`disconnect`), or the socket goes quiet or
    /// closes, the adapter opens a new one; a refused token ends the connection with
    /// [`SlackError::Unauthorized`].
    pub fn start(&self, gateway: ChannelGateway) -> SlackConnection {
        let adapter = self.clone();
        ChannelConnection::spawn(async move { adapter.run(gateway).await })
    }

    async fn run(&self, gateway: ChannelGateway) -> std::result::Result<(), SlackError> {
        let (sender, task) = dispatch(Arc::new(self.clone()), gateway);
        let _dispatcher = AbortOnDrop(task);
        let mut backoff = MIN_BACKOFF;
        loop {
            let url = match self.inner.api.open_socket().await {
                Ok(url) => url,
                Err(SlackError::Unauthorized) => return Err(SlackError::Unauthorized),
                Err(error) => {
                    tracing::warn!(error = %error, "the slack socket could not be opened; retrying");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                    continue;
                }
            };
            match self.serve(&url, &sender).await {
                Ended::Reconnect => backoff = MIN_BACKOFF,
                Ended::Failed(reason) => {
                    tracing::warn!(reason = %reason, "the slack socket ended; connecting again");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                }
            }
        }
    }

    /// Reads one socket until Slack asks for another or it fails.
    async fn serve(&self, url: &str, sender: &mpsc::UnboundedSender<Update<Carried>>) -> Ended {
        if let Err(reason) = checked_base(url, "the Slack socket address", true) {
            return Ended::Failed(reason);
        }
        adk_core::ensure_crypto_provider();
        let connect = tokio_tungstenite::connect_async(url);
        let mut socket = match tokio::time::timeout(REQUEST_TIMEOUT, connect).await {
            Ok(Ok((socket, _))) => socket,
            Ok(Err(error)) => return Ended::Failed(error.to_string()),
            Err(_) => return Ended::Failed("the socket did not open in time".into()),
        };
        let mut ping = tokio::time::interval(PING_EVERY);
        ping.tick().await;
        let mut heard = tokio::time::Instant::now();
        loop {
            let frame = tokio::select! {
                _ = ping.tick() => {
                    if heard.elapsed() > SILENT_FOR {
                        return Ended::Failed("the socket went quiet".into());
                    }
                    if let Err(error) = socket.send(Message::Ping(Vec::new().into())).await {
                        return Ended::Failed(error.to_string());
                    }
                    continue;
                }
                frame = socket.next() => frame,
            };
            heard = tokio::time::Instant::now();
            let text = match frame {
                Some(Ok(Message::Text(text))) => text,
                Some(Ok(Message::Close(_))) | None => {
                    return Ended::Failed("the socket closed".into());
                }
                Some(Ok(_)) => continue,
                Some(Err(error)) => return Ended::Failed(error.to_string()),
            };
            let Ok(envelope) = serde_json::from_str::<Value>(text.as_str()) else { continue };
            match envelope["type"].as_str() {
                Some("hello") => continue,
                Some("disconnect") => return Ended::Reconnect,
                _ => {}
            }
            // Acknowledged at once, so Slack does not deliver it again.
            if let Some(id) = envelope["envelope_id"].as_str() {
                let ack = json!({ "envelope_id": id }).to_string();
                if let Err(error) = socket.send(Message::Text(ack.into())).await {
                    return Ended::Failed(error.to_string());
                }
            }
            let parsed = wire::parse(&envelope);
            if parsed.inbound.is_some()
                && sender
                    .send(Update {
                        inbound: parsed.inbound,
                        press: Some(Carried { press: parsed.press, files: parsed.files }),
                    })
                    .is_err()
            {
                return Ended::Failed("the adapter stopped".into());
            }
        }
    }

    /// Takes the answer buttons off an answered card, so it is not pressed again.
    async fn clear_buttons(&self, press: &Press) {
        let Some((channel, ts)) = &press.card else { return };
        let mut body = wire::answered(press);
        body["channel"] = json!(channel);
        body["ts"] = json!(ts);
        if let Err(error) = self.inner.api.call("chat.update", &body, true).await {
            tracing::debug!(error = %error, "an answered card's buttons could not be removed");
        }
    }

    async fn post(&self, thread: &str, mut body: Value) -> adk_kernel::Result<()> {
        body["channel"] = json!(thread);
        // A rate limit on a reply to an account that is not paired is not waited out, so a
        // stranger's flood holds up no one's events.
        let patient = patient(SLACK_CHANNEL, thread);
        self.inner.api.call("chat.postMessage", &body, patient).await?;
        Ok(())
    }
}

#[async_trait]
impl Taker for SlackAdapter {
    type Press = Carried;

    fn channel_name(&self) -> &'static str {
        SLACK_CHANNEL
    }

    /// Hands one event to the gateway; an answered card loses its answer buttons.
    async fn take(
        &self,
        gateway: &ChannelGateway,
        update: &Update<Carried>,
        paired: bool,
    ) -> adk_kernel::Result<()> {
        let Some(mut inbound) = update.inbound.clone() else { return Ok(()) };
        let carried = update.press.as_ref();
        let press = carried.and_then(|carried| carried.press.as_ref());
        // A paired account's images, for a host that takes them; a stranger's are never
        // downloaded (spec 3.5 §10 X12).
        let files = carried.map_or(&[][..], |carried| carried.files.as_slice());
        if paired && !files.is_empty() {
            let api = &self.inner.api;
            let attachments =
                crate::channels::media::fetch_all(files, |file| api.download(file)).await;
            inbound = inbound.with_attachments(attachments);
        }
        let card = match (&inbound.body, press) {
            (InboundBody::Answer { decision, .. }, Some(press)) => Some(PressedCard {
                card: press.card.as_ref().map(|(_, ts)| ts.clone()),
                decision: decision.clone(),
            }),
            _ => None,
        };
        let handling = Handling::of(SLACK_CHANNEL, gateway, &inbound, paired, card);
        let outcome = HANDLING.scope(handling, gateway.receive(inbound)).await;
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

    async fn not_taken(&self, update: &Update<Carried>) {
        let Some(inbound) = &update.inbound else { return };
        let pressed = update.press.as_ref().is_some_and(|carried| carried.press.is_some());
        let text = if pressed { ANSWER_NOT_TAKEN } else { MESSAGE_NOT_TAKEN };
        for body in wire::plain(text) {
            if let Err(error) = self.post(&inbound.thread, body).await {
                tracing::debug!(error = %error, "a person could not be told their message was not taken");
            }
        }
    }
}

#[async_trait]
impl ChannelSink for SlackAdapter {
    fn channel(&self) -> &str {
        SLACK_CHANNEL
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
    /// confirmation is sent (spec 3.3 §8.1, §11).
    async fn send_notice(&self, thread: &str, notice: &ChannelNotice) -> adk_kernel::Result<()> {
        self.inner.kit.before_notice(SLACK_CHANNEL, SLACK, thread, notice).await;
        let words = notice_words(notice, self.inner.kit.offset, Utc::now(), false);
        self.post(thread, wire::message(&words)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_say_what_to_do_and_keep_their_category() {
        let error: AdkError = SlackError::Unauthorized.into();
        assert_eq!(
            (error.code, error.category),
            ("surface.slack.unauthorized", ErrorCategory::Unauthorized)
        );
        let refused = SlackError::Refused {
            method: "chat.postMessage".into(),
            error: "not_in_channel".into(),
        };
        assert_eq!(AdkError::from(refused).category, ErrorCategory::Forbidden);
        let limited =
            SlackError::RateLimited { method: "chat.postMessage".into(), retry_after: 90 };
        assert!(AdkError::from(limited).is_retryable());
    }

    #[test]
    fn the_builder_refuses_a_bad_token_or_address_without_echoing_the_token() {
        let error = SlackAdapter::builder("xapp-secret", "xoxb-secret")
            .api_base("http://example.com/api")
            .build()
            .unwrap_err();
        assert_eq!(error.code(), "surface.invalid");
        assert!(!error.to_string().contains("secret"), "{error}");
        assert!(SlackAdapter::builder("", "xoxb-1").build().is_err());
        assert!(SlackAdapter::builder("xapp-1", "xoxb 1").build().is_err());
        let adapter = SlackAdapter::builder("xapp-secret", "xoxb-secret").build().unwrap();
        assert!(!format!("{adapter:?}").contains("secret"));
        assert_eq!(ChannelSink::channel(&adapter), "slack");
    }
}
