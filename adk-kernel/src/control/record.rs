//! The record of what the agents did, and its check (spec 3.3 §10 K5, §5.1 "What your
//! agents did", §11).
//!
//! The kernel writes its records to a signed audit log it cannot read back or verify
//! itself: the log's store and its checkpoint keys belong to whoever opened it. A
//! [`RecordBook`] is what that host gives the kernel; `adk-rust up` builds one over the
//! kernel's signed audit log that checks it with the verifier `adk-rust audit verify` runs,
//! so the home's "Every action is on record" and the command give the same answer.
//!
//! | Route (under `/api`) | Caller | Does |
//! |---|---|---|
//! | `GET /kernel/record` | operator, or a device with `read` | The latest actions as structured lines ([`RecordResponse`], [`RecordQuery`]) |
//! | `GET /kernel/record/check` | operator, or a device with `read` | Whether every action is on record and nothing was changed or deleted ([`RecordCheck`]) |

use std::sync::Arc;

use async_trait::async_trait;
use axum::Json;
use axum::Router;
use axum::extract::{Query, State};
use axum::routing::get;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::paths;
use super::routes::{Caller, ControlBackend};
use crate::error::{KernelError, Result};

/// One action on record, as structured fields a surface words as a sentence ("You let
/// Finance send one email, from Telegram", spec 3.3 §5.1, §11).
///
/// # Example
///
/// ```rust
/// use adk_kernel::control::RecordLine;
///
/// let line = RecordLine::new(1284, chrono::Utc::now(), "approval")
///     .with_subject("ana")
///     .with_agent("finance")
///     .with_how("chat", Some("telegram"));
/// assert_eq!(serde_json::to_value(&line).unwrap()["channel"], "telegram");
///
/// let device = RecordLine::new(1285, chrono::Utc::now(), "kernel.control")
///     .with_kernel_action("approvals.decide")
///     .with_decision("approved")
///     .with_channel("Ana's phone");
/// assert_eq!(serde_json::to_value(&device).unwrap()["kernelAction"], "approvals.decide");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct RecordLine {
    /// The entry's place in the log.
    pub seq: u64,
    /// When it was recorded.
    pub at: DateTime<Utc>,
    /// The record's action, such as `approval` or `kernel.control`.
    pub action: String,
    /// What a kernel record (`kernel.control`, `kernel.channel`) did: `approvals.decide`,
    /// `pairing.issue`, `freeze`, `settings.quiet_time`, a chat answer's `decide`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kernel_action: Option<String>,
    /// The person or service that acted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// The agent that acted, or whose call was decided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// The tool, for a call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// The decision: `approved`, `rejected`, `denied`, `allowed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<String>,
    /// How it ended: `succeeded`, `failed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// How the answer was given: `keyboard`, `touch`, `voice`, `chat`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub how: Option<String>,
    /// The chat channel or device it was given from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    /// The surface it was given on, as the record names it: `web`, `tray`, `phone`,
    /// `terminal`, `voice`, `chat`, or `device` for a paired device on the kernel's own routes
    /// (spec 3.3 §11).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface: Option<String>,
    /// What it acted on: a process, a fact, an area.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// The item it acted on, as the person knows it — "News digest" — where the record names
    /// one, such as the answer to an agent's screen action (spec 3.3 §8, §11).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Who publishes the tool, for a call: `verbs:adk-rust` for a screen action, so a surface
    /// tells a feature's calls apart by their publisher, never by the tool's name (spec 3.3
    /// §7.1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_source: Option<String>,
    /// What decided a call: `confirmation` for a person's answer, `tool_policy:<name>`,
    /// `governance`, `guardrail` (spec 3.3 §11).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_ref: Option<String>,
}

impl RecordLine {
    /// Entry `seq`, recorded at `at`, of `action`.
    pub fn new(seq: u64, at: DateTime<Utc>, action: impl Into<String>) -> Self {
        Self {
            seq,
            at,
            action: action.into(),
            kernel_action: None,
            subject: None,
            agent: None,
            tool: None,
            decision: None,
            outcome: None,
            how: None,
            channel: None,
            surface: None,
            target: None,
            title: None,
            tool_source: None,
            policy_ref: None,
        }
    }

