//! The chat channel contract (spec 3.3 §5.3, §10 K1, §11 S-I5, S-I6, S-I10; feature
//! `channels`).
//!
//! A chat thread is the doorbell and the conversation: a paired account talks to its agents,
//! gets decision cards, says yes once or no, and stops everything. The kernel owns the rules;
//! a platform adapter (Telegram first, in `adk-surface`) only moves messages. An adapter
//! implements [`ChannelSink`] for what goes out and hands what comes in to
//! [`ChannelGateway::receive`] as a [`ChannelInbound`]; it passes the contract suite in
//! `conformance::channel_contract` (feature `conformance`).
//!
//! | Rule | What the gateway does |
//! |---|---|
//! | Only paired accounts reach an agent (S-I6) | An unpaired account's message, or answer, gets a pairing code ([`ChannelNotice::PairingCode`]) and starts nothing; the person accepts the code where the kernel runs (`adk-rust pair accept`, `POST /api/kernel/channels/pair`) |
//! | One code per account, at most so many waiting | The gateway serializes unpaired accounts' messages, so concurrent messages from one account get one code and the waiting limit holds; the operator dismisses a waiting account (`adk-rust pair revoke`, `POST /api/kernel/channels/waiting/{id}/dismiss`) |
//! | A paired account's message is a turn | It starts a turn as the paired person, a `Human` subject, in the thread's session, keyed by the platform's message id so a redelivery starts nothing; the turn's reply goes back to the same thread |
//! | Cards carry no arguments (S-I5) | A [`DecisionCard`] holds the agent, a short summary with every argument value masked, and the risk; never the call's arguments |
//! | Chat says yes once or no, never money (§7.2, §7.4) | Answers are [`ChannelAnswer::AllowOnce`] and [`ChannelAnswer::Deny`] at low and medium risk only, through `ApprovalService::decide` with the version the card showed (S-I2); a call that moves money ([`ChannelHost::moves_money`], [`ChannelGatewayBuilder::money_tools`]) and high risk link to the home |
//! | Only who may answer gets buttons | A card goes to the decision's person when the policy lets that person answer it, and its answers only when chat may decide it |
//! | A second answer is shown, never retried (S-I2) | A version conflict answers [`ChannelNotice::AlreadyDecided`], or a fresh card when the decision still waits; an answer that counts toward a quorum, or one from someone the policy does not ask, answers [`ChannelNotice::WaitingForOthers`] |
//! | Stop all from anywhere; resume only at home (S-I10) | "stop all" freezes the kernel and answers [`StopNotice`]; "resume" is refused with [`ChannelNotice::ResumeRefused`] |
//! | Nothing notifies during quiet time | A card for a decision asked during quiet time is delivered when quiet time ends, if it still waits |
//!
//! Every answer, pairing request, notification, stop and refusal writes a `kernel.channel`
//! audit record with `surface: chat`, the channel and the account, before it acts.
//!
//! | Backend of [`ChannelBindingStore`] | Type | Feature |
//! |---|---|---|
//! | In-memory | [`InMemoryChannelBindingStore`] | — |
//! | SQLite | `SqliteChannelBindingStore` | `sqlite` (default) |
//! | PostgreSQL | `PostgresChannelBindingStore` | `postgres` |
//! | Redis | `RedisChannelBindingStore` | `redis` |
//!
//! Text from an agent, a tool or a chat platform reaches a sink as it is; the adapter
//! escapes it for its platform before display (S-I9).

use std::fmt;
use std::sync::Arc;

use adk_core::audit_log::{AuditActor, AuditTrail};
use adk_core::{ApprovalRequest, ApprovalRisk, ApprovalService, NewRun, RunRecord};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::control::api::{ChannelAccountState, ChannelAccountView, ChannelPairingView};
use crate::error::{KernelError, Result};
use crate::pairing::secret;

mod gateway;
mod host;
mod memory;
#[cfg(feature = "postgres")]
mod postgres;
#[cfg(feature = "redis")]
mod redis;
#[cfg(any(feature = "sqlite", feature = "postgres"))]
mod sql;
#[cfg(feature = "sqlite")]
mod sqlite;
mod testing;

pub use gateway::{ChannelDoorbell, ChannelGateway, ChannelGatewayBuilder};
pub use memory::InMemoryChannelBindingStore;
#[cfg(feature = "postgres")]
pub use postgres::PostgresChannelBindingStore;
#[cfg(feature = "redis")]
pub use redis::RedisChannelBindingStore;
#[cfg(feature = "sqlite")]
pub use sqlite::SqliteChannelBindingStore;
pub use testing::{Delivered, InMemoryChannel};

/// The longest summary a card carries, in characters.
const MAX_SUMMARY_CHARS: usize = 160;
/// The longest display name kept for an account.
const MAX_NAME_CHARS: usize = 64;

/// What a person can answer from a chat thread (spec 3.3 §7.2, §8.2).
///
/// # Example
///
/// ```rust
/// use adk_kernel::channels::ChannelAnswer;
///
/// assert_eq!(ChannelAnswer::AllowOnce.label(), "Allow once");
/// assert_eq!(serde_json::to_value(ChannelAnswer::Deny).unwrap(), "deny");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum ChannelAnswer {
    /// Run this one call.
    AllowOnce,
    /// Refuse it.
    Deny,
}

