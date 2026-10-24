//! The Telegram adapter (spec 3.3 §5.3; §13 decisions 1–3).

mod api;
mod wire;

use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use adk_core::audit_log::{
    AuditActor, AuditDecision, AuditRecordDraft, AuditRecordKind, AuditTrail,
};
use adk_core::principal::SubjectRef;
use adk_core::{AdkError, ErrorCategory, ErrorComponent};
use adk_kernel::KernelError;
use adk_kernel::channels::{
    ChannelAnswer, ChannelBinding, ChannelGateway, ChannelInbound, ChannelNotice, ChannelSink,
    DecisionCard, InboundOutcome, StopNotice,
};
use async_trait::async_trait;
use chrono::{FixedOffset, Local, Utc};
use serde_json::{Value, json};
use tokio::task::JoinHandle;

use crate::answers::{AnswerLog, AnswerRecord, SURFACE_AUDIT_DECIDE};
use crate::error::{Result, SurfaceError};
use crate::origin::{Origin, SurfaceKind, Via};
use crate::rules::Answer;
use crate::words;
use api::BotApi;
use wire::{Outgoing, Press};

/// The channel the Telegram adapter serves, as the kernel names it: pairings, thread
/// sessions and `kernel.channel` audit records carry it (spec 3.3 §10 K1).
pub const TELEGRAM_CHANNEL: &str = "telegram";

/// Telegram as a person reads it, in "You said yes in Telegram at 07:31" (spec 3.3 §8.1).
pub const TELEGRAM: &str = "Telegram";

const DEFAULT_API_BASE: &str = "https://api.telegram.org";
/// How long one `getUpdates` call waits for an update.
const DEFAULT_POLL_TIMEOUT: Duration = Duration::from_secs(25);
/// How long any other call may take.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// How many times an update the gateway could not take is read again before it is skipped.
const UPDATE_ATTEMPTS: u32 = 5;

tokio::task_local! {
    /// The card a pressed answer came from, while the gateway handles the answer.
    static PRESSED: Arc<Pressed>;
}

/// A pressed answer in progress: where its card is, and whether the gateway told the
/// person it decided.
struct Pressed {
    thread: String,
    card_message: Option<i64>,
    decided: Mutex<Option<ChannelAnswer>>,
}

/// Why the Telegram adapter could not talk to Telegram (spec 3.3 §5.3).
///
/// Never carries the bot token. Converts into [`AdkError`] with a `surface.telegram.*` code
/// and provider `telegram`.
///
/// # Example
///
/// ```rust
/// use adk_surface::channels::TelegramError;
///
/// let error = TelegramError::Unauthorized;
/// assert_eq!(error.code(), "surface.telegram.unauthorized");
/// assert!(error.to_string().contains("TELEGRAM_BOT_TOKEN"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TelegramError {
    /// Telegram refused the bot token.
    #[error(
        "Telegram refused the bot token: check TELEGRAM_BOT_TOKEN, or ask @BotFather for a new one"
    )]
    Unauthorized,
    /// Another process reads the bot's updates, or the bot has a webhook.
    #[error(
        "another process reads this bot's updates, or the bot has a webhook: stop the other \
         process or delete the webhook (`deleteWebhook`); the adapter keeps trying"
    )]
    Conflict,
    /// Telegram asked the bot to slow down for longer than the adapter waits.
    #[error("Telegram asked to wait {retry_after} s before {method}: the bot is sending too much")]
    RateLimited {
        /// The Bot API method.
        method: String,
        /// How long Telegram asked to wait, in seconds.
        retry_after: u64,
    },
    /// Telegram refused the call, such as a chat that blocked the bot.
    #[error("Telegram refused {method} ({code}): {description}")]
    Refused {
        /// The Bot API method.
        method: String,
        /// Telegram's error code.
        code: i64,
        /// Telegram's description.
        description: String,
    },
    /// Telegram could not be reached.
    #[error(
        "Telegram could not be reached for {method}: {reason}; check the network and the API \
         address"
    )]
    Unreachable {
        /// The Bot API method.
        method: String,
        /// What failed.
        reason: String,
    },
    /// Telegram's answer was not the Bot API's JSON.
    #[error("Telegram's answer to {method} could not be read: {reason}")]
    InvalidResponse {
        /// The Bot API method.
        method: String,
        /// What was wrong with it.
        reason: String,
    },
}

