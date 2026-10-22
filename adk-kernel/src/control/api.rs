//! Wire types of the control plane (spec 3.2 §7.11). Every payload is `camelCase`.

use adk_core::{Event, RunState, ToolConfirmationRequest};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::partition::PartitionId;
use crate::process::Process;
use crate::sched::QueueClass;
use crate::stores::ProcessFilter;

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