impl ChannelAnswer {
    /// The button's words (spec 3.3 §8.2).
    pub fn label(self) -> &'static str {
        match self {
            Self::AllowOnce => "Allow once",
            Self::Deny => "Deny",
        }
    }
}

/// What arrived from a chat thread (spec 3.3 §10 K1): untrusted input (S-I6).
///
/// # Example
///
/// ```rust
/// use adk_kernel::channels::{ChannelInbound, InboundBody};
///
/// let inbound = ChannelInbound::text("telegram", "4217", "4217", "msg-1", "what's on today?")
///     .with_display_name("Ana");
/// assert!(matches!(inbound.body, InboundBody::Text { .. }));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ChannelInbound {
    /// The channel, such as `telegram`: the [`ChannelSink::channel`] of its adapter.
    pub channel: String,
    /// The sending account on that channel.
    pub account: String,
    /// The account's display name, as the platform gave it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// The conversation the message is in; replies go back to it.
    pub thread: String,
    /// The platform's id of the message, unique per thread, so a redelivery starts nothing.
    pub message_id: String,
    /// What the person sent.
    pub body: InboundBody,
}

impl ChannelInbound {
    fn new(
        channel: impl Into<String>,
        account: impl Into<String>,
        thread: impl Into<String>,
        message_id: impl Into<String>,
        body: InboundBody,
    ) -> Self {
        Self {
            channel: channel.into(),
            account: account.into(),
            display_name: None,
            thread: thread.into(),
            message_id: message_id.into(),
            body,
        }
    }

    /// A typed message.
    pub fn text(
        channel: impl Into<String>,
        account: impl Into<String>,
        thread: impl Into<String>,
        message_id: impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        Self::new(channel, account, thread, message_id, InboundBody::Text { text: text.into() })
    }

    /// A pressed answer on the card for `decision` at `version`.
    pub fn answer(
        channel: impl Into<String>,
        account: impl Into<String>,
        thread: impl Into<String>,
        message_id: impl Into<String>,
        card: &DecisionCard,
        answer: ChannelAnswer,
    ) -> Self {
        let body =
            InboundBody::Answer { decision: card.decision.clone(), version: card.version, answer };
        Self::new(channel, account, thread, message_id, body)
    }

    /// A pressed "Stop all".
    pub fn stop_all(
        channel: impl Into<String>,
        account: impl Into<String>,
        thread: impl Into<String>,
        message_id: impl Into<String>,
    ) -> Self {
        Self::new(channel, account, thread, message_id, InboundBody::StopAll)
    }

    /// Sets the account's display name.
    #[must_use]
    pub fn with_display_name(mut self, name: impl Into<String>) -> Self {
        self.display_name = Some(name.into());
        self
    }
}

/// What a person sent from a chat thread (spec 3.3 §10 K1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
#[non_exhaustive]
pub enum InboundBody {
    /// A typed message. "stop all" and "/stop" stop everything; "resume" and "/resume" are
    /// refused.
    Text {
        /// The text, as typed.
        text: String,
    },
    /// An answer pressed on a decision card.
    Answer {
        /// The card's [`DecisionCard::decision`].
        decision: String,
        /// The card's [`DecisionCard::version`].
        version: u64,
        /// What was pressed.
        answer: ChannelAnswer,
    },
    /// A pressed "Stop all".
    StopAll,
    /// A pressed "Resume all", which a chat thread may not do (S-I10).
    ResumeAll,
}

/// A decision as a chat thread shows it (spec 3.3 §5.3, §7.1, S-I5): who asks, a short
/// summary, the risk, and the answers the thread may give. Never the call's arguments.
///
/// # Example
///
/// ```rust
/// use adk_core::{ApprovalPolicy, ApprovalRequest, ApprovalRisk, ApprovalScope, ApproverSelector, ToolConfirmationRequest};
/// use adk_kernel::channels::{ChannelAnswer, DecisionCard};
///
/// let call = ToolConfirmationRequest::new("send_email", serde_json::json!({"to": "client@example.com"}));
/// let request = ApprovalRequest::new("a1", ApprovalScope::new("adk-rust", "ana", "s1"), &call, ApprovalPolicy::new([ApproverSelector::group("operator")]))
///     .with_summary("Send the invoice to client@example.com")
///     .with_risk(ApprovalRisk::Medium);
/// let card = DecisionCard::from_request(&request, None);
/// assert_eq!(card.summary, "Send the invoice to […]");
/// assert_eq!(card.answers, [ChannelAnswer::AllowOnce, ChannelAnswer::Deny]);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct DecisionCard {
    /// The decision's id, which an answer carries back; never shown (spec 3.3 §8).
    pub decision: String,
    /// The decision's version when the card was made, which an answer carries back so a
    /// changed decision is not answered blind (S-I2); never shown.
    pub version: u64,
    /// The agent asking.
    pub agent: String,
    /// What will happen, in at most 160 characters, with every argument value masked as
    /// `[…]`: one of three or more characters wherever it appears, ignoring case, and a
    /// shorter one wherever it stands as a word or a number of its own, newlines and other
    /// control characters read as spaces (S-I5).
    pub summary: String,
    /// The risk.
    pub risk: ApprovalRisk,
    /// The answers the thread may give: allow once and deny at low and medium risk, none at
    /// high risk.
    pub answers: Vec<ChannelAnswer>,
    /// Where the decision is answered in full: the web home, when the gateway knows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home: Option<String>,
    /// When it was asked.
    pub asked_at: DateTime<Utc>,
}

