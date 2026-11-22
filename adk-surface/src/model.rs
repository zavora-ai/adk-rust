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
    /// Where its commands run, when it has a shell and its source says (spec 3.3 §5.7).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<ShellView>,
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
    /// "You can change it back": a change a person can make again the other way, such as a
    /// time moved on their screens, which a feature's [`CardHint`](crate::CardHint) says
    /// (spec 3.3 §7.1).
    CanBeChangedBack,
}

/// A line under a decision's details that a feature adds through a
/// [`CardHint`](crate::CardHint): where the call comes from, with an optional link into the
/// home, such as "From the invoice-run workflow · Open in Runs" (spec 3.3 §7.1). Never on a
/// chat card (S-I5).
///
/// # Example
///
/// ```rust
/// use adk_surface::CardDetail;
///
/// let detail = CardDetail::new("From the invoice-run workflow").with_link("Open in Runs", "/ui/runs/r-1");
/// assert_eq!(detail.href.as_deref(), Some("/ui/runs/r-1"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CardDetail {
    /// The line, in plain words: "From the invoice-run workflow".
    pub text: String,
    /// The words of the link after the line: "Open in Runs".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_text: Option<String>,
    /// Where the link goes: a path in the home, such as `/ui/runs/r-1`. A renderer follows only
    /// a path on its own server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub href: Option<String>,
}

impl CardDetail {
    /// The line `text`, with no link.
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into(), link_text: None, href: None }
    }

    /// Adds the link `link_text` to `href`, a path in the home.
    #[must_use]
    pub fn with_link(mut self, link_text: impl Into<String>, href: impl Into<String>) -> Self {
        self.link_text = Some(link_text.into());
        self.href = Some(href.into());
        self
    }
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
    /// A line under the details from a feature's [`CardHint`](crate::CardHint), such as
    /// "From the invoice-run workflow · Open in Runs" (spec 3.3 §7.1). Never on a chat card
    /// (S-I5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<CardDetail>,
    /// What an MCP server asks the person, drawn in the home's own controls in place of the
    /// call's arguments (MCP elicitation; spec 3.3 §7.2). Never on a chat card (S-I5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elicitation: Option<ElicitationView>,
    /// The images the call would send, keep or act on, after redaction, with what was
    /// covered, where they go and what they cost (spec 3.7 §8.2, X11). Never on a chat card,
    /// which carries the image itself or a link (S-I5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media: Option<crate::evidence::CardMedia>,
    /// The run that waited on this decision ended while it still waits: the answer is kept
    /// and used when the agent picks the thread up again (spec 3.7 §8.2, board choice A12).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parked: Option<ParkedView>,
}

/// What an MCP server asks the person while the agent's call waits (MCP elicitation), drawn
/// by the home in its own controls, never by the server: a form whose answer is sent as the
/// decision's edit and checked against the server's schema, or a page the person opens
/// themselves, shown in full and never opened by the home (spec 3.3 §7.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", tag = "mode")]
pub enum ElicitationView {
    /// Fields to fill in: **Answer** sends them.
    #[serde(rename_all = "camelCase")]
    Form {
        /// The server that asks.
        server: String,
        /// What it asks, in its words.
        message: String,
        /// The fields, in the schema's order.
        fields: Vec<ElicitedField>,
    },
    /// A page the server asks the person to open.
    #[serde(rename_all = "camelCase")]
    Url {
        /// The server that asks.
        server: String,
        /// Why, in its words.
        message: String,
        /// The whole address, as sent.
        url: String,
        /// Its host, shown on its own so a look-alike stands out.
        host: String,
        /// Why to look twice, when there is a reason: look-alike characters, a name hidden
        /// before an `@`, no encryption.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        warning: Option<String>,
    },
}

/// One field of an elicitation form.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ElicitedField {
    /// Its key in the answer.
    pub name: String,
    /// What the field is called: the schema's `title`, else its key.
    pub label: String,
    /// The schema's description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// What it takes.
    pub kind: ElicitedKind,
    /// The answer is refused without it.
    pub required: bool,
    /// The value it starts with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    /// A choice's options.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<ElicitedOption>,
    /// A number's bounds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum: Option<f64>,
    /// A number's bounds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum: Option<f64>,
    /// A text's length bounds, in characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_length: Option<u64>,
    /// A text's length bounds, in characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_length: Option<u64>,
}

/// What an elicitation field takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ElicitedKind {
    /// A line of text.
    Text,
    /// An email address.
    Email,
    /// A web address, shown as text and never opened.
    Url,
    /// A date.
    Date,
    /// A date and time.
    DateTime,
    /// A number.
    Number,
    /// A whole number.
    Integer,
    /// Yes or no.
    Boolean,
    /// One of the options.
    Choice,
}

/// One option of a choice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ElicitedOption {
    /// What the answer carries.
    pub value: String,
    /// What the person reads.
    pub label: String,
}

/// An image of a person's message (spec 3.3 §5.1): PNG, JPEG, WebP or GIF, base64. A
/// command carries at most [`MAX_IMAGES`](crate::MAX_IMAGES), each at most
/// [`MAX_IMAGE_BYTES`](crate::MAX_IMAGE_BYTES) once decoded; the engine checks each image's
/// bytes, not only its declared type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImageData {
    /// `image/png`, `image/jpeg`, `image/webp` or `image/gif`.
    pub mime_type: String,
    /// The image, base64 (standard alphabet, padded).
    pub data: String,
}