impl TelegramError {
    /// The error code: `surface.telegram.unauthorized`, `.conflict`, `.rate_limited`,
    /// `.refused`, `.unreachable` or `.invalid_response`.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unauthorized => "surface.telegram.unauthorized",
            Self::Conflict => "surface.telegram.conflict",
            Self::RateLimited { .. } => "surface.telegram.rate_limited",
            Self::Refused { .. } => "surface.telegram.refused",
            Self::Unreachable { .. } => "surface.telegram.unreachable",
            Self::InvalidResponse { .. } => "surface.telegram.invalid_response",
        }
    }
}

impl From<TelegramError> for AdkError {
    fn from(error: TelegramError) -> Self {
        let category = match &error {
            TelegramError::Unauthorized => ErrorCategory::Unauthorized,
            TelegramError::Conflict => ErrorCategory::Conflict,
            TelegramError::RateLimited { .. } => ErrorCategory::RateLimited,
            TelegramError::Refused { code: 403, .. } => ErrorCategory::Forbidden,
            TelegramError::Refused { code: 400, .. } => ErrorCategory::InvalidInput,
            TelegramError::Refused { .. } | TelegramError::Unreachable { .. } => {
                ErrorCategory::Unavailable
            }
            TelegramError::InvalidResponse { .. } => ErrorCategory::Internal,
        };
        AdkError::new(ErrorComponent::Server, category, error.code(), error.to_string())
            .with_provider("telegram")
    }
}

impl From<TelegramError> for KernelError {
    fn from(error: TelegramError) -> Self {
        Self::Core(error.into())
    }
}

/// Builds a [`TelegramAdapter`].
pub struct TelegramAdapterBuilder {
    token: String,
    api_base: String,
    poll_timeout: Duration,
    offset: FixedOffset,
    answers: Option<Arc<dyn AnswerLog>>,
    audit: Option<AuditTrail>,
}

impl fmt::Debug for TelegramAdapterBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TelegramAdapterBuilder")
            .field("api_base", &self.api_base)
            .field("poll_timeout", &self.poll_timeout)
            .finish_non_exhaustive()
    }
}

impl TelegramAdapterBuilder {
    /// The Bot API's address; defaults to `https://api.telegram.org`. Plain `http` is
    /// accepted on loopback only (a local Bot API server, or a test double), since the token
    /// travels in the request path.
    #[must_use]
    pub fn api_base(mut self, url: impl Into<String>) -> Self {
        self.api_base = url.into();
        self
    }

    /// How long one long poll waits for an update, at least a second; defaults to 25 s.
    #[must_use]
    pub fn poll_timeout(mut self, timeout: Duration) -> Self {
        self.poll_timeout = timeout.max(Duration::from_secs(1));
        self
    }

    /// Shows times at `offset` east of UTC instead of the host's time zone, as
    /// `SurfaceEngineBuilder::utc_offset` does.
    #[must_use]
    pub fn utc_offset(mut self, offset: FixedOffset) -> Self {
        self.offset = offset;
        self
    }

    /// Records each answer given in Telegram in `log`, the engine's [`AnswerLog`], so every
    /// other surface shows "You said yes in Telegram at 07:31" (spec 3.3 §8.1). Give the log
    /// the engine was built with.
    #[must_use]
    pub fn answer_log(mut self, log: Arc<dyn AnswerLog>) -> Self {
        self.answers = Some(log);
        self
    }