impl DecisionCard {
    /// The card for `request`, linking to `home`.
    pub fn from_request(request: &ApprovalRequest, home: Option<&str>) -> Self {
        let mut answers = Vec::new();
        if chat_may_decide(request.risk) {
            if request.allowed_actions.contains(&adk_core::ApprovalAction::Approve) {
                answers.push(ChannelAnswer::AllowOnce);
            }
            if request.allowed_actions.contains(&adk_core::ApprovalAction::Reject) {
                answers.push(ChannelAnswer::Deny);
            }
        }
        Self {
            decision: request.id.clone(),
            version: request.version,
            agent: agent_name(request),
            summary: card_summary(&request.summary, &request.args_redacted),
            risk: request.risk,
            answers,
            home: home.map(str::to_string),
            asked_at: request.created_at,
        }
    }

    /// The card in plain words (spec 3.3 §8): who asks, what will happen, and the risk.
    pub fn text(&self) -> String {
        let mut text = format!(
            "{} is waiting for you\n{}\n{}",
            self.agent,
            self.summary,
            risk_words(self.risk)
        );
        if self.answers.is_empty() {
            match &self.home {
                Some(home) => text.push_str(&format!("\nDecide in the home: {home}")),
                None => text.push_str("\nDecide in the home"),
            }
        }
        text
    }
}

/// Whether a chat thread may answer a decision of `risk` (spec 3.3 §7.2: low and medium).
pub(crate) fn chat_may_decide(risk: ApprovalRisk) -> bool {
    matches!(risk, ApprovalRisk::Low | ApprovalRisk::Medium)
}

/// The risk in plain words.
fn risk_words(risk: ApprovalRisk) -> &'static str {
    match risk {
        ApprovalRisk::Low => "Low risk",
        ApprovalRisk::Medium => "Medium risk",
        ApprovalRisk::High => "High risk",
        ApprovalRisk::Critical => "Critical risk",
    }
}

/// The asking agent's name: the last part of its id, or "Your agent".
fn agent_name(request: &ApprovalRequest) -> String {
    let id = request.acting.as_ref().map(|acting| acting.agent.id.as_str()).unwrap_or_default();
    let name = id.rsplit(['/', ':']).next().unwrap_or_default();
    match secret::clean_name(name, MAX_NAME_CHARS) {
        name if name.is_empty() => "Your agent".to_string(),
        name => name,
    }
}

/// `summary` with every argument value of `args` masked as `[…]`, in at most
/// [`MAX_SUMMARY_CHARS`] characters (S-I5).
///
/// The summary and the values are read alike: control characters and runs of whitespace
/// become one space, so a value with a newline or a tab is found in a summary that quotes
/// it. A value of three or more characters is masked wherever it appears, ignoring case; a
/// shorter one, and `true` or `false`, wherever it stands as a word or a number of its own,
/// so `50` is masked in "Transfer 50 EUR" and "EUR50" but `5` is not taken out of "500".
/// Whole numbers of four or more digits are also found with thousands separators, as
/// `1,250`. Where masked values overlap or touch, one `[…]` covers them.
pub(crate) fn card_summary(summary: &str, args: &Value) -> String {
    let mut values = Vec::new();
    collect_values(args, &mut values);
    let text: Vec<char> = plain(summary).chars().collect();
    let folded: Vec<char> = text.iter().map(|c| fold(*c)).collect();
    let mut masked = vec![false; text.len()];
    for value in values {
        let needle: Vec<char> = plain(&value.text).chars().map(fold).collect();
        if needle.is_empty() || needle.len() > text.len() {
            continue;
        }
        let whole = value.whole_word || needle.len() < 3;
        for start in 0..=text.len() - needle.len() {
            let end = start + needle.len();
            if folded[start..end] == needle[..] && (!whole || stands_alone(&text, start, end)) {
                masked[start..end].fill(true);
            }
        }
    }
    let mut out = String::with_capacity(text.len());
    for (index, c) in text.iter().enumerate() {
        match (masked[index], index.checked_sub(1).is_some_and(|before| masked[before])) {
            (true, true) => {}
            (true, false) => out.push_str("[…]"),
            (false, _) => out.push(*c),
        }
    }
    let text = out.trim();
    if text.chars().count() <= MAX_SUMMARY_CHARS {
        return text.to_string();
    }
    let mut cut: String = text.chars().take(MAX_SUMMARY_CHARS - 1).collect();
    cut.push('…');
    cut
}

