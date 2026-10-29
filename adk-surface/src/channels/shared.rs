//! What the WhatsApp, Slack, Discord and Matrix adapters share (spec 3.5 §10 X12, on spec
//! 3.3 §5.3): the rules the adapter keeps, kept once.
//!
//! | Part | What it keeps |
//! |---|---|
//! | [`dispatch`] | A paired account's updates are handed over first, each account's in the order sent; a failed update is taken again only when it is a paired account's and the failure may clear, and that account's later updates wait with it (S-I6, S-I10) |
//! | [`HANDLING`], [`patient`] | Nothing sent to an account that is not paired waits out a rate limit |
//! | [`Kit`] | A card headed by the call in plain words, a feature's chat heading, no answer on a call that spends money, and each answer recorded with where it was given before its confirmation is sent (spec 3.3 §7.1, §7.4, §8.1, §11) |
//! | [`Words`] | The chat screen's words, the same on every platform, which each adapter escapes for its markup (S-I9) |
//! | [`clean_inbound`] | A person's words are only what they saw: the characters Telegram's adapter drops are dropped here too (S-I6) |

use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use adk_core::audit_log::{
    AuditActor, AuditDecision, AuditRecordDraft, AuditRecordKind, AuditTrail,
};
use adk_core::principal::SubjectRef;
use adk_core::{ApprovalRequest, ApprovalService, ErrorCategory};
use adk_kernel::KernelError;
use adk_kernel::channels::{
    ChannelAnswer, ChannelBinding, ChannelBindingStore, ChannelGateway, ChannelInbound,
    ChannelNotice, DecisionCard, StopNotice,
};
use async_trait::async_trait;
use chrono::{DateTime, FixedOffset, Utc};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;

#[cfg(any(feature = "whatsapp", feature = "slack", feature = "discord"))]
pub(crate) use super::telegram::wire::parse_callback;
pub(crate) use super::telegram::wire::{
    ANSWER_NOT_TAKEN, Heading, MESSAGE_NOT_TAKEN, callback_data, heading, hh_mm, is_hidden,
    is_invisible, split,
};
use crate::answers::{AnswerLog, AnswerRecord, SURFACE_AUDIT_DECIDE};
use crate::origin::{Origin, SurfaceKind, Via};
use crate::project::humanize;
use crate::rules::{Answer, Risk};
use crate::words;

/// The longest text taken from a person, in characters.
const MAX_INBOUND_CHARS: usize = 4096;
/// The first wait before a paired account's update is taken again.
const MIN_BACKOFF: Duration = Duration::from_secs(1);
/// How many times a paired account's update is tried while the gateway's failure on it may
/// clear, before it is dropped and the person told.
const UPDATE_ATTEMPTS: u32 = 5;
/// The longest rate limit a send waits out; a longer one fails the send.
#[cfg(any(feature = "slack", feature = "discord", feature = "matrix"))]
pub(crate) const MAX_RATE_LIMIT_WAIT: Duration = Duration::from_secs(30);
/// How many times one send is tried while the platform rate-limits it.
pub(crate) const RATE_LIMIT_ATTEMPTS: u32 = 3;
/// How long any call to a platform may take.
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// Text
// ---------------------------------------------------------------------------

/// A person's text as the gateway gets it (S-I6): the characters a chat app shows as
/// nothing dropped, at most 4096 characters, trimmed. `None` when nothing is left.
pub(crate) fn clean_inbound(text: &str) -> Option<String> {
    let text: String = text.chars().filter(|c| !is_invisible(*c)).take(MAX_INBOUND_CHARS).collect();
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// Text from an agent, a tool or a person without hidden characters, trimmed (S-I9).
pub(crate) fn clean_outbound(text: &str) -> String {
    text.chars().filter(|c| !is_hidden(*c)).collect::<String>().trim().to_string()
}

/// `text` in pieces of at most `max` characters, nothing lost and nothing repeated; empty
/// pieces are dropped.
pub(crate) fn chunks(text: &str, max: usize) -> Vec<String> {
    // `split` counts UTF-16 units, at least as many as characters.
    split(&clean_outbound(text), max).into_iter().filter(|chunk| !chunk.trim().is_empty()).collect()
}

/// `home` when it is an `http` or `https` address, the only kind a card links to.
pub(crate) fn web_home(home: Option<&str>) -> Option<&str> {
    home.filter(|home| home.starts_with("https://") || home.starts_with("http://"))
}

/// `text` cut to `max` characters, with `…` when it was longer.
pub(crate) fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut cut: String = text.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

// ---------------------------------------------------------------------------
// The chat screen's words
// ---------------------------------------------------------------------------

/// A piece of a line, which each platform renders in its own markup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Piece {
    /// Text that may come from a model, a tool or a person: escaped by the platform (S-I9).
    Plain(String),
    /// A heading, escaped too.
    Bold(String),
    /// A command the person types, such as `adk-rust pair accept ABCD-2345`: the gateway's
    /// own words, shown as code.
    Code(String),
    /// "Open in home", linking to an `http` or `https` address.
    Link {
        /// The address.
        url: String,
        /// The words.
        label: String,
    },
}