    /// Writes a `surface.decide` record of each answer given in Telegram to `trail`, the
    /// engine's audit trail, so an [`AuditAnswerLog`](crate::AuditAnswerLog) over the same
    /// store finds how it was given after a restart (spec 3.3 §11).
    #[must_use]
    pub fn audit(mut self, trail: AuditTrail) -> Self {
        self.audit = Some(trail);
        self
    }

    /// The adapter.
    ///
    /// # Errors
    ///
    /// Returns `surface.invalid` for a malformed token, or an API address that is not
    /// `https` (or `http` on loopback).
    pub fn build(self) -> Result<TelegramAdapter> {
        let api = BotApi::new(&self.api_base, &self.token).map_err(SurfaceError::Invalid)?;
        Ok(TelegramAdapter {
            inner: Arc::new(Inner {
                api,
                poll_timeout: self.poll_timeout,
                offset: self.offset,
                answers: self.answers,
                audit: self.audit,
            }),
        })
    }
}

/// The Telegram adapter (spec 3.3 §5.3): a bot as the person's chat thread with their
/// agents, over the kernel's channel contract (§10 K1).
///
/// It is the gateway's [`ChannelSink`] for the channel [`TELEGRAM_CHANNEL`], and
/// [`start`](Self::start) reads the bot's updates by long polling and hands each to
/// [`ChannelGateway::receive`]. The gateway decides what happens: pairing, turns, answers,
/// stopping. The adapter keeps to what reaches Telegram and what is taken from it:
///
/// | Direction | What |
/// |---|---|
/// | Out | A card: "Waiting for you · since 18:00", the kernel's masked summary, the agent and the risk, a button for each answer the card offers and no others, and "Open in home" — never the call's arguments (S-I5). Agent replies as plain text, so nothing in them is read as markup (S-I9) |
/// | In | New text messages and pressed buttons from a person in a private chat; groups, channels, bots, edits and media are ignored. Typed text never answers a decision (S-I4), and `/start` loses a deep link's payload (S-I6) |
/// | Commands | `/stop` is listed among the bot's commands; "stop all" and `/stop` stop everything, and resuming is refused (S-I10) |
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use adk_kernel::channels::{ChannelDoorbell, ChannelGateway, InMemoryChannelBindingStore};
/// use adk_surface::channels::TelegramAdapter;
///
/// # async fn example(kernel: adk_kernel::Kernel, doorbell: Arc<ChannelDoorbell>) -> Result<(), Box<dyn std::error::Error>> {
/// let telegram = TelegramAdapter::builder(std::env::var("TELEGRAM_BOT_TOKEN")?).build()?;
/// let gateway = ChannelGateway::builder(Arc::new(kernel), Arc::new(InMemoryChannelBindingStore::new()))
///     .sink(Arc::new(telegram.clone()))
///     .home_url("http://127.0.0.1:8080/ui/")
///     .doorbell(doorbell)
///     .build();
/// gateway.start().await?;
/// gateway.ring_waiting().await?;
/// let polling = telegram.start(gateway);
/// polling.join().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct TelegramAdapter {
    inner: Arc<Inner>,
}

struct Inner {
    api: BotApi,
    poll_timeout: Duration,
    offset: FixedOffset,
    answers: Option<Arc<dyn AnswerLog>>,
    audit: Option<AuditTrail>,
}

impl fmt::Debug for TelegramAdapter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TelegramAdapter")
            .field("channel", &TELEGRAM_CHANNEL)
            .field("poll_timeout", &self.inner.poll_timeout)
            .finish_non_exhaustive()
    }
}

impl TelegramAdapter {
    /// An adapter for the bot with `token`, from `@BotFather`.
    pub fn builder(token: impl Into<String>) -> TelegramAdapterBuilder {
        TelegramAdapterBuilder {
            token: token.into(),
            api_base: DEFAULT_API_BASE.to_string(),
            poll_timeout: DEFAULT_POLL_TIMEOUT,
            offset: *Local::now().offset(),
            answers: None,
            audit: None,
        }
    }