    /// Sets who acted.
    #[must_use]
    pub fn with_subject(mut self, subject: impl Into<String>) -> Self {
        self.subject = Some(subject.into());
        self
    }

    /// Sets the agent.
    #[must_use]
    pub fn with_agent(mut self, agent: impl Into<String>) -> Self {
        self.agent = Some(agent.into());
        self
    }

    /// Sets the tool.
    #[must_use]
    pub fn with_tool(mut self, tool: impl Into<String>) -> Self {
        self.tool = Some(tool.into());
        self
    }

    /// Sets the decision.
    #[must_use]
    pub fn with_decision(mut self, decision: impl Into<String>) -> Self {
        self.decision = Some(decision.into());
        self
    }

    /// Sets the outcome.
    #[must_use]
    pub fn with_outcome(mut self, outcome: impl Into<String>) -> Self {
        self.outcome = Some(outcome.into());
        self
    }

    /// Sets how the answer was given, and from which channel or device.
    #[must_use]
    pub fn with_how(mut self, how: impl Into<String>, channel: Option<&str>) -> Self {
        self.how = Some(how.into());
        self.channel = channel.map(str::to_string);
        self
    }

    /// Sets the chat channel or device it was given from, when how is not known.
    #[must_use]
    pub fn with_channel(mut self, channel: impl Into<String>) -> Self {
        self.channel = Some(channel.into());
        self
    }

    /// Sets the surface it was given on, such as `tray` for an answer from the desktop tray
    /// (spec 3.3 §11).
    #[must_use]
    pub fn with_surface(mut self, surface: impl Into<String>) -> Self {
        self.surface = Some(surface.into());
        self
    }

    /// Sets what a kernel record did.
    #[must_use]
    pub fn with_kernel_action(mut self, action: impl Into<String>) -> Self {
        self.kernel_action = Some(action.into());
        self
    }

    /// Sets what it acted on.
    #[must_use]
    pub fn with_target(mut self, target: impl Into<String>) -> Self {
        self.target = Some(target.into());
        self
    }

    /// Sets the item it acted on, as the person knows it (spec 3.3 §8).
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_kernel::control::RecordLine;
    ///
    /// let denied = RecordLine::new(7, chrono::Utc::now(), "surface.decide")
    ///     .with_tool("verb_snooze")
    ///     .with_decision("rejected")
    ///     .with_title("News digest");
    /// assert_eq!(serde_json::to_value(&denied).unwrap()["title"], "News digest");
    /// ```
    #[must_use]
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Sets who publishes the called tool, and what decided the call (spec 3.3 §7.1, §11).
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_kernel::control::RecordLine;
    ///
    /// let refused = RecordLine::new(8, chrono::Utc::now(), "verb_snooze")
    ///     .with_tool("verb_snooze")
    ///     .with_decision("denied")
    ///     .with_call("verbs:adk-rust", Some("confirmation"));
    /// let wire = serde_json::to_value(&refused).unwrap();
    /// assert_eq!(wire["toolSource"], "verbs:adk-rust");
    /// assert_eq!(wire["policyRef"], "confirmation");
    /// ```
    #[must_use]
    pub fn with_call(mut self, tool_source: impl Into<String>, policy_ref: Option<&str>) -> Self {
        self.tool_source = Some(tool_source.into());
        self.policy_ref = policy_ref.map(str::to_string);
        self
    }
}

