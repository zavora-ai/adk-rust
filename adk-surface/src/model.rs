//! The projection types every surface draws from (spec 3.3 §6, §7.1, §8).
//!
//! Each type carries the words to show (from [`words`](crate::words)) beside the data they
//! were made from, so a renderer never composes on-screen text of its own. Identifiers such as
//! [`DecisionCard::id`] and [`DecisionCard::version`] are for commands and are never shown
//! (spec 3.3 §8: no internal identifiers). Every string that came from a model, a tool or a
//! file is data: renderers escape it before display (§11 S-I9).
//!
//! All payloads serialize in `camelCase`.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::origin::Via;
use crate::rules::{Allowed, Risk};

/// An agent's avatar: a rounded monogram in the agent's own colour (spec 3.3 §9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Avatar {
    /// One or two letters.
    pub monogram: String,
    /// The agent's colour, `#RRGGBB`.
    pub color: String,
}

/// An agent as a decision or a thread names it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentRef {
    /// The agent's name in the runtime.
    pub name: String,
    /// The name to show, such as "Finance".
    pub display_name: String,
    /// The avatar.
    pub avatar: Avatar,
}

/// What an agent is doing (spec 3.3 §8.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AgentStatus {
    /// "Working": `RunState::Running`, or queued to run.
    Working,
    /// "Waiting for you": waiting with a held call.
    WaitingForYou,
    /// "Asleep until 07:00": waiting with a wake set, or parked by a stop.
    Asleep,
    /// "Done": completed, or cancelled.
    Done,
    /// "Failed".
    Failed,
    /// "Ready": an agent the person added that has not run yet (spec 3.3 §5.7, §8.1).
    Ready,
    /// "Paused": an agent paused by hand (spec 3.3 §5.7, §8.1).
    Paused,
    /// "Can't start": an agent the kernel cannot build (spec 3.3 §5.7, §8.1).
    CantStart,
}

/// One agent in the roster (spec 3.3 §5.1, §8.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentRow {
    /// The agent.
    pub agent: AgentRef,
    /// What it is doing.
    pub status: AgentStatus,
    /// The status word: "Working", "Waiting for you", "Asleep until 07:00", "Done", "Failed".
    pub status_text: String,
    /// When it wakes, while asleep with a wake time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake_at: Option<DateTime<Utc>>,
    /// The session of its latest work, to open its thread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Its area, for an agent the person added (spec 3.3 §5.7).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub area: Option<String>,
    /// How much it asks first, for an agent the person added (spec 3.3 §5.7).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<ModeView>,
    /// An agent the person added: it has settings beyond its mode and rules (spec 3.3 §5.7).
    /// Absent when false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub added: bool,
    /// The caller may change it here (spec 3.3 §7.2). Absent when false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub can_change: bool,
}

/// How much agents act on their own (spec 3.3 §8.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Mode {
    /// "Just look": `HarnessMode::ReadOnly`.
    JustLook,
    /// "Suggest": `HarnessMode::Plan`.
    Suggest,
    /// "Ask first": `HarnessMode::Ask`.
    AskFirst,
    /// "Go ahead": `HarnessMode::Auto`.
    GoAhead,
}

/// The mode with its words.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModeView {
    /// The mode.
    pub mode: Mode,
    /// The setting's name: "Ask first".
    pub label: String,
    /// As a status: "Asks first".
    pub status: String,
}

/// Today's spend against the daily limit (spec 3.3 §8.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SpendView {
    /// Spent today, micro-USD.
    pub spent_micro_usd: u64,
    /// The daily spending limit, micro-USD, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_micro_usd: Option<u64>,
    /// "$1.84 of $5.00 today", or "$1.84 today" with no limit.
    pub text: String,
}

/// Quiet time (spec 3.3 §8.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct QuietView {
    /// Quiet time holds now.
    pub active: bool,
    /// Local start, `HH:MM`.
    pub start: String,
    /// Local end, `HH:MM`.
    pub end: String,
    /// "Quiet from 22:00", or "Quiet until 07:00" while it holds.
    pub text: String,
}