    /// What the adapter makes of one Bot API `Update` (spec 3.3 S-I4, S-I6): a typed
    /// message, a pressed answer, or `None` for anything the gateway is not given.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_kernel::channels::InboundBody;
    /// use adk_surface::channels::TelegramAdapter;
    ///
    /// let telegram = TelegramAdapter::builder("123456789:AA-example").build().unwrap();
    /// let update = serde_json::json!({
    ///     "update_id": 1,
    ///     "message": {
    ///         "message_id": 31,
    ///         "from": { "id": 4217, "is_bot": false, "first_name": "Ana" },
    ///         "chat": { "id": 4217, "type": "private" },
    ///         "date": 0,
    ///         "text": "stop all",
    ///     },
    /// });
    /// let inbound = telegram.inbound(&update).unwrap();
    /// assert_eq!(inbound.body, InboundBody::Text { text: "stop all".into() });
    /// assert_eq!((inbound.account.as_str(), inbound.thread.as_str()), ("4217", "4217"));
    /// ```
    pub fn inbound(&self, update: &Value) -> Option<ChannelInbound> {
        wire::parse(update).inbound
    }

    /// Reads the bot's updates by long polling and hands each to `gateway`, which must have
    /// this adapter as its sink, until [`TelegramPolling::stop`]. Lists `/stop` ("Stop all")
    /// among the bot's commands first.
    ///
    /// An update is acknowledged to Telegram only once the gateway took it, so a restart
    /// reads it again; the gateway starts one turn per message whatever the redeliveries. An
    /// update the gateway fails on is read again, waiting longer each time, and skipped after
    /// five attempts. Transport failures are retried with backoff; a refused token ends the
    /// polling with [`TelegramError::Unauthorized`].
    pub fn start(&self, gateway: ChannelGateway) -> TelegramPolling {
        let adapter = self.clone();
        let task = tokio::spawn(async move { adapter.poll(&gateway).await });
        TelegramPolling { task: Some(task) }
    }

    async fn poll(&self, gateway: &ChannelGateway) -> std::result::Result<(), TelegramError> {
        let commands = json!({ "commands": [{ "command": "stop", "description": words::STOP_ALL }] });
        match self.inner.api.call("setMyCommands", &commands, REQUEST_TIMEOUT).await {
            Err(TelegramError::Unauthorized) => return Err(TelegramError::Unauthorized),
            Err(error) => tracing::warn!(error = %error, "the bot's commands could not be listed"),
            Ok(_) => {}
        }
        let mut offset: Option<i64> = None;
        let mut backoff = MIN_BACKOFF;
        let mut failures = 0;
        loop {
            let mut request = json!({
                "timeout": self.inner.poll_timeout.as_secs(),
                "allowed_updates": ["message", "callback_query"],
            });
            if let Some(offset) = offset {
                request["offset"] = json!(offset);
            }
            let timeout = self.inner.poll_timeout + REQUEST_TIMEOUT;
            let updates = match self.inner.api.call("getUpdates", &request, timeout).await {
                Ok(Value::Array(updates)) => {
                    backoff = MIN_BACKOFF;
                    updates
                }
                Ok(_) => {
                    tracing::warn!("telegram answered getUpdates without a list; retrying");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                    continue;
                }
                Err(TelegramError::Unauthorized) => return Err(TelegramError::Unauthorized),
                Err(TelegramError::RateLimited { retry_after, .. }) => {
                    tokio::time::sleep(Duration::from_secs(retry_after.max(1))).await;
                    continue;
                }
                Err(error) => {
                    tracing::warn!(error = %error, "reading telegram updates failed; retrying");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                    continue;
                }
            };
            for update in &updates {
                let id = update.get("update_id").and_then(Value::as_i64);
                if let Err(error) = self.handle(gateway, update).await {
                    failures += 1;
                    if failures < UPDATE_ATTEMPTS {
                        tracing::warn!(update.id = ?id, error = %error, attempt = failures, "the gateway could not take a telegram update; reading it again");
                        tokio::time::sleep(MIN_BACKOFF * 2u32.pow(failures - 1)).await;
                        break;
                    }
                    tracing::error!(update.id = ?id, error = %error, "the gateway could not take a telegram update; skipping it");
                }
                failures = 0;
                if let Some(id) = id {
                    offset = Some(id + 1);
                }
            }
        }
    }

