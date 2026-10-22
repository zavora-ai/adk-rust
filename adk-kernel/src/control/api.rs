//! Wire types of the control plane (spec 3.2 §7.11). Every payload is `camelCase`.

use std::collections::BTreeMap;

use adk_core::{
    ApprovalAction, ApprovalScope, ApprovalState, Event, RunState, ToolConfirmationRequest,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::partition::PartitionId;
use crate::process::{Process, ProcessKind};
use crate::sched::QueueClass;
use crate::stores::ProcessFilter;
use crate::wake::WakeSet;

/// `GET /kernel/processes` query: `adk-rust ps [--partition P] [--state S]` (spec 3.2 §8.1).
///
/// # Example
///
/// ```rust
/// use adk_core::RunState;
/// use adk_kernel::control::api::ProcessesQuery;
///
/// let query = ProcessesQuery { state: Some(RunState::Waiting), ..ProcessesQuery::default() };
/// assert_eq!(query.to_filter().states, vec![RunState::Waiting]);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessesQuery {
    /// Only processes in this partition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition: Option<PartitionId>,
    /// Only processes in this state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<RunState>,
    /// At most this many processes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

impl ProcessesQuery {
    /// The [`ProcessFilter`] the query selects.
    pub fn to_filter(&self) -> ProcessFilter {
        let mut filter = ProcessFilter::default();
        if let Some(partition) = &self.partition {
            filter = filter.partition(partition.clone());
        }
        if let Some(state) = self.state {
            filter = filter.states([state]);
        }
        if let Some(limit) = self.limit {
            filter = filter.limit(limit);
        }
        filter
    }
}

/// `GET /kernel/processes` answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessesResponse {
    /// The processes, each with its live view.
    pub processes: Vec<Process>,
}

/// The answer of a process action: the process after it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessResponse {
    /// The process.
    pub process: Process,
}

/// `POST /kernel/processes/{pid}/kill` body.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KillRequest {
    /// Why, recorded on the run and in the audit record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// `POST /kernel/processes/{pid}/nice` body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NiceRequest {
    /// The queue class to move the process to.
    pub queue: QueueClass,
}

/// `POST /kernel/freeze` body.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FreezeRequest {
    /// The freeze reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// `POST /kernel/freeze` and `POST /kernel/unfreeze` answer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FreezeResponse {
    /// Whether execution is frozen after the call.
    pub frozen: bool,
    /// The freeze reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Running processes the freeze parked, on this replica.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parked_runs: Option<usize>,
    /// Why the freeze's `kernel.control` record could not be written; the freeze took effect
    /// regardless, since the kill switch never waits on the audit log.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audit_error: Option<String>,
}

/// `GET /kernel/events` query. The `Last-Event-ID` header, when present, takes precedence.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventsQuery {
    /// Replay events after this cursor before the live ones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<u64>,
}

/// `POST /kernel/attach/{session}` body.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachRequest {
    /// The session's user; defaults to the caller.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    /// Only transcript events after this time: the cursor of an earlier attach.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<DateTime<Utc>>,
}

/// `POST /kernel/attach/{session}` answer: transcript cursor, pending holds, process
/// (spec 3.2 §7.11).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachResponse {
    /// The session.
    pub session_id: String,
    /// The transcript since the request's cursor.
    pub transcript: Vec<Event>,
    /// The cursor to attach from next time: the last transcript event's time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<DateTime<Utc>>,
    /// Calls the session holds for approval.
    #[serde(default)]
    pub pending: Vec<ToolConfirmationRequest>,
    /// The process running the session, when one does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<Process>,
}

/// `POST /kernel/processes` body: a process to start (spec 3.2 §7.3, §7.11), run as the
/// authenticated caller.
///
/// The run's user is always the caller; a body naming one, or any other unknown field, is
/// refused with `422`. A wake set starts the process asleep (`adk-rust wake`).
///
/// # Example
///
/// ```rust
/// use adk_kernel::control::api::SpawnRequest;
///
/// let request = SpawnRequest::new("adk-rust", serde_json::json!({ "text": "run the tests" }))
///     .with_session_id("s-1");
/// let json = serde_json::to_value(&request).unwrap();
/// assert_eq!(json["sessionId"], "s-1");
/// assert!(serde_json::from_value::<SpawnRequest>(serde_json::json!({
///     "target": "adk-rust",
///     "userId": "someone-else",
/// }))
/// .is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SpawnRequest {
    /// A target registered on the kernel, such as `adk-rust` or `harness.goal`.
    pub target: String,
    /// The run's input: a string, `{"text": ...}` or `{"content": ...}` for an agent.
    #[serde(default)]
    pub input: Value,
    /// The session an agent run continues; defaults to a new one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// The queue class; defaults to interactive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue: Option<QueueClass>,
    /// What kind of work it is; defaults to interactive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ProcessKind>,
    /// The process that caused it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Starts the process asleep on this wake set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake: Option<WakeSet>,
    /// Free-form labels.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
}

impl SpawnRequest {
    /// A process of `target` with `input`, interactive, in a new session.
    pub fn new(target: impl Into<String>, input: Value) -> Self {
        Self {
            target: target.into(),
            input,
            session_id: None,
            queue: None,
            kind: None,
            parent: None,
            wake: None,
            labels: BTreeMap::new(),
        }
    }

    /// Continues `session_id`.
    #[must_use]
    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    /// Runs on `queue`.
    #[must_use]
    pub fn with_queue(mut self, queue: QueueClass) -> Self {
        self.queue = Some(queue);
        self
    }

    /// Records the process as `kind`.
    #[must_use]
    pub fn with_kind(mut self, kind: ProcessKind) -> Self {
        self.kind = Some(kind);
        self
    }

    /// Starts the process asleep on `wake`.
    #[must_use]
    pub fn with_wake(mut self, wake: WakeSet) -> Self {
        self.wake = Some(wake);
        self
    }
}

/// A held call waiting in the approvals inbox, as `GET /api/approvals` shows it to an
/// approver: without the call's fingerprint, which holds its unredacted arguments.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalItem {
    /// The request id `decide` takes.
    pub id: String,
    /// The session the call was held in.
    pub scope: ApprovalScope,
    /// The tool called.
    pub tool_name: String,
    /// The arguments, redacted.
    pub args_redacted: Value,
    /// A one-line description.
    #[serde(default)]
    pub summary: String,
    /// The version `decide` must name.
    pub version: u64,
    /// Where the request is.
    pub state: ApprovalState,
    /// What an approver may do.
    #[serde(default)]
    pub allowed_actions: Vec<ApprovalAction>,
    /// The durable run parked on the call, when one is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// When the call was held.
    pub created_at: DateTime<Utc>,
    /// When the request expires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
}