/// The status line every surface shows (spec 3.3 §5.1, §6): the mode word, spend against the
/// daily limit, quiet time and the count of decisions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StatusLine {
    /// The mode, when the source runs a harness.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<ModeView>,
    /// Spend, when the source knows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spend: Option<SpendView>,
    /// Quiet time, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quiet: Option<QuietView>,
    /// Decisions waiting for the caller: the filled badge.
    pub decisions: u32,
    /// Everything is stopped.
    pub stopped: bool,
    /// "All agents stopped", while stopped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped_text: Option<String>,
    /// Minutes east of UTC of the server's time zone now: the kernel reads a quiet window that
    /// carries no offset of its own there, so a page in another zone reads it there too
    /// (spec 3.3 §5.1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_utc_offset_minutes: Option<i32>,
}

/// The exact call a decision is about (spec 3.3 §7.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CallView {
    /// The tool.
    pub tool: String,
    /// Its arguments in full, as the inbox shows them to approvers (secrets removed).
    pub args: Value,
    /// The call as text, `send_email(to: "…", subject: "…")`, cut short when long.
    pub text: String,
    /// `text` is cut short: the call must be expanded before it can be approved.
    pub truncated: bool,
}

/// Whether a call can be taken back (spec 3.3 §7.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Reversibility {
    /// "Can't be unsent": a message, once sent.
    CantBeUnsent,
    /// "Can't be undone".
    CantBeUndone,
    /// "You can remove it later": an agent a person adds (spec 3.3 §5.7).
    CanBeRemoved,
}

/// One step of how a decision got here (spec 3.3 §7.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TimelineStep {
    /// "Asked at 18:00", "Waited through quiet time", "Waiting for you".
    pub text: String,
    /// The step the decision is at now.
    pub current: bool,
}

/// How a decision ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum DecidedAnswer {
    /// Allowed.
    Yes,
    /// Refused.
    No,
    /// Allowed with changed arguments.
    Edited,
    /// Answered with a message instead.
    Replied,
    /// Cancelled or expired without an answer.
    Ended,
    /// Answered, but the source does not say how.
    Unknown,
}

/// A decision answered, here or elsewhere (spec 3.3 §8.1 "Decided", §11 S-I2): who answered,
/// where and how, and when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Decided {
    /// Who answered: "You" for the caller, else the approver's name; empty when nobody did.
    pub by: String,
    /// The caller answered.
    pub by_you: bool,
    /// Where and how, when the answer came through a surface.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<Via>,
    /// When.
    pub at: DateTime<Utc>,
    /// Local `HH:MM`.
    pub at_text: String,
    /// The answer.
    pub answer: DecidedAnswer,
    /// "You said yes in Telegram at 07:31".
    pub text: String,
    /// How far the answer reaches, shown after the text: "done once" for allow once, the
    /// standing rule's label for always allow (spec 3.3 §5.4); `None` when no surface
    /// recorded the answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_text: Option<String>,
}

/// Where a decision is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum CardStatus {
    /// Waiting for an answer.
    Waiting,
    /// Answered, or ended.
    Decided,
}

/// A decision, drawn the same way on every surface from its inbox entry, never from agent
/// output (spec 3.3 §7.1, §11 S-I1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DecisionCard {
    /// The inbox entry: what a command answers. Never shown.
    pub id: String,
    /// The version the surface saw, which its answer carries (S-I2). Never shown.
    pub version: u64,
    /// The session the call belongs to.
    pub session_id: String,
    /// Who asks.
    pub agent: AgentRef,
    /// Since when it waits.
    pub waiting_since: DateTime<Utc>,
    /// "since 18:00".
    pub since_text: String,
    /// Waiting or decided.
    pub status: CardStatus,
    /// The pill: "Waiting for you" or "Decided".
    pub status_text: String,
    /// What will happen, in plain words.
    pub heading: String,
    /// What the heading alone does not say, in plain words, for a decision the kernel itself
    /// asks: who suggests the work and when it runs once allowed, such as "Finance suggests
    /// this. Once you say yes, it runs every 5 minutes." (spec 3.3 §7.1, §8.1). Never on a
    /// chat card (S-I5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// The exact call: the details under the plain words.
    pub call: CallView,
    /// How it got here.
    pub timeline: Vec<TimelineStep>,
    /// The answers this surface may give; empty once decided.
    pub allowed: Allowed,
    /// The risk.
    pub risk: Risk,
    /// "Medium risk".
    pub risk_text: String,
    /// Whether it can be taken back.
    pub reversibility: Reversibility,
    /// "Can't be unsent".
    pub reversibility_text: String,
    /// The call spends money.
    pub money: bool,
    /// "Also on your phone and in Telegram".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub also_shown: Option<String>,
    /// The answer, once decided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided: Option<Decided>,
    /// The agent a suggestion would add, shown in place of the call (spec 3.3 §5.7, S-I13).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adds_agent: Option<crate::agents::AgentCardView>,
}