    /// Hands one update to the gateway. A pressed button is acknowledged, its card loses
    /// its buttons once the press was answered, and an answer that decided is recorded with
    /// where it was given.
    async fn handle(&self, gateway: &ChannelGateway, update: &Value) -> adk_kernel::Result<()> {
        let parsed = wire::parse(update);
        let (inbound, press) = match (parsed.inbound, parsed.press) {
            (Some(inbound), Some(press)) => (inbound, press),
            (Some(inbound), None) => return gateway.receive(inbound).await.map(drop),
            (None, Some(press)) => {
                self.acknowledge(&press).await;
                return Ok(());
            }
            (None, None) => return Ok(()),
        };
        let pressed = Arc::new(Pressed {
            thread: inbound.thread.clone(),
            card_message: press.card.map(|(_, message)| message),
            decided: Mutex::new(None),
        });
        let account = inbound.account.clone();
        let outcome = PRESSED.scope(Arc::clone(&pressed), gateway.receive(inbound)).await;
        self.acknowledge(&press).await;
        let outcome = outcome?;
        if matches!(
            outcome,
            InboundOutcome::Decided { .. }
                | InboundOutcome::AlreadyDecided { .. }
                | InboundOutcome::DecisionChanged { .. }
                | InboundOutcome::DecisionRefused { .. }
        ) {
            self.clear_buttons(&press).await;
        }
        let decided = *pressed.decided.lock().unwrap_or_else(PoisonError::into_inner);
        if let (InboundOutcome::Decided { decision }, Some(answer)) = (&outcome, decided) {
            self.record_answer(gateway, &account, decision, answer).await;
        }
        Ok(())
    }

    /// Stops the button's spinner.
    async fn acknowledge(&self, press: &Press) {
        let body = json!({ "callback_query_id": press.callback_id });
        if let Err(error) = self.inner.api.call("answerCallbackQuery", &body, REQUEST_TIMEOUT).await
        {
            tracing::debug!(error = %error, "a pressed button could not be acknowledged");
        }
    }

    /// Takes the buttons off an answered card, so it is not pressed again.
    async fn clear_buttons(&self, press: &Press) {
        let Some((chat, message)) = press.card else { return };
        let body = json!({
            "chat_id": chat,
            "message_id": message,
            "reply_markup": { "inline_keyboard": [] },
        });
        if let Err(error) =
            self.inner.api.call("editMessageReplyMarkup", &body, REQUEST_TIMEOUT).await
        {
            tracing::debug!(error = %error, "an answered card's buttons could not be removed");
        }
    }