/// The answer of the record check (spec 3.3 §5.1 "Every action is on record"): whether the
/// log verifies against its signed checkpoints, how many entries it holds, and the
/// verifier's own report.
///
/// # Example
///
/// ```rust
/// use adk_kernel::control::RecordCheck;
///
/// let check = RecordCheck::new(true, 1284, chrono::Utc::now());
/// assert_eq!(serde_json::to_value(&check).unwrap()["entries"], 1284);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct RecordCheck {
    /// Every entry is covered by a signed checkpoint and nothing was changed or deleted.
    pub valid: bool,
    /// How many entries the log holds.
    pub entries: u64,
    /// When the check ran.
    pub checked_at: DateTime<Utc>,
    /// Why the check failed, in the verifier's words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
    /// The verifier's report, as `adk-rust audit verify --json` prints it.
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub report: Value,
}

impl RecordCheck {
    /// A check that found the log `valid` with `entries` entries at `checked_at`.
    pub fn new(valid: bool, entries: u64, checked_at: DateTime<Utc>) -> Self {
        Self { valid, entries, checked_at, problem: None, report: Value::Null }
    }

    /// Sets why it failed.
    #[must_use]
    pub fn with_problem(mut self, problem: impl Into<String>) -> Self {
        self.problem = Some(problem.into());
        self
    }

    /// Sets the verifier's report.
    #[must_use]
    pub fn with_report(mut self, report: Value) -> Self {
        self.report = report;
        self
    }
}

/// What the host of the kernel's audit log gives the control plane to read and check it
/// (spec 3.3 §10 K5).
///
/// # Example
///
/// ```rust
/// use adk_kernel::control::{RecordBook, RecordCheck, RecordLine};
///
/// struct Empty;
///
/// #[async_trait::async_trait]
/// impl RecordBook for Empty {
///     async fn recent(&self, _limit: usize) -> adk_kernel::Result<Vec<RecordLine>> { Ok(vec![]) }
///     async fn check(&self) -> adk_kernel::Result<RecordCheck> { Ok(RecordCheck::new(true, 0, chrono::Utc::now())) }
/// }
/// ```
#[async_trait]
pub trait RecordBook: Send + Sync {
    /// The latest `limit` actions, newest first.
    ///
    /// # Errors
    ///
    /// The log store's error.
    async fn recent(&self, limit: usize) -> Result<Vec<RecordLine>>;

    /// Verifies the whole log against its signed checkpoints.
    ///
    /// # Errors
    ///
    /// The log store's error; a log that does not verify is a [`RecordCheck`] with `valid`
    /// unset, not an error.
    async fn check(&self) -> Result<RecordCheck>;
}

/// `GET /kernel/record` query (spec 3.3 §10 K5).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordQuery {
    /// At most this many lines; 20 when absent, at most 500.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

/// `GET /kernel/record` answer (spec 3.3 §10 K5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordResponse {
    /// The latest actions, newest first.
    pub lines: Vec<RecordLine>,
}

type Backend = State<Arc<dyn ControlBackend>>;

/// The routes, relative to `/api`, over the backend in their state.
pub(super) fn routes() -> Router<Arc<dyn ControlBackend>> {
    Router::new().route(paths::RECORD, get(recent)).route(paths::RECORD_CHECK, get(check))
}

/// The record book of `backend`, or a `Config` error naming why there is none.
fn book_of(backend: &dyn ControlBackend) -> Result<Arc<dyn RecordBook>> {
    backend.kernel().and_then(|kernel| kernel.config().control.record.clone()).ok_or_else(|| {
        KernelError::Config {
            reason: "this kernel shows no record: set ControlPlaneConfig::with_record, as \
                     `adk-rust up` does over its signed audit log"
                .into(),
        }
    })
}

async fn recent(
    State(backend): Backend,
    caller: Caller,
    Query(query): Query<RecordQuery>,
) -> Result<Json<RecordResponse>> {
    caller.require_operator("read the record")?;
    let limit = query.limit.unwrap_or(20).clamp(1, 500);
    Ok(Json(RecordResponse { lines: book_of(backend.as_ref())?.recent(limit).await? }))
}

async fn check(State(backend): Backend, caller: Caller) -> Result<Json<RecordCheck>> {
    caller.require_operator("check the record")?;
    Ok(Json(book_of(backend.as_ref())?.check().await?))
}