/// Who wrote a message in a thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Speaker {
    /// The person.
    You,
    /// An agent.
    Agent,
}

/// One thing in a thread (spec 3.3 §5.1): a message, something an agent did, or a decision,
/// which links to its inbox entry and is drawn from the card (§11 S-I1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ThreadItem {
    /// A message.
    #[serde(rename_all = "camelCase")]
    Message {
        /// The person or an agent.
        from: Speaker,
        /// The author's name: the agent's display name, or "You".
        author: String,
        /// The text, as data.
        text: String,
    },
    /// An agent used a tool.
    #[serde(rename_all = "camelCase")]
    Activity {
        /// The tool.
        tool: String,
        /// "Read file timesheet-october.csv".
        text: String,
        /// The call, to match its result.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call_id: Option<String>,
    },
    /// A tool's result.
    #[serde(rename_all = "camelCase")]
    Result {
        /// The tool.
        tool: String,
        /// The call it answers.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call_id: Option<String>,
        /// It succeeded.
        ok: bool,
        /// "Done" or "Failed".
        text: String,
    },
    /// An agent asked first: a held call. Its card comes from the inbox entry.
    #[serde(rename_all = "camelCase")]
    Decision {
        /// The inbox entry, when the call reached the inbox.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        decision_id: Option<String>,
        /// The tool.
        tool: String,
        /// "Waiting for you".
        text: String,
    },
}

/// One event of a session's store, numbered by its position there (spec 3.3 §6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FeedEntry {
    /// The event's position in the session store, from 1: the `Last-Event-ID` a surface
    /// resumes from.
    pub seq: u64,
    /// When.
    pub at: DateTime<Utc>,
    /// Local `HH:MM`.
    pub at_text: String,
    /// What it shows; empty for events with nothing to show.
    pub items: Vec<ThreadItem>,
}

/// A pill on a briefing line (spec 3.3 §5.1 Today).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Pill {
    /// "Done".
    Done,
    /// "Failed".
    Failed,
    /// "Waiting for you".
    WaitingForYou,
    /// "You decided".
    YouDecided,
    /// "Suggestion".
    Suggestion,
    /// "Question".
    Question,
}

/// One line of "While you were away", from one `/proc` ledger row (spec 3.3 §5.1, §8.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BriefingLine {
    /// The ledger row it came from: its record.
    pub seq: u64,
    /// When.
    pub at: DateTime<Utc>,
    /// Local `HH:MM`.
    pub at_text: String,
    /// The agent.
    pub agent: String,
    /// The pill, when the line has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pill: Option<Pill>,
    /// The pill's words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pill_text: Option<String>,
    /// The sentence.
    pub text: String,
}

/// "While you were away" (spec 3.3 §5.1, §8.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Briefing {
    /// "While you were away".
    pub title: String,
    /// Since when.
    pub since: DateTime<Utc>,
    /// "Since 22:04".
    pub since_text: String,
    /// What was spent today.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spend: Option<SpendView>,
    /// Decisions waiting.
    pub waiting: u32,
    /// One line per event, oldest first.
    pub lines: Vec<BriefingLine>,
    /// The source keeps an activity record. Without one, `lines` is empty whatever ran, so a
    /// surface says nothing is recorded rather than that nothing happened.
    #[serde(default = "kept")]
    pub recorded: bool,
}

fn kept() -> bool {
    true
}