/// `text` with control characters and runs of whitespace as one space, trimmed.
fn plain(text: &str) -> String {
    text.split(|c: char| c.is_whitespace() || c.is_control())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// `c` in lower case, one character for one, so positions line up.
fn fold(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// What a character is, for telling where a short value stands on its own.
#[derive(PartialEq, Eq)]
enum CharClass {
    Digit,
    Letter,
    Other,
}

fn class(c: char) -> CharClass {
    if c.is_numeric() {
        CharClass::Digit
    } else if c.is_alphabetic() {
        CharClass::Letter
    } else {
        CharClass::Other
    }
}

/// Whether `text[start..end]` is not part of a longer word or number: the characters on
/// either side are of another kind than its own first and last.
fn stands_alone(text: &[char], start: usize, end: usize) -> bool {
    let edge = |inside: char, outside: Option<&char>| {
        class(inside) == CharClass::Other || outside.is_none_or(|c| class(*c) != class(inside))
    };
    edge(text[start], start.checked_sub(1).and_then(|before| text.get(before)))
        && edge(text[end - 1], text.get(end))
}

/// An argument value as the masking reads it.
struct ArgValue {
    text: String,
    /// Masked only where it stands on its own, whatever its length.
    whole_word: bool,
}

/// Every string, number and boolean of `value`, as text; whole numbers of four or more
/// digits also with thousands separators.
fn collect_values(value: &Value, out: &mut Vec<ArgValue>) {
    match value {
        Value::String(text) => out.push(ArgValue { text: text.clone(), whole_word: false }),
        Value::Number(number) => {
            let text = number.to_string();
            if let Some(whole) = number.as_i64().filter(|whole| whole.unsigned_abs() >= 1_000) {
                out.push(ArgValue { text: grouped(whole), whole_word: false });
            }
            out.push(ArgValue { text, whole_word: false });
        }
        Value::Bool(flag) => out.push(ArgValue { text: flag.to_string(), whole_word: true }),
        Value::Array(items) => items.iter().for_each(|item| collect_values(item, out)),
        Value::Object(fields) => fields.values().for_each(|item| collect_values(item, out)),
        Value::Null => {}
    }
}

/// `number` with a comma between each group of three digits, as `-1,250,000`.
fn grouped(number: i64) -> String {
    let digits = number.unsigned_abs().to_string();
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    if number < 0 { format!("-{out}") } else { out }
}

/// Everything was stopped from a chat thread (spec 3.3 S-I10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct StopNotice {
    /// When.
    pub at: DateTime<Utc>,
    /// Where everything is resumed, when the gateway knows the home.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home: Option<String>,
}

impl StopNotice {
    /// The notice in plain words.
    pub fn text(&self) -> String {
        match &self.home {
            Some(home) => format!("Stopped all agents. Resume all in the home: {home}"),
            None => "Stopped all agents. Resume all in the home.".to_string(),
        }
    }
}

/// Something the gateway tells a chat thread (spec 3.3 §5.3, §8).
///
/// # Example
///
/// ```rust
/// use adk_kernel::channels::ChannelNotice;
///
/// let notice = ChannelNotice::ResumeRefused { home: None };
/// assert_eq!(notice.text(), "Resuming happens only in the home.");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase", tag = "kind")]
#[non_exhaustive]
pub enum ChannelNotice {
    /// The account is not paired: its code, to be accepted where the kernel runs.
    PairingCode {
        /// The code, as `ABCD-EFGH-2345`.
        code: String,
        /// When it stops working.
        expires_at: DateTime<Utc>,
    },
    /// The account was sent a code that still works.
    PairingWaiting {
        /// When it stops working.
        expires_at: DateTime<Utc>,
    },
    /// Too many accounts wait to be paired; nothing was sent.
    PairingBusy,
    /// The answer was recorded.
    Decided {
        /// What was answered.
        answer: ChannelAnswer,
        /// The decision's summary.
        summary: String,
    },
    /// The decision was answered before this answer arrived (S-I2).
    AlreadyDecided {
        /// Who answered: `None` when the answer was the same person's.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        by: Option<String>,
        /// What they answered, in plain words, such as "yes".
        said: String,
        /// When.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at: Option<DateTime<Utc>>,
    },
    /// The decision changed since the card was sent; a fresh card follows.
    DecisionChanged,
    /// The decision is not one this thread may answer: high risk, or an answer it does not
    /// allow (spec 3.3 §7.2).
    DecideInHome {
        /// The web home, when the gateway knows it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        home: Option<String>,
    },
    /// The decision is no longer waiting, or is not this person's.
    DecisionGone,
    /// The decision waits for someone else: another approver after this person's answer
    /// counted, or an approver other than this person.
    WaitingForOthers {
        /// Whether this person's answer was counted.
        answered: bool,
    },
    /// A chat thread cannot resume (S-I10).
    ResumeRefused {
        /// The web home, when the gateway knows it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        home: Option<String>,
    },
    /// Everything is stopped, so the message started nothing.
    Stopped {
        /// The web home, when the gateway knows it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        home: Option<String>,
    },
    /// The turn failed.
    TurnFailed,
}

impl ChannelNotice {
    /// The notice in plain words (spec 3.3 §8).
    pub fn text(&self) -> String {
        let home = |home: &Option<String>| match home {
            Some(home) => format!(": {home}"),
            None => ".".to_string(),
        };
        match self {
            Self::PairingCode { code, expires_at } => format!(
                "This account isn't paired yet. To pair it, run `adk-rust pair accept {code}` where \
                 your agents run, before {}.",
                local_time(*expires_at)
            ),
            Self::PairingWaiting { expires_at } => format!(
                "This account is waiting to be paired. Use the code you were sent before {}.",
                local_time(*expires_at)
            ),
            Self::PairingBusy => {
                "Too many accounts are waiting to be paired. Try again later.".to_string()
            }
            Self::Decided { answer: ChannelAnswer::AllowOnce, summary } => {
                format!("Allowed once: {summary}")
            }
            Self::Decided { answer: ChannelAnswer::Deny, summary } => format!("Denied: {summary}"),
            Self::AlreadyDecided { by, said, at } => {
                let who = by.as_deref().unwrap_or("You");
                match at {
                    Some(at) => {
                        format!("Decided already: {who} said {said} at {}.", local_time(*at))
                    }
                    None => format!("Decided already: {who} said {said}."),
                }
            }
            Self::DecisionChanged => "This decision changed. Here it is again.".to_string(),
            Self::DecideInHome { home: link } => {
                format!("Decide this one in the home{}", home(link))
            }
            Self::DecisionGone => "That decision is no longer waiting for you.".to_string(),
            Self::WaitingForOthers { answered: true } => {
                "Your answer counts. The decision also waits for someone else.".to_string()
            }
            Self::WaitingForOthers { answered: false } => {
                "This decision waits for someone else to answer it.".to_string()
            }
            Self::ResumeRefused { home: link } => match link {
                Some(link) => format!("Resuming happens only in the home: {link}"),
                None => "Resuming happens only in the home.".to_string(),
            },
            Self::Stopped { home: link } => {
                format!("All agents are stopped. Resume all in the home{}", home(link))
            }
            Self::TurnFailed => {
                "Something went wrong. Open the home to see what happened.".to_string()
            }
        }
    }
}

/// `at` as `HH:MM` on this host's clock.
fn local_time(at: DateTime<Utc>) -> String {
    at.with_timezone(&chrono::Local).format("%H:%M").to_string()
}

/// Where a channel's messages go out (spec 3.3 §10 K1): implemented by each platform
/// adapter.
///
/// Every method sends to `thread`, a conversation of [`channel`](Self::channel). The kernel
/// sends structured values; the adapter renders them for its platform, escaping text
/// (S-I9), and offers a card's [`answers`](DecisionCard::answers) as buttons, no others.
/// The `text` method of each value gives plain words an adapter may use as they are.
///
/// # Example
///
/// ```rust
/// use adk_kernel::channels::{ChannelSink, DecisionCard, StopNotice};
/// use async_trait::async_trait;
///
/// struct Log;
///
/// #[async_trait]
/// impl ChannelSink for Log {
///     fn channel(&self) -> &str {
///         "log"
///     }
///
///     async fn send_message(&self, thread: &str, text: &str) -> adk_kernel::Result<()> {
///         tracing::info!(thread, text, "message");
///         Ok(())
///     }
///
///     async fn send_decision(&self, thread: &str, card: &DecisionCard) -> adk_kernel::Result<()> {
///         self.send_message(thread, &card.text()).await
///     }
///
///     async fn send_stopped(&self, thread: &str, notice: &StopNotice) -> adk_kernel::Result<()> {
///         self.send_message(thread, &notice.text()).await
///     }
/// }
/// ```
#[async_trait]
pub trait ChannelSink: Send + Sync {
    /// The channel's name, such as `telegram`, which its inbound messages carry.
    fn channel(&self) -> &str;

    /// Sends an agent's reply or other text.
    ///
    /// # Errors
    ///
    /// Returns the platform's error; the gateway logs it.
    async fn send_message(&self, thread: &str, text: &str) -> Result<()>;

    /// Sends a decision card: its summary, agent and risk, with its answers as buttons.
    ///
    /// # Errors
    ///
    /// Returns the platform's error.
    async fn send_decision(&self, thread: &str, card: &DecisionCard) -> Result<()>;

    /// Says that everything was stopped.
    ///
    /// # Errors
    ///
    /// Returns the platform's error.
    async fn send_stopped(&self, thread: &str, notice: &StopNotice) -> Result<()>;

    /// Sends a notice; by default as [`send_message`](Self::send_message) of its plain
    /// words.
    ///
    /// # Errors
    ///
    /// Returns the platform's error.
    async fn send_notice(&self, thread: &str, notice: &ChannelNotice) -> Result<()> {
        self.send_message(thread, &notice.text()).await
    }
}

/// A chat account paired to a person (spec 3.3 §10 K1): its messages are that person's turns
/// and its answers are that person's decisions, within what chat may decide.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ChannelBinding {
    /// The pairing's id, [`ChannelBinding::id_for`] the channel and account.
    pub id: String,
    /// The channel.
    pub channel: String,
    /// The account on it.
    pub account: String,
    /// The account's display name, as the platform gave it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// The thread decision cards go to: the one it asked to pair from.
    pub thread: String,
    /// The person it acts for: who accepted its code.
    pub user_id: String,
    /// Their approval groups when they accepted it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<String>,
    /// When it was paired.
    pub paired_at: DateTime<Utc>,
    /// When it was removed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<DateTime<Utc>>,
}

