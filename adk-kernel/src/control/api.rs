//! Wire types of the control plane (spec 3.2 §7.11). Every payload is `camelCase`.

use std::collections::BTreeMap;

use adk_core::{
    ApprovalAction, ApprovalScope, ApprovalState, Event, RunState, ToolConfirmationRequest,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use super::pairing::{
    AcceptChannelRequest, ChannelAccountResponse, ChannelAccountState, ChannelAccountView,
    ChannelAccountsResponse, ChannelPairingResponse, ChannelPairingView, ClaimRequest,
    DeviceResponse, DevicesResponse, IssueRequest,
};
// The routes the surfaces need (spec 3.3 §10 K3–K5).
pub use super::agents::AgentsResponse;
#[cfg(feature = "proc")]
pub use super::away::{
    AwayCounts, AwayKind, AwayLine, AwayLineResponse, AwayQuery, AwayResponse, DEFAULT_VIEWER,
    MAX_PAGE_ROWS, QuietTimeReport, SeenRequest, SeenResponse,
};
pub use super::memory::{
    FactResponse, FixRequest, ForgetResponse, MemoryQuery, MemoryResponse, UndoRequest,
};
pub use super::record::{RecordQuery, RecordResponse};
pub use super::settings::{
    AreaResponse, AreaView, QuietTimeRequest, SettingsResponse, SpendingLimitRequest,
};

use crate::agenda::{AgendaFilter, AgendaItem, AgendaStateKind};
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

/// `GET /kernel/agenda` query: `adk-rust agenda list` (spec 3.2 §7.11, §8.1).
///
/// # Example
///
/// ```rust
/// use adk_kernel::AgendaStateKind;
/// use adk_kernel::control::api::AgendaQuery;
///
/// let query = AgendaQuery { state: Some(AgendaStateKind::Proposed), ..AgendaQuery::default() };
/// assert_eq!(query.to_filter().states, vec![AgendaStateKind::Proposed]);
///
/// // The newest twenty, then the twenty before them (spec 3.3 §5.1).
/// let newest = AgendaQuery { newest_first: true, limit: Some(20), ..AgendaQuery::default() };
/// let next = AgendaQuery { after: Some("item-41".into()), ..newest.clone() };
/// assert_eq!(serde_json::to_value(&next).unwrap()["newestFirst"], true);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgendaQuery {
    /// Only items of this partition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition: Option<PartitionId>,
    /// Only items in this state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<AgendaStateKind>,
    /// At most this many items.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    /// Newest first instead of oldest first, so a surface reads the newest items without
    /// paging through every older one (spec 3.3 §5.1).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub newest_first: bool,
    /// Only the items after this one in the order asked: the last item of the page before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
}

impl AgendaQuery {
    /// The [`AgendaFilter`] the query selects.
    pub fn to_filter(&self) -> AgendaFilter {
        let mut filter = AgendaFilter::default();
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

/// `GET /kernel/agenda` answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgendaResponse {
    /// The items, oldest first, or newest first when asked.
    pub items: Vec<AgendaItem>,
}

/// The answer of an agenda action: the item after it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgendaItemResponse {
    /// The item.
    pub item: AgendaItem,
}

/// `POST /kernel/agenda/{id}/reject` body.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RejectRequest {
    /// Why, recorded on the event and in the audit record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// `POST /kernel/agenda/{id}/schedule` body: `adk-rust agenda schedule ID --at|--cron`.
///
/// # Example
///
/// ```rust
/// use adk_kernel::{Wake, WakeSet};
/// use adk_kernel::control::api::ScheduleRequest;
///
/// let request = ScheduleRequest { wake: WakeSet::any_of([Wake::At(chrono::Utc::now())]) };
/// assert!(serde_json::to_value(&request).unwrap()["wake"]["anyOf"].is_array());
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleRequest {
    /// The wake set the item's process sleeps on.
    pub wake: WakeSet,
}

