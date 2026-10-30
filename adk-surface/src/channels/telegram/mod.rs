//! The Telegram adapter (spec 3.3 §5.3; §13 decisions 1–3).

mod api;
pub(super) mod wire;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use adk_core::audit_log::{
    AuditActor, AuditDecision, AuditRecordDraft, AuditRecordKind, AuditTrail,
};
use adk_core::principal::SubjectRef;
use adk_core::{AdkError, ApprovalRequest, ApprovalService, ErrorCategory, ErrorComponent};
use adk_kernel::KernelError;
use adk_kernel::channels::{
    ChannelAnswer, ChannelBinding, ChannelBindingStore, ChannelGateway, ChannelInbound,
    ChannelNotice, ChannelSink, DecisionCard, InboundBody, InboundOutcome, StopNotice,
};
use async_trait::async_trait;
use chrono::{FixedOffset, Utc};
use serde_json::{Value, json};
use tokio::task::JoinHandle;

use crate::answers::{AnswerLog, AnswerRecord, SURFACE_AUDIT_DECIDE};
use crate::error::{Result, SurfaceError};
use crate::origin::{Origin, SurfaceKind, Via};
use crate::rules::Answer;
use crate::words;
use api::BotApi;
use wire::{Outgoing, Parsed, Press};

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
/// How many times a paired account's update is tried while the gateway's failure on it may
/// clear, before it is dropped and the person told.
const UPDATE_ATTEMPTS: u32 = 5;

tokio::task_local! {
    /// The update the gateway is handling, while it handles it.
    static HANDLING: Arc<Handling>;
}

/// An update in the gateway's hands: whose it is and, for a pressed answer, its card.
struct Handling {
    /// The private chat it came from, which is the account's own.
    thread: String,
    /// The account.
    account: String,
    /// Whether the account is paired, or its pairing could not be read. A rate limit on what
    /// is sent to an account that is not paired is never waited out.
    paired: bool,
    /// Where paired accounts are kept, to name the person who answered.
    bindings: Arc<dyn ChannelBindingStore>,
    /// A pressed answer's card and decision.
    press: Option<PressedCard>,
}

/// The card a pressed answer came from.
struct PressedCard {
    /// The card's message.
    message: Option<i64>,
    /// The decision the answer is for.
    decision: String,
}

/// What the polling did with the updates Telegram has not yet been told were taken.
#[derive(Default)]
struct Reads {
    /// Updates taken or dropped above the offset Telegram was last given.
    handled: BTreeSet<i64>,
    /// How many times the gateway failed on each update that is read again.
    failures: HashMap<i64, u32>,
}

impl Reads {
    /// Update `id` was taken, or dropped: it is not handed over again.
    fn done(&mut self, id: i64) {
        self.handled.insert(id);
        self.failures.remove(&id);
    }

    /// The gateway failed on update `id` once more; returns how many times it failed.
    fn failed(&mut self, id: i64) -> u32 {
        let failures = self.failures.entry(id).or_default();
        *failures += 1;
        *failures
    }

    /// The offset telling Telegram that `updates` were taken, up to the first one read again.
    fn acknowledged(&mut self, updates: &[Value]) -> Option<i64> {
        let mut ids: Vec<i64> =
            updates.iter().filter_map(|update| update.get("update_id")?.as_i64()).collect();
        ids.sort_unstable();
        let mut next = None;
        for id in ids {
            if !self.handled.remove(&id) {
                break;
            }
            next = Some(id + 1);
        }
        next
    }
}

/// Whether the gateway's failure may clear by itself: a store or network outage, a timeout
/// or a rate limit, never a refusal.
fn may_clear(error: &KernelError) -> bool {
    if matches!(error, KernelError::Frozen { .. }) {
        return false;
    }
    match error.category() {
        ErrorCategory::Unavailable | ErrorCategory::Timeout | ErrorCategory::RateLimited => true,
        ErrorCategory::InvalidInput
        | ErrorCategory::Unauthorized
        | ErrorCategory::Forbidden
        | ErrorCategory::NotFound
        | ErrorCategory::Cancelled
        | ErrorCategory::Internal
        | ErrorCategory::Unsupported
        | ErrorCategory::ResourceExhausted
        | ErrorCategory::Conflict => false,
    }
}