impl ChannelBinding {
    /// A binding of `account` on `channel` to `user_id`, delivering cards to `thread`.
    pub fn new(
        channel: impl Into<String>,
        account: impl Into<String>,
        thread: impl Into<String>,
        user_id: impl Into<String>,
        paired_at: DateTime<Utc>,
    ) -> Self {
        let (channel, account) = (channel.into(), account.into());
        Self {
            id: Self::id_for(&channel, &account),
            channel,
            account,
            display_name: None,
            thread: thread.into(),
            user_id: user_id.into(),
            groups: Vec::new(),
            paired_at,
            revoked_at: None,
        }
    }

    /// The id of the binding of `account` on `channel`: `ch-` and sixteen hex digits of a
    /// SHA-256 of both, so one account has one binding.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_kernel::channels::ChannelBinding;
    ///
    /// let id = ChannelBinding::id_for("telegram", "4217");
    /// assert!(id.starts_with("ch-"));
    /// assert_ne!(id, ChannelBinding::id_for("matrix", "4217"));
    /// ```
    pub fn id_for(channel: &str, account: &str) -> String {
        format!("ch-{}", &secret::digest(&format!("{channel}\0{account}"))[..16])
    }

    /// Sets the approval groups.
    #[must_use]
    pub fn with_groups(mut self, groups: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.groups = groups.into_iter().map(Into::into).collect();
        self
    }