/// `GET /kernel/drivers` answer (spec 3.2 §7.7, §7.11).
#[cfg(feature = "drivers")]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DriversResponse {
    /// The mounted drivers, ordered by id.
    pub drivers: Vec<crate::drivers::DriverView>,
}

/// `POST /kernel/drivers/{id}/mount` body (spec 3.2 §7.7).
///
/// Without `confirm` the kernel answers what the mount would approve and mounts nothing;
/// with it, the caller approves that and the driver is mounted.
///
/// # Example
///
/// ```rust
/// use adk_kernel::control::api::MountDriverRequest;
///
/// let request = MountDriverRequest::new("id = \"fs\"\ncommand = \"mcp-fs\"\n").confirmed();
/// assert!(request.confirm);
/// ```
#[cfg(feature = "drivers")]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MountDriverRequest {
    /// The manifest, as its TOML file holds it.
    pub manifest: String,
    /// Whether the caller approves the mount.
    #[serde(default)]
    pub confirm: bool,
}

#[cfg(feature = "drivers")]
impl MountDriverRequest {
    /// A request to see what mounting `manifest` approves.
    pub fn new(manifest: impl Into<String>) -> Self {
        Self { manifest: manifest.into(), confirm: false }
    }

    /// The same request, approving the mount.
    #[must_use]
    pub fn confirmed(mut self) -> Self {
        self.confirm = true;
        self
    }
}

/// `POST /kernel/drivers/{id}/mount` answer.
#[cfg(feature = "drivers")]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MountDriverResponse {
    /// What the mount approves: the command line, the partitions, the effect classes.
    pub proposal: crate::drivers::DriverProposal,
    /// The mounted driver, when the request confirmed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mounted: Option<crate::drivers::DriverView>,
}

/// `POST /kernel/drivers/{id}/{restart,reapprove}` answer.
#[cfg(feature = "drivers")]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DriverResponse {
    /// The driver after the action.
    pub driver: crate::drivers::DriverView,
}

/// `POST /kernel/drivers/{id}/unmount` answer.
#[cfg(feature = "drivers")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnmountDriverResponse {
    /// Whether a driver of that id was mounted.
    pub unmounted: bool,
}

/// How `GET /kernel/proc/ledger` answers (spec 3.2 §7.10, §7.11).
///
/// # Example
///
/// ```rust
/// use adk_kernel::control::api::LedgerFormat;
///
/// assert_eq!(serde_json::to_value(LedgerFormat::Jsonl).unwrap(), "jsonl");
/// ```
#[cfg(feature = "proc")]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LedgerFormat {
    /// A [`LedgerResponse`] page.
    #[default]
    Json,
    /// One row per line, the JSONL `cargo adk eval --ledger` reads
    /// (`application/x-ndjson`); the next page's cursor is the `x-ledger-next` header.
    Jsonl,
}

/// `GET /kernel/proc/ledger` query: `adk-rust proc ledger` (spec 3.2 §7.10, §7.11, §8.1).
///
/// `limit` defaults to [`LedgerParams::DEFAULT_LIMIT`] and is capped at
/// [`LedgerParams::MAX_LIMIT`]; page on with `after` set to the last row's `seq`.
///
/// # Example
///
/// ```rust
/// use adk_kernel::control::api::LedgerParams;
///
/// let params = LedgerParams { kind: Some("tool_call,intent".into()), after: Some(40), ..LedgerParams::default() };
/// let query = params.to_query();
/// assert_eq!(query.kinds, ["tool_call", "intent"]);
/// assert_eq!(query.limit, Some(LedgerParams::DEFAULT_LIMIT));
/// ```
#[cfg(feature = "proc")]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LedgerParams {
    /// Only rows of this partition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition: Option<PartitionId>,
    /// Only rows of these kinds, comma-separated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Only rows of this agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Only rows at or after this instant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<DateTime<Utc>>,
    /// Only rows before this instant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<DateTime<Utc>>,
    /// Only rows after this sequence number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<u64>,
    /// At most this many rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    /// JSON page or JSONL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<LedgerFormat>,
}