/// A message in the chat screen's words (spec 3.3 §5.3, §8): its lines.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Words {
    pub(crate) lines: Vec<Vec<Piece>>,
}

impl Words {
    fn line(mut self, pieces: Vec<Piece>) -> Self {
        self.lines.push(pieces);
        self
    }

    fn plain(text: impl Into<String>) -> Self {
        Self::default().line(vec![Piece::Plain(text.into())])
    }

    /// `sentence`, then "Open in home" linking to `home` when it is a web address.
    fn with_home(sentence: &str, home: Option<&str>) -> Self {
        let words = Self::plain(sentence);
        match web_home(home) {
            Some(url) => words.line(vec![Piece::Link {
                url: url.to_string(),
                label: words::OPEN_IN_HOME.to_string(),
            }]),
            None => words,
        }
    }

    /// The link the message carries, if any.
    pub(crate) fn link(&self) -> Option<(&str, &str)> {
        self.lines.iter().flatten().find_map(|piece| match piece {
            Piece::Link { url, label } => Some((url.as_str(), label.as_str())),
            _ => None,
        })
    }

    /// The message without its link line.
    #[cfg(any(feature = "slack", feature = "discord", feature = "matrix"))]
    pub(crate) fn without_link(&self) -> Self {
        let lines = self
            .lines
            .iter()
            .filter(|line| !line.iter().all(|piece| matches!(piece, Piece::Link { .. })))
            .cloned()
            .collect();
        Self { lines }
    }

    /// The message as plain text, each piece as it reads, a link as "Open in home: url".
    #[cfg(any(feature = "slack", feature = "matrix", test))]
    pub(crate) fn text(&self) -> String {
        self.render(|piece| match piece {
            Piece::Plain(text) | Piece::Bold(text) | Piece::Code(text) => text.clone(),
            Piece::Link { url, label } => format!("{label}: {url}"),
        })
    }