    /// Records that `account`'s person answered `decision` in Telegram (spec 3.3 §8.1, §11).
    async fn record_answer(
        &self,
        gateway: &ChannelGateway,
        account: &str,
        decision: &str,
        answer: ChannelAnswer,
    ) {
        let answer = match answer {
            ChannelAnswer::AllowOnce => Answer::AllowOnce,
            ChannelAnswer::Deny => Answer::Deny,
            _ => return,
        };
        let id = ChannelBinding::id_for(TELEGRAM_CHANNEL, account);
        let binding = match gateway.bindings().get_binding(&id).await {
            Ok(Some(binding)) => binding,
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(error = %error, "a telegram answer could not be recorded");
                return;
            }
        };
        let record = AnswerRecord {
            approval_id: decision.to_string(),
            by: SubjectRef::human(binding.user_id),
            via: Via { surface: SurfaceKind::Chat, origin: Origin::Chat { channel: TELEGRAM.into() } },
            answer,
            at: Utc::now(),
        };
        if let Some(log) = &self.inner.answers
            && let Err(error) = log.record(&record).await
        {
            tracing::warn!(error = %error, "a telegram answer could not be kept in the answer log");
        }
        if let Some(trail) = &self.inner.audit
            && let Err(error) = trail.record(answer_record(&record)).await
        {
            tracing::warn!(error = %error, "a telegram answer's audit record could not be written");
        }
    }

    async fn send(
        &self,
        thread: &str,
        message: Outgoing,
        reply_to: Option<i64>,
    ) -> adk_kernel::Result<()> {
        // Telegram's chat ids are numbers; a channel's public `@name` is the one text form.
        let chat = thread.parse::<i64>().map_or_else(|_| json!(thread), |id| json!(id));
        let mut body = json!({
            "chat_id": chat,
            "text": message.text,
            "link_preview_options": { "is_disabled": true },
        });
        if message.html {
            body["parse_mode"] = json!("HTML");
        }
        if let Some(keyboard) = message.keyboard {
            body["reply_markup"] = keyboard;
        }
        if let Some(id) = reply_to {
            body["reply_parameters"] = json!({ "message_id": id, "allow_sending_without_reply": true });
        }
        self.inner.api.call("sendMessage", &body, REQUEST_TIMEOUT).await?;
        Ok(())
    }
}

/// The `surface.decide` record of an answer given in Telegram, as the engine writes its own
/// answers' (spec 3.3 §11).
fn answer_record(record: &AnswerRecord) -> AuditRecordDraft {
    let decision = match record.answer {
        Answer::Deny => AuditDecision::Rejected,
        Answer::AllowOnce | Answer::AlwaysAllow | Answer::Edit => AuditDecision::Approved,
    };
    AuditRecordDraft::new(AuditRecordKind::Approval, SURFACE_AUDIT_DECIDE)
        .actor(AuditActor::subject(record.by.clone()))
        .ext("surface", serde_json::to_value(record.via.surface).unwrap_or(Value::Null))
        .ext("origin", serde_json::to_value(&record.via.origin).unwrap_or(Value::Null))
        .ext("approvalId", json!(record.approval_id))
        .ext("answer", serde_json::to_value(record.answer).unwrap_or(Value::Null))
        .ext("outcome", json!("decided"))
        .decision(decision)
}

#[async_trait]
impl ChannelSink for TelegramAdapter {
    fn channel(&self) -> &str {
        TELEGRAM_CHANNEL
    }

    async fn send_message(&self, thread: &str, text: &str) -> adk_kernel::Result<()> {
        for chunk in wire::plain_chunks(text) {
            self.send(thread, Outgoing::plain(chunk), None).await?;
        }
        Ok(())
    }

    async fn send_decision(&self, thread: &str, card: &DecisionCard) -> adk_kernel::Result<()> {
        self.send(thread, wire::card(card, self.inner.offset), None).await
    }

    async fn send_stopped(&self, thread: &str, notice: &StopNotice) -> adk_kernel::Result<()> {
        self.send(thread, wire::stopped(notice), None).await
    }

    /// A notice about a pressed card answers that card; "decided" there means the gateway
    /// recorded the answer.
    async fn send_notice(&self, thread: &str, notice: &ChannelNotice) -> adk_kernel::Result<()> {
        let pressed =
            PRESSED.try_with(Arc::clone).ok().filter(|pressed| pressed.thread == thread);
        if let (Some(pressed), ChannelNotice::Decided { answer, .. }) = (&pressed, notice) {
            *pressed.decided.lock().unwrap_or_else(PoisonError::into_inner) = Some(*answer);
        }
        let reply_to = pressed.and_then(|pressed| pressed.card_message);
        let message = wire::notice(notice, self.inner.offset, Utc::now(), reply_to.is_some());
        self.send(thread, message, reply_to).await
    }
}