    /// Whether the account reaches the agents.
    pub fn is_active(&self) -> bool {
        self.revoked_at.is_none()
    }

    /// The binding as the control plane lists it.
    pub fn view(&self) -> ChannelAccountView {
        ChannelAccountView {
            id: self.id.clone(),
            channel: self.channel.clone(),
            account: self.account.clone(),
            display_name: self.display_name.clone(),
            user_id: self.user_id.clone(),
            paired_at: self.paired_at,
            revoked_at: self.revoked_at,
            state: if self.is_active() {
                ChannelAccountState::Active
            } else {
                ChannelAccountState::Revoked
            },
        }
    }
}

/// An unpaired account's request to pair (spec 3.3 §10 K1): the hash of the code it was
/// sent, never the code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ChannelPairingRequest {
    /// The SHA-256 of the normalized code, in hex.
    pub code_hash: String,
    /// The channel.
    pub channel: String,
    /// The account.
    pub account: String,
    /// The account's display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// The thread it asked from, where cards will go.
    pub thread: String,
    /// When it asked.
    pub requested_at: DateTime<Utc>,
    /// When its code stops working.
    pub expires_at: DateTime<Utc>,
}

impl ChannelPairingRequest {
    /// A request from `account` on `channel` in `thread`, its code hashed `code_hash`.
    pub fn new(
        code_hash: impl Into<String>,
        channel: impl Into<String>,
        account: impl Into<String>,
        thread: impl Into<String>,
        requested_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Self {
        Self {
            code_hash: code_hash.into(),
            channel: channel.into(),
            account: account.into(),
            display_name: None,
            thread: thread.into(),
            requested_at,
            expires_at,
        }
    }

    /// The request as the control plane lists it.
    pub fn view(&self) -> ChannelPairingView {
        ChannelPairingView {
            id: ChannelBinding::id_for(&self.channel, &self.account),
            channel: self.channel.clone(),
            account: self.account.clone(),
            display_name: self.display_name.clone(),
            requested_at: self.requested_at,
            expires_at: self.expires_at,
        }
    }
}

/// Where paired chat accounts and their pairing requests are kept (spec 3.3 §10 K1), on the
/// backends the kernel's other stores use, with one conformance suite
/// (`channel_binding_store_conformance_tests!`, feature `conformance`).
#[async_trait]
pub trait ChannelBindingStore: Send + Sync {
    /// Keeps a binding, replacing the one with the same id.
    async fn put_binding(&self, binding: ChannelBinding) -> Result<()>;

    /// The binding `id`, removed ones included.
    async fn get_binding(&self, id: &str) -> Result<Option<ChannelBinding>>;

    /// Every binding, oldest pairing first, then by id.
    async fn list_bindings(&self) -> Result<Vec<ChannelBinding>>;

    /// Keeps an account's request, replacing that account's earlier one.
    async fn put_request(&self, request: ChannelPairingRequest) -> Result<()>;

    /// The request of `account` on `channel`, expired or not.
    async fn get_request(
        &self,
        channel: &str,
        account: &str,
    ) -> Result<Option<ChannelPairingRequest>>;

    /// Removes and returns the request whose code is hashed `code_hash`, expired or not; two
    /// takes of one code take it once.
    async fn take_request(&self, code_hash: &str) -> Result<Option<ChannelPairingRequest>>;

    /// Every request, oldest first, then by channel and account.
    async fn list_requests(&self) -> Result<Vec<ChannelPairingRequest>>;