    /// The message with each piece rendered by `render`, its lines joined by newlines.
    pub(crate) fn render(&self, mut render: impl FnMut(&Piece) -> String) -> String {
        self.lines
            .iter()
            .map(|line| line.iter().map(&mut render).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// An answer a card offers, as a button: the card's own words for it and the data it
/// carries back, `y:` or `n:`, the version and the decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Button {
    pub(crate) answer: ChannelAnswer,
    pub(crate) label: &'static str,
    pub(crate) data: String,
}

/// The buttons of `card`, one for each answer it offers and no others; empty when an answer
/// does not fit a button, so the card is answered in the home.
pub(crate) fn buttons(card: &DecisionCard) -> Vec<Button> {
    let buttons: Option<Vec<Button>> = card
        .answers
        .iter()
        .map(|answer| {
            callback_data(*answer, &card.decision, card.version).map(|data| Button {
                answer: *answer,
                // The card's own words: "Don't add" on a suggested agent (spec 3.3 §7.2).
                label: card.label(*answer),
                data,
            })
        })
        .collect();
    buttons.unwrap_or_default()
}

/// A decision card as the chat screen of spec 3.3 §5.3 draws it: "Waiting for you · since
/// 18:00", the heading, the kernel's masked summary when a route wrote one, "Finance · Medium
/// risk", why it is answered in the home when it offers no button, and "Open in home".
/// Nothing of the call's arguments is added to what the kernel's card carries (S-I5).
pub(crate) fn card_words(
    card: &DecisionCard,
    heading: &Heading,
    offset: Option<FixedOffset>,
) -> Words {
    let mut words = Words::plain(format!(
        "{} · {}",
        words::WAITING_FOR_YOU,
        words::since(&hh_mm(card.asked_at, offset))
    ));
    if !heading.title.trim().is_empty() {
        words = words.line(vec![Piece::Bold(clean_outbound(&heading.title))]);
    }
    if let Some(detail) = &heading.detail {
        words = words.line(vec![Piece::Plain(clean_outbound(detail))]);
    }
    words = words.line(vec![Piece::Plain(format!(
        "{} · {}",
        clean_outbound(&humanize(&card.agent)),
        Risk::from(card.risk).words()
    ))]);
    if card.answers.is_empty() {
        words = words.line(vec![Piece::Plain(words::NEEDS_A_SCREEN.to_string())]);
    } else if buttons(card).is_empty() {
        // The decision's id is too long for a button: it is answered in the home.
        words = words.line(vec![Piece::Plain(ChannelNotice::DecideInHome { home: None }.text())]);
    }
    if let Some(url) = web_home(card.home.as_deref()) {
        words = words.line(vec![Piece::Link {
            url: url.to_string(),
            label: words::OPEN_IN_HOME.to_string(),
        }]);
    }
    words
}

/// "All agents stopped. Use Resume all in the home app." (spec 3.3 §5.3, S-I10).
pub(crate) fn stopped_words(notice: &StopNotice) -> Words {
    Words::with_home(words::ALL_AGENTS_STOPPED_RESUME_IN_HOME, notice.home.as_deref())
}

/// A notice in the words of the chat screen (spec 3.3 §5.3, §8), every time at `offset`.
/// `replying` is set when the message answers the card it is about, so a decided line need
/// not repeat its summary.
pub(crate) fn notice_words(
    notice: &ChannelNotice,
    offset: Option<FixedOffset>,
    now: DateTime<Utc>,
    replying: bool,
) -> Words {
    match notice {
        ChannelNotice::PairingCode { code, expires_at } => Words::default().line(vec![
            Piece::Plain("This account isn't paired yet. To pair it, run ".into()),
            Piece::Code(format!("adk-rust pair accept {}", clean_outbound(code))),
            Piece::Plain(format!(" where your agents run, before {}.", hh_mm(*expires_at, offset))),
        ]),
        ChannelNotice::PairingWaiting { expires_at } => Words::plain(format!(
            "This account is waiting to be paired. Use the code you were sent before {}.",
            hh_mm(*expires_at, offset)
        )),
        ChannelNotice::AlreadyDecided { by, said, at } => {
            let who = by.as_deref().unwrap_or("You");
            let line = match at {
                Some(at) => {
                    format!("Decided already: {who} said {said} at {}.", hh_mm(*at, offset))
                }
                None => format!("Decided already: {who} said {said}."),
            };
            Words::plain(clean_outbound(&line))
        }
        ChannelNotice::Decided { answer, summary } => {
            let line = match answer {
                ChannelAnswer::AllowOnce => format!("You allowed it once · {}", hh_mm(now, offset)),
                ChannelAnswer::Deny => format!("You denied it · {}", hh_mm(now, offset)),
                _ => return Words::plain(clean_outbound(&notice.text())),
            };
            if replying {
                Words::plain(line)
            } else {
                Words::plain(line).line(vec![Piece::Plain(clean_outbound(summary))])
            }
        }
        ChannelNotice::Stopped { home } => {
            Words::with_home(words::ALL_AGENTS_STOPPED_RESUME_IN_HOME, home.as_deref())
        }
        ChannelNotice::DecideInHome { home } => {
            Words::with_home(&ChannelNotice::DecideInHome { home: None }.text(), home.as_deref())
        }
        ChannelNotice::ResumeRefused { home } => {
            Words::with_home(&ChannelNotice::ResumeRefused { home: None }.text(), home.as_deref())
        }
        other => Words::plain(clean_outbound(&other.text())),
    }
}

// ---------------------------------------------------------------------------
// The update in the gateway's hands
// ---------------------------------------------------------------------------

tokio::task_local! {
    /// The update the gateway is handling, while it handles it.
    pub(crate) static HANDLING: Arc<Handling>;
}

/// An update in the gateway's hands: whose it is and, for a pressed answer, its card.
pub(crate) struct Handling {
    /// The adapter's channel.
    pub(crate) channel: &'static str,
    /// The thread it came from.
    pub(crate) thread: String,
    /// The account.
    pub(crate) account: String,
    /// Whether the account is paired, or its pairing could not be read.
    pub(crate) paired: bool,
    /// Where paired accounts are kept, to name the person who answered.
    pub(crate) bindings: Arc<dyn ChannelBindingStore>,
    /// A pressed answer's card and decision.
    pub(crate) press: Option<PressedCard>,
}

/// The card a pressed answer came from.
pub(crate) struct PressedCard {
    /// The card's message on the platform, which a notice about it replies to.
    pub(crate) card: Option<String>,
    /// The decision the answer is for.
    pub(crate) decision: String,
}

impl Handling {
    /// The update `inbound` of `channel` in `gateway`'s hands.
    pub(crate) fn of(
        channel: &'static str,
        gateway: &ChannelGateway,
        inbound: &ChannelInbound,
        paired: bool,
        press: Option<PressedCard>,
    ) -> Arc<Self> {
        Arc::new(Self {
            channel,
            thread: inbound.thread.clone(),
            account: inbound.account.clone(),
            paired,
            bindings: Arc::clone(gateway.bindings()),
            press,
        })
    }
}

/// The update of `channel` being handled for `thread`, if any.
pub(crate) fn handling(channel: &str, thread: &str) -> Option<Arc<Handling>> {
    HANDLING
        .try_with(Arc::clone)
        .ok()
        .filter(|handling| handling.channel == channel && handling.thread == thread)
}

/// Whether a send of `channel` to `thread` waits out a rate limit: not when it answers an
/// account that is not paired, so a stranger's flood holds up no one.
pub(crate) fn patient(channel: &str, thread: &str) -> bool {
    handling(channel, thread).is_none_or(|handling| handling.paired)
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

/// Whether `account` on `channel` is paired; `true` when its pairing could not be read, since
/// a store's failure is not the account's doing.
pub(crate) async fn is_paired(gateway: &ChannelGateway, channel: &str, account: &str) -> bool {
    paired_binding(gateway, channel, account).await.map_or(true, |binding| binding.is_some())
}

/// The active binding of `account` on `channel`, or the store's error.
pub(crate) async fn paired_binding(
    gateway: &ChannelGateway,
    channel: &str,
    account: &str,
) -> std::result::Result<Option<ChannelBinding>, ()> {
    let id = ChannelBinding::id_for(channel, account);
    match gateway.bindings().get_binding(&id).await {
        Ok(binding) => Ok(binding.filter(ChannelBinding::is_active)),
        Err(error) => {
            tracing::debug!(channel.name = channel, error = %error, "a chat account's pairing could not be read");
            Err(())
        }
    }
}

// ---------------------------------------------------------------------------
// Handing updates over
// ---------------------------------------------------------------------------

/// One update an adapter read: what it gives the gateway, and the platform's press to
/// acknowledge or the card it answers.
pub(crate) struct Update<P> {
    pub(crate) inbound: Option<ChannelInbound>,
    pub(crate) press: Option<P>,
}

/// An adapter as [`dispatch`] drives it.
#[async_trait]
pub(crate) trait Taker: Send + Sync + 'static {
    /// The platform's press.
    type Press: Send + Sync + 'static;

    /// The adapter's channel.
    fn channel_name(&self) -> &'static str;

    /// Hands one update to the gateway.
    async fn take(
        &self,
        gateway: &ChannelGateway,
        update: &Update<Self::Press>,
        paired: bool,
    ) -> adk_kernel::Result<()>;

    /// Tells a paired person that their update reached no agent.
    async fn not_taken(&self, update: &Update<Self::Press>);
}

struct Queued<P> {
    update: Update<P>,
    attempts: u32,
    ready_at: Instant,
}

impl<P> Queued<P> {
    fn new(update: Update<P>) -> Self {
        Self { update, attempts: 0, ready_at: Instant::now() }
    }
}

/// Hands the updates sent on the returned sender to `gateway` through `taker` (S-I6, S-I10):
/// whatever has arrived is taken as one batch, a paired account's updates first, each
/// account's in the order sent. An update the gateway fails on is taken again, after 1, 2, 4
/// and 8 seconds, only when it is a paired account's and the failure may clear, such as a
/// store outage; that account's later updates wait with it, while the other accounts' updates
/// are handed over. Any other failed update is dropped, and a paired person is told it was
/// not taken. The task ends when the sender is dropped and nothing waits.
pub(crate) fn dispatch<T: Taker>(
    taker: Arc<T>,
    gateway: ChannelGateway,
) -> (mpsc::UnboundedSender<Update<T::Press>>, JoinHandle<()>) {
    let (sender, mut receiver) = mpsc::unbounded_channel::<Update<T::Press>>();
    let task = tokio::spawn(async move {
        let mut queue: Vec<Queued<T::Press>> = Vec::new();
        let mut open = true;
        loop {
            if queue.is_empty() {
                if !open {
                    return;
                }
                match receiver.recv().await {
                    Some(update) => queue.push(Queued::new(update)),
                    None => return,
                }
            }
            while let Ok(update) = receiver.try_recv() {
                queue.push(Queued::new(update));
            }
            queue = take_batch(taker.as_ref(), &gateway, queue).await;
            let Some(next) = queue.iter().map(|queued| queued.ready_at).min() else { continue };
            if open {
                // An update that arrives meanwhile is handed over at once.
                tokio::select! {
                    () = tokio::time::sleep_until(next) => {}
                    received = receiver.recv() => match received {
                        Some(update) => queue.push(Queued::new(update)),
                        None => open = false,
                    },
                }
            } else {
                tokio::time::sleep_until(next).await;
            }
        }
    });
    (sender, task)
}

/// Takes one batch, as [`dispatch`] describes; returns the updates to take again.
async fn take_batch<T: Taker>(
    taker: &T,
    gateway: &ChannelGateway,
    queue: Vec<Queued<T::Press>>,
) -> Vec<Queued<T::Press>> {
    let now = Instant::now();
    let mut batch = Vec::with_capacity(queue.len());
    for queued in queue {
        let paired = match &queued.update.inbound {
            Some(inbound) => is_paired(gateway, taker.channel_name(), &inbound.account).await,
            None => false,
        };
        batch.push((queued, paired));
    }
    // Stable: a paired account's updates first, each account's in the order sent.
    batch.sort_by_key(|(_, paired)| !*paired);
    let mut held: HashSet<String> = HashSet::new();
    let mut kept = Vec::new();
    for (mut queued, paired) in batch {
        let account = queued.update.inbound.as_ref().map(|inbound| inbound.account.clone());
        if account.as_ref().is_some_and(|account| held.contains(account)) {
            kept.push(queued);
            continue;
        }
        if queued.ready_at > now {
            held.extend(account);
            kept.push(queued);
            continue;
        }
        let Err(error) = taker.take(gateway, &queued.update, paired).await else { continue };
        queued.attempts += 1;
        let attempt = queued.attempts;
        if paired && may_clear(&error) && attempt < UPDATE_ATTEMPTS {
            tracing::warn!(channel.name = taker.channel_name(), error = %error, attempt, "the gateway could not take a chat update; taking it again");
            queued.ready_at = Instant::now() + MIN_BACKOFF * 2u32.pow(attempt - 1);
            held.extend(account);
            kept.push(queued);
            continue;
        }
        tracing::warn!(channel.name = taker.channel_name(), error = %error, attempt, "the gateway could not take a chat update; dropping it");
        if paired {
            taker.not_taken(&queued.update).await;
        }
    }
    kept
}

/// Aborts a task when dropped, so a stopped connection stops what it started.
pub(crate) struct AbortOnDrop(pub(crate) JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// A running chat adapter: its connection to the platform, or its webhook's handing over
/// (spec 3.5 §10 X12). Dropping it stops the adapter.
///
/// # Example
///
/// ```rust,no_run
/// use adk_surface::channels::ChannelConnection;
///
/// async fn stop<E: std::fmt::Debug>(connection: ChannelConnection<E>) {
///     connection.stop();
///     assert!(connection.join().await.is_ok());
/// }
/// ```
pub struct ChannelConnection<E> {
    task: Option<JoinHandle<std::result::Result<(), E>>>,
}

impl<E> fmt::Debug for ChannelConnection<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChannelConnection").field("running", &!self.is_finished()).finish()
    }
}

impl<E: Send + 'static> ChannelConnection<E> {
    pub(crate) fn spawn(
        future: impl Future<Output = std::result::Result<(), E>> + Send + 'static,
    ) -> Self {
        Self { task: Some(tokio::spawn(future)) }
    }
}