#[cfg(feature = "proc")]
impl LedgerParams {
    /// Rows a page holds when the query names no limit.
    pub const DEFAULT_LIMIT: usize = 500;
    /// Most rows one page holds.
    pub const MAX_LIMIT: usize = 5_000;

    /// The store query the parameters select.
    pub fn to_query(&self) -> crate::proc::LedgerQuery {
        let mut query = crate::proc::LedgerQuery::default()
            .limit(self.limit.unwrap_or(Self::DEFAULT_LIMIT).clamp(1, Self::MAX_LIMIT));
        if let Some(partition) = &self.partition {
            query = query.partition(partition.clone());
        }
        for kind in self.kind.iter().flat_map(|kinds| kinds.split(',')) {
            let kind = kind.trim();
            if !kind.is_empty() {
                query = query.kind(kind);
            }
        }
        if let Some(agent) = &self.agent {
            query = query.agent(agent.clone());
        }
        if let Some(from) = self.from {
            query = query.from(from);
        }
        if let Some(to) = self.to {
            query = query.to(to);
        }
        if let Some(after) = self.after {
            query = query.after(after);
        }
        query
    }
}

/// `GET /kernel/proc/ledger` answer.
#[cfg(feature = "proc")]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LedgerResponse {
    /// The rows, in sequence order.
    pub entries: Vec<crate::proc::LedgerEntry>,
    /// The cursor of the next page, when this one is full.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<u64>,
}

/// `GET /kernel/proc/baselines` answer: each partition's last pass (spec 3.2 §7.10).
#[cfg(feature = "proc")]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BaselinesResponse {
    /// One state per partition the pass has tested, by partition; its `baselines` holds
    /// every dimension's baseline and deviation test on `lastDay`.
    pub partitions: Vec<crate::proc::PassState>,
}

/// `GET /kernel/proc/observations` query (spec 3.2 §7.10, §7.11).
///
/// # Example
///
/// ```rust
/// use adk_kernel::control::api::ObservationsParams;
///
/// let params = ObservationsParams { limit: Some(3), ..ObservationsParams::default() };
/// assert_eq!(params.to_query().limit, Some(3));
/// ```
#[cfg(feature = "proc")]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservationsParams {
    /// Only observations of this partition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition: Option<PartitionId>,
    /// Only observations of this day or later.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<chrono::NaiveDate>,
    /// At most this many.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

#[cfg(feature = "proc")]
impl ObservationsParams {
    /// The store query the parameters select.
    pub fn to_query(&self) -> crate::proc::ObservationQuery {
        let mut query = crate::proc::ObservationQuery::default();
        if let Some(partition) = &self.partition {
            query = query.partition(partition.clone());
        }
        if let Some(since) = self.since {
            query = query.since(since);
        }
        if let Some(limit) = self.limit {
            query = query.limit(limit);
        }
        query
    }
}

/// `GET /kernel/proc/observations` answer.
#[cfg(feature = "proc")]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservationsResponse {
    /// The observations, newest day first.
    pub observations: Vec<crate::proc::Observation>,
}

/// `GET /kernel/partitions` answer (spec 3.2 §7.8, §7.11).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartitionsResponse {
    /// Every partition, ordered by id.
    pub partitions: Vec<crate::partition::Partition>,
}

/// `POST /kernel/partitions` and `POST /kernel/partitions/{id}/mode` answer: the partition
/// after the change (spec 3.2 §7.8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartitionResponse {
    /// The partition.
    pub partition: crate::partition::Partition,
}

/// `POST /kernel/partitions/{id}/mode` body (spec 3.2 §8.1 `partitions set-mode`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartitionModeRequest {
    /// The partition's new mode.
    pub mode: crate::partition::PartitionMode,
}