    /// Removes the requests that expired at or before `before`; returns how many.
    async fn prune_requests(&self, before: DateTime<Utc>) -> Result<usize>;
}

/// Pairs the account whose request was sent `code` to `user_id` with `groups` (spec 3.3 §10
/// K1): the control plane's `POST /kernel/channels/pair`.
///
/// # Errors
///
/// Returns [`KernelError::Unauthorized`] for a wrong, used or expired code, and the store's
/// error.
pub async fn accept_pairing(
    store: &dyn ChannelBindingStore,
    code: &str,
    user_id: &str,
    groups: &[String],
    now: DateTime<Utc>,
) -> Result<ChannelBinding> {
    let request = take_pairing(store, code, now).await?;
    let binding = binding_for(&request, user_id, groups, now);
    if let Err(error) = store.put_binding(binding.clone()).await {
        restore_pairing(store, request).await;
        return Err(error);
    }
    Ok(binding)
}

/// Takes the waiting request that was sent `code`.
///
/// # Errors
///
/// Returns [`KernelError::Unauthorized`] for a wrong, used or expired code, and the store's
/// error.
pub(crate) async fn take_pairing(
    store: &dyn ChannelBindingStore,
    code: &str,
    now: DateTime<Utc>,
) -> Result<ChannelPairingRequest> {
    store
        .take_request(&secret::code_hash(code))
        .await?
        .filter(|request| now < request.expires_at)
        .ok_or_else(|| KernelError::Unauthorized {
            reason: "the chat pairing code is wrong, used or expired: write to the agent from the \
                     chat account again for a new one"
                .into(),
        })
}

/// The binding a taken request becomes once `user_id` accepts it.
pub(crate) fn binding_for(
    request: &ChannelPairingRequest,
    user_id: &str,
    groups: &[String],
    now: DateTime<Utc>,
) -> ChannelBinding {
    let mut binding = ChannelBinding::new(
        request.channel.clone(),
        request.account.clone(),
        request.thread.clone(),
        user_id,
        now,
    )
    .with_groups(groups.iter().cloned());
    binding.display_name = request.display_name.clone();
    binding
}

/// Puts a taken request back, so its code works again; a failure is logged, since the
/// caller is already answering with the error that made it put it back.
pub(crate) async fn restore_pairing(
    store: &dyn ChannelBindingStore,
    request: ChannelPairingRequest,
) {
    if let Err(error) = store.put_request(request).await {
        tracing::error!(error = %error, "a chat pairing code could not be put back");
    }
}

/// The unexpired-or-not request of the account whose binding id would be `id`.
///
/// # Errors
///
/// Returns the store's error.
pub(crate) async fn waiting_request(
    store: &dyn ChannelBindingStore,
    id: &str,
) -> Result<Option<ChannelPairingRequest>> {
    Ok(store
        .list_requests()
        .await?
        .into_iter()
        .find(|request| ChannelBinding::id_for(&request.channel, &request.account) == id))
}

/// Removes the chat account `id` (spec 3.3 §10 K1); removing a removed one changes nothing.
///
/// # Errors
///
/// Returns [`KernelError::Core`] coded `kernel.not_found` for an unknown account, and the
/// store's error.
pub async fn revoke_binding(
    store: &dyn ChannelBindingStore,
    id: &str,
    now: DateTime<Utc>,
) -> Result<ChannelBinding> {
    let Some(mut binding) = store.get_binding(id).await? else {
        return Err(KernelError::Core(adk_core::AdkError::new(
            adk_core::ErrorComponent::Server,
            adk_core::ErrorCategory::NotFound,
            crate::error::NOT_FOUND_CODE,
            format!("no paired chat account {id}: list them with `adk-rust pair list`"),
        )));
    };
    if binding.revoked_at.is_none() {
        binding.revoked_at = Some(now);
        store.put_binding(binding.clone()).await?;
    }
    Ok(binding)
}

/// What the gateway runs on (spec 3.3 §10 K1): the [`Kernel`](crate::Kernel), or a test
/// double such as the contract suite's.
#[async_trait]
pub trait ChannelHost: Send + Sync + 'static {
    /// The application the turns run in.
    fn app_name(&self) -> &str;

    /// The time now.
    fn now(&self) -> DateTime<Utc>;

    /// When quiet time that holds `at` ends, or `None` when `at` is not in quiet time; no
    /// card is delivered before then.
    fn quiet_until(&self, at: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let _ = at;
        None
    }

    /// The approval service decisions go through; `None` when the host has none.
    fn approvals(&self) -> Option<Arc<dyn ApprovalService>>;

    /// Whether `request`'s call moves money, which chat never decides (spec 3.3 §7.4 rule
    /// 4). The kernel answers for the tools its mounted drivers declare `financial`; the
    /// gateway adds the tools [`ChannelGatewayBuilder::money_tools`] names. `false` by
    /// default.
    fn moves_money(&self, request: &ApprovalRequest) -> bool {
        let _ = request;
        false
    }

    /// The audit trail the `kernel.channel` records go to.
    fn audit_trail(&self) -> Option<AuditTrail>;

    /// Starts `run`, a paired account's turn; a run whose idempotency key was used returns
    /// the process that key started. Returns the process id.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Frozen`] while everything is stopped, or the host's error.
    async fn start_turn(&self, run: NewRun) -> Result<String>;

    /// The turns that finish from now on, as their final run records.
    ///
    /// # Errors
    ///
    /// Returns the host's error when it cannot follow its runs.
    async fn finished_turns(&self) -> Result<BoxStream<'static, RunRecord>>;