impl<E> ChannelConnection<E> {
    /// Stops the adapter. The gateway it was started with is dropped with it, so its replies
    /// and cards stop too unless the caller kept a handle.
    pub fn stop(&self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }

    /// Whether the adapter stopped: stopped, or ended by a refused token.
    pub fn is_finished(&self) -> bool {
        self.task.as_ref().is_none_or(JoinHandle::is_finished)
    }

    /// Waits for the adapter to stop.
    ///
    /// # Errors
    ///
    /// Returns the platform's error when it refused the adapter's token for good; a stopped
    /// adapter ends with `Ok`.
    pub async fn join(mut self) -> std::result::Result<(), E> {
        let Some(task) = self.task.take() else { return Ok(()) };
        match task.await {
            Ok(result) => result,
            Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
            Err(_) => Ok(()),
        }
    }
}

impl<E> Drop for ChannelConnection<E> {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

// ---------------------------------------------------------------------------
// Cards and answers
// ---------------------------------------------------------------------------

/// What every adapter's builder takes beyond the platform's credentials (spec 3.3 §7.1,
/// §8.1, §11), and what it does with them.
#[derive(Clone, Default)]
pub(crate) struct Kit {
    pub(crate) offset: Option<FixedOffset>,
    pub(crate) approvals: Option<Arc<dyn ApprovalService>>,
    pub(crate) answers: Option<Arc<dyn AnswerLog>>,
    pub(crate) audit: Option<AuditTrail>,
    pub(crate) card_hints: Vec<Arc<dyn crate::hints::CardHints>>,
}

impl Kit {
    /// The heading of `card` and the card as it is offered: headed by the call in plain
    /// words, or a feature's chat heading, with no answer when a hint marks the call as
    /// spending money (spec 3.3 §7.1, §7.4 rule 4).
    pub(crate) async fn prepare(&self, card: &DecisionCard) -> (Heading, DecisionCard) {
        let request = match &self.approvals {
            Some(approvals) => approvals.get(&card.decision).await.unwrap_or_else(|error| {
                tracing::warn!(error = %error, "a card's decision could not be read; headed by its summary");
                None
            }),
            None => None,
        };
        let mut heading = heading(card, request.as_ref());
        let mut card = card.clone();
        if let Some(request) = request.as_ref().filter(|request| request.id == card.decision) {
            let mut headed = false;
            for hints in &self.card_hints {
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
        (heading, card)
    }

    /// Before a notice to `thread` is sent: when it says an answer pressed there decided its
    /// decision, the answer is recorded with where it was given (spec 3.3 §8.1, §11), so
    /// neither a slow platform nor a stop loses it. Returns the pressed card's message, which
    /// the notice replies to.
    pub(crate) async fn before_notice(
        &self,
        channel: &'static str,
        label: &'static str,
        thread: &str,
        notice: &ChannelNotice,
    ) -> Option<String> {
        let handling = handling(channel, thread)?;
        let press = handling.press.as_ref()?;
        if let ChannelNotice::Decided { answer, .. } = notice {
            self.record_answer(channel, label, &handling, &press.decision, *answer).await;
        }
        press.card.clone()
    }

    /// Records that the person of `handling`'s account answered `decision` in `label`.
    async fn record_answer(
        &self,
        channel: &str,
        label: &str,
        handling: &Handling,
        decision: &str,
        answer: ChannelAnswer,
    ) {
        let answer = match answer {
            ChannelAnswer::AllowOnce => Answer::AllowOnce,
            ChannelAnswer::Deny => Answer::Deny,
            _ => return,
        };
        let id = ChannelBinding::id_for(channel, &handling.account);
        let binding = match handling.bindings.get_binding(&id).await {
            Ok(Some(binding)) => binding,
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(channel.name = channel, error = %error, "a chat answer could not be recorded");
                return;
            }
        };
        let record = AnswerRecord {
            approval_id: decision.to_string(),
            by: SubjectRef::human(binding.user_id),
            via: Via { surface: SurfaceKind::Chat, origin: Origin::Chat { channel: label.into() } },
            answer,
            at: Utc::now(),
        };
        if let Some(log) = &self.answers
            && let Err(error) = log.record(&record).await
        {
            tracing::warn!(channel.name = channel, error = %error, "a chat answer could not be kept in the answer log");
        }
        let request = match (&self.audit, &self.approvals) {
            (Some(_), Some(approvals)) => approvals.get(decision).await.ok().flatten(),
            _ => None,
        };
        let title = match &request {
            Some(request) => crate::hints::named(&self.card_hints, request).await,
            None => None,
        };
        if let Some(trail) = &self.audit
            && let Err(error) = trail.record(answer_record(&record, request.as_ref(), title)).await
        {
            tracing::warn!(channel.name = channel, error = %error, "a chat answer's audit record could not be written");
        }
    }
}

/// The `surface.decide` record of an answer given in chat, as the engine writes its own
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

/// Implements the builder methods every adapter shares over its `kit` field.
macro_rules! kit_builder {
    ($adapter:literal) => {
        /// Shows every time — "since 18:00", "07:31", a pairing code's "before 08:31" — at
        /// `offset` east of UTC instead of in the host's time zone.
        #[must_use]
        pub fn utc_offset(mut self, offset: chrono::FixedOffset) -> Self {
            self.kit.offset = Some(offset);
            self
        }

        /// Reads each card's decision from `approvals`, the kernel's approval service, so the
        /// card is headed by the call in plain words — "Write file", "Send email" — with a
        /// summary an approval route wrote under it (spec 3.3 §7.1, §8). Without it, a card
        /// is headed by the kernel's masked summary.
        #[must_use]
        pub fn approvals(mut self, approvals: std::sync::Arc<dyn adk_core::ApprovalService>) -> Self {
            self.kit.approvals = Some(approvals);
            self
        }

        #[doc = concat!("Records each answer given in ", $adapter, " in `log`, the engine's [`AnswerLog`](crate::AnswerLog), so every")]
        #[doc = concat!("other surface shows \"You said yes in ", $adapter, " at 07:31\" (spec 3.3 §8.1). Give the log")]
        /// the engine was built with.
        #[must_use]
        pub fn answer_log(mut self, log: std::sync::Arc<dyn crate::answers::AnswerLog>) -> Self {
            self.kit.answers = Some(log);
            self
        }

        #[doc = concat!("Writes a `surface.decide` record of each answer given in ", $adapter, " to `trail`, the")]
        /// engine's audit trail, so an [`AuditAnswerLog`](crate::AuditAnswerLog) over the same
        /// store finds how it was given after a restart (spec 3.3 §11).
        #[must_use]
        pub fn audit(mut self, trail: adk_core::audit_log::AuditTrail) -> Self {
            self.kit.audit = Some(trail);
            self
        }

        /// Heads a card with the chat heading a feature's [`CardHints`](crate::CardHints) give
        /// its decision, in the order added, and offers no answer on a card a hint marks as
        /// spending money (spec 3.3 §7.1, §7.4 rule 4). Needs [`approvals`](Self::approvals).
        #[must_use]
        pub fn card_hints(mut self, hints: std::sync::Arc<dyn crate::hints::CardHints>) -> Self {
            self.kit.card_hints.push(hints);
            self
        }
    };
}
pub(crate) use kit_builder;

// ---------------------------------------------------------------------------
// Addresses
// ---------------------------------------------------------------------------

/// Checks that `base` is an `https` address, or `http` (and `ws` with `websocket`) on
/// loopback only, since a token travels with every call. `what` names the address in the
/// error, such as "the Slack API address".
pub(crate) fn checked_base(base: &str, what: &str, websocket: bool) -> Result<String, String> {
    let base = base.trim_end_matches('/');
    let url = reqwest::Url::parse(base)
        .map_err(|error| format!("{what} `{base}` is not a URL: {error}"))?;
    let secure = if websocket { "wss" } else { "https" };
    let plain = if websocket { "ws" } else { "http" };
    match url.scheme() {
        scheme if scheme == secure => Ok(base.to_string()),
        scheme if scheme == plain && is_loopback(&url) => Ok(base.to_string()),
        scheme if scheme == plain => Err(format!(
            "{what} `{base}` must use {secure}: the token travels with every call (plain \
             {plain} is accepted on loopback only)"
        )),
        other => Err(format!("{what} `{base}` uses `{other}`: use {secure}")),
    }
}

/// Whether `url` names this machine.
fn is_loopback(url: &reqwest::Url) -> bool {
    let host = url.host_str().unwrap_or_default();
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host == "localhost"
        || host.ends_with(".localhost")
        || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// Checks that `token` is non-empty printable ASCII without spaces, as every platform's
/// tokens are, so it can travel in a header. `what` names it in the error.
pub(crate) fn checked_token(token: &str, what: &str) -> Result<(), String> {
    if token.is_empty() || !token.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(format!("{what} is empty or malformed: copy it again from the platform"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use adk_core::{
        ActingContext, AgentPrincipal, ApprovalPolicy, ApprovalRisk, ApprovalScope,
        ApproverSelector, ToolConfirmationRequest,
    };

    use super::*;

    fn utc() -> Option<FixedOffset> {
        FixedOffset::east_opt(0)
    }

    fn card(risk: ApprovalRisk) -> DecisionCard {
        let call =
            ToolConfirmationRequest::new("send_email", json!({ "to": "client@example.com" }));
        let request = ApprovalRequest::new(
            "apr_1",
            ApprovalScope::new("adk-rust", "ana", "s1"),
            &call,
            ApprovalPolicy::new([ApproverSelector::group("operator")]),
        )
        .with_summary("Send the invoice to client@example.com")
        .with_risk(risk)
        .with_acting(ActingContext::new(AgentPrincipal::derived("adk-rust", "finance")));
        DecisionCard::from_request(&request, Some("https://home.example/ui/"))
    }

    #[test]
    fn a_card_reads_as_the_chat_screen_and_carries_no_argument() {
        let card = card(ApprovalRisk::Medium);
        let words = card_words(&card, &heading(&card, None), utc());
        let asked = hh_mm(card.asked_at, utc());
        assert_eq!(
            words.text(),
            format!(
                "Waiting for you · since {asked}\nSend the invoice to […]\nFinance · Medium \
                 risk\nOpen in home: https://home.example/ui/"
            )
        );
        assert!(!words.text().contains("client@example.com"));
        assert_eq!(
            buttons(&card),
            [
                Button {
                    answer: ChannelAnswer::AllowOnce,
                    label: "Allow once",
                    data: format!("y:{}:apr_1", card.version)
                },
                Button {
                    answer: ChannelAnswer::Deny,
                    label: "Deny",
                    data: format!("n:{}:apr_1", card.version)
                },
            ]
        );
        assert_eq!(words.link(), Some(("https://home.example/ui/", "Open in home")));
        #[cfg(any(feature = "slack", feature = "discord", feature = "matrix"))]
        assert!(words.without_link().link().is_none());
        let high = self::card(ApprovalRisk::High);
        let words = card_words(&high, &heading(&high, None), utc());
        assert!(buttons(&high).is_empty());
        assert!(words.text().contains(words::NEEDS_A_SCREEN), "{}", words.text());
    }

    /// A chat answer is on record with the item its call acts on and the card's summary, as
    /// the engine records its own answers (spec 3.3 §8, §11).
    #[test]
    fn a_chat_answer_names_the_item_a_hint_names() {
        let call = ToolConfirmationRequest::new("verb_snooze", json!({ "item": "news-digest" }));
        let request = ApprovalRequest::new(
            "apr_1",
            ApprovalScope::new("adk-rust", "ana", "s1"),
            &call,
            ApprovalPolicy::new([ApproverSelector::group("operator")]),
        )
        .with_summary("Move News digest to 19:00");
        let given = AnswerRecord {
            approval_id: "apr_1".into(),
            by: SubjectRef::human("ana"),
            via: Via {
                surface: SurfaceKind::Chat,
                origin: Origin::Chat { channel: "Slack".into() },
            },
            answer: Answer::Deny,
            at: Utc::now(),
        };
        let named = answer_record(&given, Some(&request), Some("News digest".into()));
        assert_eq!(
            (named.ext.get("title"), named.ext.get("summary")),
            (Some(&json!("News digest")), Some(&json!("Move News digest to 19:00")))
        );
        let unnamed = answer_record(&given, Some(&request), None);
        assert_eq!((unnamed.ext.get("title"), unnamed.ext.get("summary")), (None, None));
    }

    #[test]
    fn invisible_characters_never_reach_the_agent() {
        assert_eq!(
            clean_inbound("pay\u{200B}ment\u{202E} now\u{E0041} "),
            Some("payment now".into())
        );
        assert_eq!(clean_inbound("\u{200F}\u{FEFF} "), None);
        assert_eq!(clean_inbound(&"x".repeat(5000)).unwrap().chars().count(), MAX_INBOUND_CHARS);
        assert_eq!(chunks("  Done.\u{202E}\n ", 10), ["Done."]);
        assert!(chunks(" \u{200E} ", 10).is_empty());
    }

    #[test]
    fn addresses_and_tokens_are_checked() {
        assert!(checked_base("https://slack.com/api/", "x", false).is_ok());
        assert!(checked_base("http://127.0.0.1:9", "x", false).is_ok());
        assert!(checked_base("http://example.com", "x", false).is_err());
        assert!(checked_base("ws://[::1]:9/socket", "x", true).is_ok());
        assert!(checked_base("ws://example.com/socket", "x", true).is_err());
        assert!(checked_base("wss://example.com/socket", "x", true).is_ok());
        assert!(checked_token("xoxb-1-abc", "x").is_ok());
        for bad in ["", "a b", "a\nb", "é"] {
            assert!(checked_token(bad, "x").is_err(), "{bad:?}");
        }
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("abc", 4), "abc");
    }
}