/// The polling a [`TelegramAdapter::start`] runs; dropping it stops the polling.
///
/// # Example
///
/// ```rust,no_run
/// # async fn example(telegram: adk_surface::channels::TelegramAdapter, gateway: adk_kernel::channels::ChannelGateway) {
/// let polling = telegram.start(gateway);
/// polling.stop();
/// assert!(polling.join().await.is_ok());
/// # }
/// ```
#[derive(Debug)]
pub struct TelegramPolling {
    task: Option<JoinHandle<std::result::Result<(), TelegramError>>>,
}

impl TelegramPolling {
    /// Stops reading updates.
    pub fn stop(&self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }

    /// Whether the polling ended: stopped, or ended by a refused token.
    pub fn is_finished(&self) -> bool {
        self.task.as_ref().is_none_or(JoinHandle::is_finished)
    }

    /// Waits for the polling to end.
    ///
    /// # Errors
    ///
    /// Returns [`TelegramError::Unauthorized`] when Telegram refused the bot token; a
    /// stopped polling ends with `Ok`.
    pub async fn join(mut self) -> std::result::Result<(), TelegramError> {
        let Some(task) = self.task.take() else { return Ok(()) };
        match task.await {
            Ok(result) => result,
            Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
            Err(_) => Ok(()),
        }
    }
}

impl Drop for TelegramPolling {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use adk_core::audit_log::AuditRecordDraft;

    use super::*;
    use crate::answers::answer_of;

    /// What the adapter writes is what `AuditAnswerLog` reads back.
    #[test]
    fn the_answer_record_is_read_back_as_given_in_telegram() {
        for answer in [Answer::AllowOnce, Answer::Deny] {
            let given = AnswerRecord {
                approval_id: "apr_1".into(),
                by: SubjectRef::human("ana"),
                via: Via {
                    surface: SurfaceKind::Chat,
                    origin: Origin::Chat { channel: TELEGRAM.into() },
                },
                answer,
                at: Utc::now(),
            };
            let draft: AuditRecordDraft = answer_record(&given);
            let record = draft.into_record("replica-1", 0, given.at, None);
            let read = answer_of(&record, "apr_1").expect("the record is an answer");
            assert_eq!(read, given);
            assert_eq!(read.via.phrase(), "in Telegram");
            assert!(answer_of(&record, "apr_2").is_none());
        }
    }

    #[test]
    fn errors_say_what_to_do_and_keep_their_category() {
        let error: AdkError = TelegramError::Unauthorized.into();
        assert_eq!(error.code, "surface.telegram.unauthorized");
        assert_eq!(error.category, ErrorCategory::Unauthorized);
        let blocked = TelegramError::Refused {
            method: "sendMessage".into(),
            code: 403,
            description: "Forbidden: bot was blocked by the user".into(),
        };
        assert_eq!(AdkError::from(blocked).category, ErrorCategory::Forbidden);
        let limited = TelegramError::RateLimited { method: "sendMessage".into(), retry_after: 90 };
        assert!(AdkError::from(limited).is_retryable());
    }

    #[test]
    fn the_builder_refuses_a_bad_token_or_address_without_echoing_the_token() {
        let error =
            TelegramAdapter::builder("123:secret").api_base("http://example.com").build().unwrap_err();
        assert_eq!(error.code(), "surface.invalid");
        assert!(!error.to_string().contains("secret"), "{error}");
        assert!(TelegramAdapter::builder("").build().is_err());
        let adapter = TelegramAdapter::builder("123:secret").build().unwrap();
        assert!(!format!("{adapter:?}").contains("secret"));
        assert_eq!(adapter.channel(), "telegram");
    }
}