/// A command or code an agent ran (spec 3.3 §5.1), from its tool's result: the last lines it
/// wrote, how it exited and for how long. Every string is data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CommandRun {
    /// The command line, when the tool's result repeats it; otherwise the call's own
    /// activity line says what ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// The language of code the sandbox ran: `python`, `javascript`, `command`, ….
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// How it exited, when it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// How long it ran, when the tool says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// The last lines it wrote, at most [`RUN_LINES`](crate::RUN_LINES).
    pub output: Vec<OutputLine>,
    /// How many lines the result holds in all.
    pub lines: u32,
    /// The tool cut what it wrote; the agent can read the whole of it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    /// Why it stopped short, when it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped: Option<RunStop>,
}

/// A line a run wrote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OutputLine {
    /// The line, as data.
    pub text: String,
    /// It went to standard error. Absent when false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stderr: bool,
}

/// Why a run stopped short.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum RunStop {
    /// It ran out of time.
    Timeout,
    /// It ran out of memory.
    MemoryExceeded,
    /// The sandbox refused what it tried.
    PolicyViolation,
    /// It could not run.
    Error,
}

/// Where an agent's commands run (spec 3.3 §5.7), as its harness reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ShellPlace {
    /// On this computer, with no sandbox.
    Host,
    /// In the OS sandbox.
    #[serde(rename_all = "camelCase")]
    Sandbox {
        /// The enforcer: `seatbelt`, `bubblewrap`; absent when the source does not say.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        enforcer: Option<String>,
    },
    /// In a container.
    #[serde(rename_all = "camelCase")]
    Container {
        /// The image.
        image: String,
    },
    /// In an environment from a sandbox provider.
    #[serde(rename_all = "camelCase")]
    Remote {
        /// The provider: `e2b`, `daytona`, `modal`, ….
        provider: String,
        /// It runs off this computer, so commands cannot reach its files.
        off_host: bool,
    },
    /// A sandbox was asked for and is unavailable, so commands run on this computer.
    #[serde(rename_all = "camelCase")]
    Degraded {
        /// The sandbox asked for.
        requested: String,
        /// Why it is unavailable.
        reason: String,
    },
}

impl ShellPlace {
    /// Commands run apart from this computer's files and network: a sandbox, a container
    /// or a remote environment.
    pub fn isolated(&self) -> bool {
        matches!(self, Self::Sandbox { .. } | Self::Container { .. } | Self::Remote { .. })
    }
}

/// Where an agent's commands run, and the words that say so.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ShellView {
    /// Where.
    pub place: ShellPlace,
    /// "Commands run in the sandbox (Seatbelt)".
    pub text: String,
}

/// A decision whose run stopped waiting for it (spec 3.7 §8.2, board choice A12).
///
/// # Example
///
/// ```rust
/// use adk_surface::ParkedView;
///
/// let parked = ParkedView {
///     at: chrono::Utc::now(),
///     at_text: "10:24".into(),
///     text: adk_surface::words::stopped_waiting("Document desk", "10:24"),
/// };
/// assert!(parked.text.contains("picks this thread up again"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ParkedView {
    /// When the run ended.
    pub at: DateTime<Utc>,
    /// Local `HH:MM`.
    pub at_text: String,
    /// "Document desk stopped waiting at 10:24 and finished its run without this. …".
    pub text: String,
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
        /// The images the person sent with it (spec 3.3 §5.1). Absent when none.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<ImageData>,
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
        /// The image a vision tool answered about, by digest: the thread links the call to
        /// the annotated viewer (spec 3.7 §8.2, board choice V1).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        image: Option<String>,
    },
    /// A command or code an agent ran, and what it wrote: the shell's `bash` or the sandbox's
    /// `sandbox_exec` (spec 3.3 §5.1). It stands for that call's [`Result`](Self::Result).
    #[serde(rename_all = "camelCase")]
    Run {
        /// The tool.
        tool: String,
        /// The call it answers.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call_id: Option<String>,
        /// It succeeded: it exited with 0.
        ok: bool,
        /// "Done" or "Failed".
        text: String,
        /// The run.
        run: CommandRun,
    },
    /// Replies the agent suggests the person might send next (spec 3.3 §5.1): tapping one fills
    /// the message box, and the person still sends it.
    #[serde(rename_all = "camelCase")]
    Suggestions {
        /// The replies, checked: one to four short lines, as data.
        replies: Vec<String>,
        /// The call that offered them.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call_id: Option<String>,
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
    /// Something an agent drew (spec 3.3 §6.1 `genui`, §7.4 rule 5): a form, card, table or
    /// screen on the normal card background, never a decision. Only a render tool's result
    /// with the reserved state key and the drawing's digest makes one.
    #[serde(rename_all = "camelCase")]
    Drawn {
        /// The drawing, its strings resolved on the server.
        surface: Box<crate::genui::DrawnSurface>,
    },
    /// A drawing that was not shown: a plain notice (spec 3.3 §7.4 rule 5).
    #[serde(rename_all = "camelCase")]
    DrawnRefused {
        /// What the thread says, and the reason the developer panel shows.
        refusal: crate::genui::RefusedDrawing,
    },
    /// A person sent a drawn form: the call they asked for, held as the drawing agent for
    /// their decision, or refused by its rules (spec 3.3 §7.4 rule 5). The decision's own
    /// `Decision` item follows it in the same entry.
    #[serde(rename_all = "camelCase")]
    DrawnSent {
        /// The call and the form it came from.
        sent: crate::genui::DrawnSent,
    },
    /// An app an MCP server's tool showed (spec 3.3 §6.1 `genui-apps`): on the normal card
    /// background, labelled as unable to approve anything, framed sandboxed by the web home
    /// and shown as its text result elsewhere. Only the event of the MCP tool's own call, which
    /// carries its `adk.mcp.app` record, makes one.
    #[serde(rename_all = "camelCase")]
    App {
        /// The app.
        app: Box<crate::genui::AppView>,
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