    /// Stops everything, as `by`.
    ///
    /// # Errors
    ///
    /// Returns the host's error.
    async fn stop_all(&self, reason: &str, by: AuditActor) -> Result<()>;
}

/// What the gateway did with one [`ChannelInbound`] (spec 3.3 §10 K1).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum InboundOutcome {
    /// An unpaired account was sent a pairing code; nothing started (S-I6).
    PairingRequested,
    /// An unpaired account that already holds a working code was reminded; nothing started.
    PairingWaiting,
    /// Too many accounts wait; the account was told to try later and nothing started.
    PairingBusy,
    /// A paired account's message started, or for a redelivery found, a turn.
    TurnStarted {
        /// The turn's process.
        pid: String,
    },
    /// Everything is stopped, so the message started nothing.
    TurnRefused,
    /// Everything was stopped (S-I10).
    Stopped,
    /// Resuming from a chat thread was refused (S-I10).
    ResumeRefused,
    /// The answer decided the decision.
    Decided {
        /// The decision.
        decision: String,
    },
    /// The decision was answered before; the thread was told by whom (S-I2).
    AlreadyDecided {
        /// The decision.
        decision: String,
    },
    /// The decision changed since its card; a fresh card was sent.
    DecisionChanged {
        /// The decision.
        decision: String,
    },
    /// The answer was refused: high risk, an answer the decision does not allow, or a
    /// decision that is not this person's or no longer waits (spec 3.3 §7.2).
    DecisionRefused {
        /// The decision.
        decision: String,
    },
}

/// The channel, account and thread a turn came from, in its run's input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TurnOrigin {
    pub(crate) channel: String,
    pub(crate) account: String,
    pub(crate) thread: String,
}

impl TurnOrigin {
    /// The origin a run's input carries, when a channel started it.
    pub(crate) fn of(run: &RunRecord) -> Option<Self> {
        serde_json::from_value(run.input.get("channel")?.clone()).ok()
    }
}

/// The session a thread's turns continue: `chat-<channel>-` and sixteen hex digits of the
/// thread's SHA-256.
pub(crate) fn thread_session(channel: &str, thread: &str) -> String {
    format!("chat-{channel}-{}", &secret::digest(&format!("{channel}\0{thread}"))[..16])
}

impl fmt::Display for ChannelAnswer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn summaries_mask_every_argument_value() {
        let args =
            json!({"to": "client@example.com", "amount": 500, "cc": ["ab", "boss@example.com"]});
        assert_eq!(
            card_summary("Send 500 USD to client@example.com, cc boss@example.com", &args),
            "Send […] USD to […], cc […]"
        );
        assert_eq!(card_summary("Line one\nline two", &json!({})), "Line one line two");
        assert_eq!(card_summary("Pay 1,250 now", &json!({"amount": 1250})), "Pay […] now");
        let long = "x".repeat(400);
        assert_eq!(card_summary(&long, &json!({})).chars().count(), MAX_SUMMARY_CHARS);
    }

    /// Review finding 2: a value with a newline, a carriage return or a tab is masked in a
    /// summary that quotes it, raw or with the control characters already turned to spaces.
    #[test]
    fn values_with_control_characters_are_masked() {
        let message = "Sign in to Acme\nAccount 4417-2201";
        let args = json!({"message": message, "url": "https://acme.example/login"});
        let summary = format!("MCP server acme asks you to open a URL: {message}");
        let card = card_summary(&summary, &args);
        assert_eq!(card, "MCP server acme asks you to open a URL: […]");
        let spaced = "Sign in to Acme Account 4417-2201";
        assert!(!card_summary(spaced, &args).contains("4417"), "{}", card_summary(spaced, &args));
        let tabbed = json!({"note": "a\tb\r\nc"});
        assert_eq!(card_summary("Note: a\tb\r\nc!", &tabbed), "Note: […]!");
    }

    /// Review finding 12: short values, strings and numbers alike, are masked where they
    /// stand on their own, and not inside a longer word or number.
    #[test]
    fn short_values_are_masked_where_they_stand_alone() {
        let args = json!({"amount": "50", "currency": "EUR", "fee": 5, "confirm": true});
        assert_eq!(
            card_summary("Transfer 50 EUR to savings, fee 5, confirm true", &args),
            "Transfer […] […] to savings, fee […], confirm […]"
        );
        assert_eq!(
            card_summary("EUR50 and 500 and a5", &json!({"n": "50"})),
            "EUR[…] and 500 and a5"
        );
        assert_eq!(card_summary("Transfer 50 EUR", &json!({"amount": 50})), "Transfer […] EUR");
        assert_eq!(card_summary("a cat", &json!({"x": "a"})), "[…] cat");
        assert_eq!(card_summary("Pay acme CORP", &json!({"to": "Acme Corp"})), "Pay […]");
    }

    #[test]
    fn thread_sessions_are_stable_and_distinct() {
        assert_eq!(thread_session("telegram", "42"), thread_session("telegram", "42"));
        assert_ne!(thread_session("telegram", "42"), thread_session("telegram", "43"));
        assert_ne!(thread_session("telegram", "42"), thread_session("matrix", "42"));
    }
}