/// Whether `account` is paired; `true` when its pairing could not be read, since a store's
/// failure is not the account's doing.
async fn is_paired(gateway: &ChannelGateway, account: &str) -> bool {
    let id = ChannelBinding::id_for(TELEGRAM_CHANNEL, account);
    match gateway.bindings().get_binding(&id).await {
        Ok(binding) => binding.is_some_and(|binding| binding.is_active()),
        Err(error) => {
            tracing::debug!(error = %error, "a telegram account's pairing could not be read");
            true
        }
    }
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
    offset: Option<FixedOffset>,
    approvals: Option<Arc<dyn ApprovalService>>,
    answers: Option<Arc<dyn AnswerLog>>,
    audit: Option<AuditTrail>,
    card_hints: Vec<Arc<dyn crate::hints::CardHints>>,
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

    /// Shows every time — "since 18:00", "07:31", a pairing code's "before 08:31" — at
    /// `offset` east of UTC instead of in the host's time zone, as
    /// `SurfaceEngineBuilder::utc_offset` does. Without it, times follow the host's time zone,
    /// daylight saving time included.
    #[must_use]
    pub fn utc_offset(mut self, offset: FixedOffset) -> Self {
        self.offset = Some(offset);
        self
    }

    /// Reads each card's decision from `approvals`, the kernel's approval service, so the card
    /// is headed by the call in plain words — "Write file", "Send email" — as the engine heads
    /// a chat card, with a summary an approval route wrote under it (spec 3.3 §7.1, §8).
    /// Without it, a card is headed by the kernel's masked summary.
    #[must_use]
    pub fn approvals(mut self, approvals: Arc<dyn ApprovalService>) -> Self {
        self.approvals = Some(approvals);
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

    /// Heads a card with the chat heading a feature's [`CardHints`](crate::CardHints) give its
    /// decision — "Snooze something in Coming up" — in the order added, as the engine heads a
    /// chat card (spec 3.3 §7.1), and offers no answer on a card a hint marks as spending money
    /// (§7.4 rule 4). Needs [`approvals`](Self::approvals): a hint reads the decision. Give the
    /// hints the engine was built with.
    ///
    /// The kernel's chat gateway decides from the inbox entry alone: a feature whose hint marks
    /// money also rates the call high when it is held, through its approval router, so a chat
    /// answer is refused there too (owner question 30).
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use std::sync::Arc;
    /// use adk_surface::channels::TelegramAdapter;
    ///
    /// # fn example(approvals: Arc<dyn adk_core::ApprovalService>, hints: Arc<dyn adk_surface::CardHints>) {
    /// let telegram = TelegramAdapter::builder("123456789:AA-example")
    ///     .approvals(approvals)
    ///     .card_hints(hints)
    ///     .build();
    /// # let _ = telegram;
    /// # }
    /// ```
    #[must_use]
    pub fn card_hints(mut self, hints: Arc<dyn crate::hints::CardHints>) -> Self {
        self.card_hints.push(hints);
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
                approvals: self.approvals,
                answers: self.answers,
                audit: self.audit,
                card_hints: self.card_hints,
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
/// | Out | A card: "Waiting for you · since 18:00", the tool's name in plain words (with [`approvals`](TelegramAdapterBuilder::approvals)) above the kernel's masked summary when an approval route wrote one, or else that summary alone, the agent and the risk, a button for each answer the card offers and no others, and "Open in home" — never the call's arguments (S-I5). Agent replies as plain text, so nothing in them is read as markup (S-I9) |
/// | In | New text messages and pressed buttons from a person in a private chat; groups, channels, bots, edits and media are ignored. Typed text never answers a decision (S-I4), loses the characters Telegram shows as nothing, and `/start` loses a deep link's payload (S-I6) |
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
    offset: Option<FixedOffset>,
    approvals: Option<Arc<dyn ApprovalService>>,
    answers: Option<Arc<dyn AnswerLog>>,
    audit: Option<AuditTrail>,
    card_hints: Vec<Arc<dyn crate::hints::CardHints>>,
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
            offset: None,
            approvals: None,
            answers: None,
            audit: None,
            card_hints: Vec::new(),
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
    /// The polling holds `gateway`, whose replies and cards stop when its last handle is
    /// dropped: keep a clone for them to outlast the polling.
    ///
    /// An update is acknowledged to Telegram only once the gateway took it, so a restart
    /// reads it again; the gateway starts one turn per message whatever the redeliveries. In
    /// each batch a paired account's updates go first, each account's in the order sent, so
    /// a stranger's messages never hold a paired person's "stop all" or answer (S-I6,
    /// S-I10). An update the gateway fails on is read again, after 1, 2, 4 and 8 seconds,
    /// only when it is a paired account's and the failure may clear, such as a store outage;
    /// that account's later updates wait with it, while the other accounts' updates already
    /// read are handed over. Any other failed update
    /// is dropped, and a paired person is told it was not taken. Transport failures are
    /// retried with backoff; a refused token ends the polling with
    /// [`TelegramError::Unauthorized`].
    pub fn start(&self, gateway: ChannelGateway) -> TelegramPolling {
        let adapter = self.clone();
        let task = tokio::spawn(async move { adapter.poll(&gateway).await });
        TelegramPolling { task: Some(task) }
    }

    async fn poll(&self, gateway: &ChannelGateway) -> std::result::Result<(), TelegramError> {
        let commands =
            json!({ "commands": [{ "command": "stop", "description": words::STOP_ALL }] });
        match self.inner.api.call("setMyCommands", &commands, REQUEST_TIMEOUT).await {
            Err(TelegramError::Unauthorized) => return Err(TelegramError::Unauthorized),
            Err(error) => tracing::warn!(error = %error, "the bot's commands could not be listed"),
            Ok(_) => {}
        }
        let mut offset: Option<i64> = None;
        let mut backoff = MIN_BACKOFF;
        let mut conflicted = false;
        let mut reads = Reads::default();
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
                    conflicted = false;
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
                    // Two readers of one bot end each other's polls: said once, loudly.
                    if matches!(error, TelegramError::Conflict) && !conflicted {
                        conflicted = true;
                        tracing::error!(error = %error, "another process reads this bot's updates");
                    } else {
                        tracing::warn!(error = %error, "reading telegram updates failed; retrying");
                    }
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                    continue;
                }
            };
            let wait = self.take(gateway, &updates, &mut reads).await;
            if let Some(next) = reads.acknowledged(&updates) {
                offset = Some(next);
            }
            if let Some(wait) = wait {
                tokio::time::sleep(wait).await;
            }
        }
    }

    /// Hands one batch of updates to the gateway, as [`start`](Self::start) describes.
    /// Returns how long to wait before reading again when an update is to be read again.
    async fn take(
        &self,
        gateway: &ChannelGateway,
        updates: &[Value],
        reads: &mut Reads,
    ) -> Option<Duration> {
        let mut batch = Vec::new();
        for update in updates {
            // Telegram numbers every update; one without a number cannot be acknowledged.
            let Some(id) = update.get("update_id").and_then(Value::as_i64) else { continue };
            if reads.handled.contains(&id) {
                continue;
            }
            let parsed = wire::parse(update);
            let paired = match &parsed.inbound {
                Some(inbound) => is_paired(gateway, &inbound.account).await,
                None => false,
            };
            batch.push((id, parsed, paired));
        }
        // Stable: a paired account's updates first, each account's in the order sent.
        batch.sort_by_key(|(_, _, paired)| !*paired);
        let mut held: HashSet<String> = HashSet::new();
        let mut wait: Option<Duration> = None;
        for (id, parsed, paired) in batch {
            let account = parsed.inbound.as_ref().map(|inbound| inbound.account.clone());
            if account.as_ref().is_some_and(|account| held.contains(account)) {
                continue;
            }
            let Err(error) = self.handle(gateway, &parsed, paired).await else {
                reads.done(id);
                continue;
            };
            let attempt = reads.failed(id);
            if paired && may_clear(&error) && attempt < UPDATE_ATTEMPTS {
                tracing::warn!(update.id = id, error = %error, attempt, "the gateway could not take a telegram update; reading it again");
                let delay = MIN_BACKOFF * 2u32.pow(attempt - 1);
                wait = Some(wait.map_or(delay, |wait| wait.min(delay)));
                held.extend(account);
                continue;
            }
            tracing::warn!(update.id = id, error = %error, attempt, "the gateway could not take a telegram update; dropping it");
            reads.done(id);
            if paired {
                self.not_taken(&parsed).await;
            }
        }
        wait
    }

    /// Hands one update to the gateway. A pressed button is acknowledged, and its card
    /// loses its buttons once the press was answered; an answer that decided is recorded with
    /// where it was given before its confirmation is sent ([`ChannelSink::send_notice`]).
    async fn handle(
        &self,
        gateway: &ChannelGateway,
        parsed: &Parsed,
        paired: bool,
    ) -> adk_kernel::Result<()> {
        let Some(mut inbound) = parsed.inbound.clone() else {
            // A press that answers nothing, such as forged data, is only acknowledged.
            if let Some(press) = &parsed.press {
                self.acknowledge(press).await;
            }
            return Ok(());
        };
        // A paired account's photo, for a host that takes it; a stranger's is never
        // downloaded (spec 3.5 §10 X12).
        if paired
            && let Some(photo) = &parsed.photo
            && let Some(attachment) = self.inner.api.download(photo).await
        {
            inbound = inbound.with_attachments([attachment]);
        }
        let press = parsed.press.as_ref();
        let card = match (&inbound.body, press) {
            (InboundBody::Answer { decision, .. }, Some(press)) => Some(PressedCard {
                message: press.card.as_ref().map(|(_, message)| *message),
                decision: decision.clone(),
            }),
            _ => None,
        };
        let handling = Arc::new(Handling {
            thread: inbound.thread.clone(),
            account: inbound.account.clone(),
            paired,
            bindings: Arc::clone(gateway.bindings()),
            press: card,
        });
        let outcome = HANDLING.scope(handling, gateway.receive(inbound)).await;
        let Some(press) = press else { return outcome.map(drop) };
        self.acknowledge(press).await;
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

    /// Tells a paired person that their update reached no agent: a typed message, as a reply
    /// to it, or a pressed answer that decided nothing, as a reply to its card.
    async fn not_taken(&self, parsed: &Parsed) {
        let Some(inbound) = &parsed.inbound else { return };
        let (text, reply_to) = match &parsed.press {
            Some(press) => (wire::ANSWER_NOT_TAKEN, press.card.as_ref().map(|(_, id)| *id)),
            None => (wire::MESSAGE_NOT_TAKEN, inbound.message_id.parse().ok()),
        };
        let message = Outgoing::plain(text.to_string());
        if let Err(error) = self.send(&inbound.thread, message, reply_to).await {
            tracing::debug!(error = %error, "a person could not be told their update was not taken");
        }
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
        let Some((chat, message)) = &press.card else { return };
        let body = json!({
            "chat_id": chat_id(chat),
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
        bindings: &dyn ChannelBindingStore,
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
        let binding = match bindings.get_binding(&id).await {
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
            via: Via {
                surface: SurfaceKind::Chat,
                origin: Origin::Chat { channel: TELEGRAM.into() },
            },
            answer,
            at: Utc::now(),
        };
        if let Some(log) = &self.inner.answers
            && let Err(error) = log.record(&record).await
        {
            tracing::warn!(error = %error, "a telegram answer could not be kept in the answer log");
        }
        // The call it answered, so the record names its tool and whose call it was (§11).
        let request = match (&self.inner.audit, &self.inner.approvals) {
            (Some(_), Some(approvals)) => approvals.get(decision).await.ok().flatten(),
            _ => None,
        };
        let title = match &request {
            Some(request) => crate::hints::named(&self.inner.card_hints, request).await,
            None => None,
        };
        if let Some(trail) = &self.inner.audit
            && let Err(error) = trail.record(answer_record(&record, request.as_ref(), title)).await
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
        let mut body = json!({
            "chat_id": chat_id(thread),
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
            body["reply_parameters"] =
                json!({ "message_id": id, "allow_sending_without_reply": true });
        }
        // A rate limit on a reply to an account that is not paired is not waited out, so a
        // stranger's flood holds up no one's updates.
        let patient = HANDLING
            .try_with(|handling| handling.paired || handling.thread != thread)
            .unwrap_or(true);
        if patient {
            self.inner.api.call("sendMessage", &body, REQUEST_TIMEOUT).await?;
        } else {
            self.inner.api.call_once("sendMessage", &body, REQUEST_TIMEOUT).await?;
        }
        Ok(())
    }
}

/// `thread` as the Bot API's `chat_id`: a number, or text for a channel's public `@name`.
fn chat_id(thread: &str) -> Value {
    thread.parse::<i64>().map_or_else(|_| json!(thread), |id| json!(id))
}

/// The `surface.decide` record of an answer given in Telegram, as the engine writes its own
/// answers' (spec 3.3 §11).
fn answer_record(
    record: &AnswerRecord,
    request: Option<&ApprovalRequest>,
    title: Option<String>,
) -> AuditRecordDraft {
    let decision = match record.answer {
        Answer::Deny => AuditDecision::Rejected,
        Answer::AllowOnce | Answer::AlwaysAllow | Answer::Edit => AuditDecision::Approved,
    };
    let mut draft = AuditRecordDraft::new(AuditRecordKind::Approval, SURFACE_AUDIT_DECIDE)
        .actor(AuditActor::subject(record.by.clone()))
        .ext("surface", serde_json::to_value(record.via.surface).unwrap_or(Value::Null))
        .ext("origin", serde_json::to_value(&record.via.origin).unwrap_or(Value::Null))
        .ext("approvalId", json!(record.approval_id))
        .ext("answer", serde_json::to_value(record.answer).unwrap_or(Value::Null))
        .ext("outcome", json!("decided"))
        .decision(decision);
    if let Some(request) = request {
        draft = draft.ext("tool", json!(request.tool_name));
        if let Some(acting) = &request.acting {
            draft = draft.ext("actingAgent", json!(acting.agent.id.to_string()));
        }
        // What the call acts on, as the engine's own record names it (§8).
        if let Some(title) = title {
            draft = draft.ext("title", json!(title)).ext("summary", json!(request.summary));
        }
    }
    draft
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
        let request = match &self.inner.approvals {
            Some(approvals) => approvals.get(&card.decision).await.unwrap_or_else(|error| {
                tracing::warn!(error = %error, "a card's decision could not be read; headed by its summary");
                None
            }),
            None => None,
        };
        let mut heading = wire::heading(card, request.as_ref());
        let mut card = card.clone();
        // A feature's chat heading, as the engine heads a chat card (spec 3.3 §7.1); a call
        // a hint marks as spending money is never answered in chat (§7.4 rule 4).
        if let Some(request) = request.as_ref().filter(|request| request.id == card.decision) {
            let mut headed = false;
            for hints in &self.inner.card_hints {
                let Some(hint) = hints.hint(request).await else { continue };
                if let Some(chat) = hint.chat_heading.filter(|_| !headed) {
                    heading.title = chat;
                    headed = true;
                }
                if hint.money == Some(true) && !card.adds_agent {
                    card.answers.clear();
                }
            }
        }
        self.send(thread, wire::card(&card, &heading, self.inner.offset), None).await
    }

    async fn send_stopped(&self, thread: &str, notice: &StopNotice) -> adk_kernel::Result<()> {
        self.send(thread, wire::stopped(notice), None).await
    }

    /// A notice about a pressed card answers that card. The gateway says "decided" there
    /// only once the answer decided the decision, a quorum's last answer included and its
    /// earlier ones not; the answer is recorded with where it was given before the
    /// confirmation is sent, so neither a slow Telegram nor a stop loses it (spec 3.3 §8.1,
    /// §11).
    async fn send_notice(&self, thread: &str, notice: &ChannelNotice) -> adk_kernel::Result<()> {
        let handling =
            HANDLING.try_with(Arc::clone).ok().filter(|handling| handling.thread == thread);
        let press = handling.as_ref().and_then(|handling| handling.press.as_ref());
        if let (Some(handling), Some(press), ChannelNotice::Decided { answer, .. }) =
            (&handling, press, notice)
        {
            let bindings = handling.bindings.as_ref();
            self.record_answer(bindings, &handling.account, &press.decision, *answer).await;
        }
        let reply_to = press.and_then(|press| press.message);
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
    /// Stops reading updates. The gateway given to [`TelegramAdapter::start`] is dropped
    /// with the polling, so its replies and cards stop too unless the caller kept a handle.
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
            let draft: AuditRecordDraft = answer_record(&given, None, None);
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
        let error = TelegramAdapter::builder("123:secret")
            .api_base("http://example.com")
            .build()
            .unwrap_err();
        assert_eq!(error.code(), "surface.invalid");
        assert!(!error.to_string().contains("secret"), "{error}");
        assert!(TelegramAdapter::builder("").build().is_err());
        let adapter = TelegramAdapter::builder("123:secret").build().unwrap();
        assert!(!format!("{adapter:?}").contains("secret"));
        assert_eq!(adapter.channel(), "telegram");
    }
}
