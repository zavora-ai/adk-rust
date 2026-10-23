//! The agenda: proactivity under control (spec 3.2 §7.5).
//!
//! The agenda is the kernel-scope todo list: [`AgendaItem`]s that outlive a session and may
//! become processes. Proposing is free; running is governed. An item starts `Proposed`
//! and runs only once it is `Approved` — by a person, or by an Auto partition's policy
//! (K-I10) — and then only outside quiet hours and within the partition's daily budget
//! (K-I11). An item of [`AgendaOrigin::Consolidation`] needs a person's approval in every
//! mode, Auto included (K-I23).
//!
//! | State | Meaning |
//! |---|---|
//! | [`Proposed`](AgendaState::Proposed) | Waiting for approval, or held by a ReadOnly or Plan partition |
//! | [`Approved`](AgendaState::Approved) | Approved; the kernel schedules it next |
//! | [`Scheduled`](AgendaState::Scheduled) | A sleeping process waits on its wake set |
//! | [`Running`](AgendaState::Running) | Its process runs |
//! | [`Done`](AgendaState::Done), [`Rejected`](AgendaState::Rejected), [`Expired`](AgendaState::Expired) | Finished |
//!
//! Items live in an [`AgendaStore`] on every backend the [`ProcessStore`](crate::ProcessStore)
//! has: in-memory, SQLite, PostgreSQL and Redis.

use std::fmt;
use std::sync::Arc;

use adk_core::principal::{SubjectKind, SubjectRef};
use adk_core::{
    ApprovalAction, ApprovalPolicy, ApprovalRequest, ApprovalRisk, ApprovalScope, ApprovalService,
    ApprovalState, ApproverSelector, NewRun, RunBudget, RunState, ToolConfirmationRequest,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::consolidation::ConsolidationTick;
use crate::error::{KernelError, Result};
use crate::partition::{PartitionId, PartitionMode};
use crate::process::{ProcessKind, ProcessSpec, ProcessTable};
use crate::sched::{QueueClass, QuietHours};
use crate::supervise::Supervision;
use crate::wake::{Wake, WakeSet};

/// One item of the agenda (spec 3.2 §7.5).
///
/// `when` holds the wake the proposer asked for until the item is scheduled; an item with
/// none runs as soon as it is admitted.
///
/// # Example
///
/// ```rust
/// use adk_core::principal::SubjectRef;
/// use adk_kernel::{AgendaItem, AgendaOrigin, AgendaState};
///
/// let item = AgendaItem::new(
///     "item-1",
///     SubjectRef::human("alice"),
///     "Re-run the tests",
///     "The dependency bump lands tonight",
///     AgendaOrigin::Agent { pid: "run-7".into(), confidence: 1.0 },
///     chrono::Utc::now(),
/// )
/// .with_partition("work");
/// assert_eq!(item.state, AgendaState::Proposed);
/// let json = serde_json::to_value(&item).unwrap();
/// assert_eq!(json["origin"]["agent"]["pid"], "run-7");
/// assert_eq!(json["state"], "proposed");
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct AgendaItem {
    /// The item id.
    pub id: String,
    /// The partition the item belongs to.
    #[serde(default)]
    pub partition: PartitionId,
    /// Whom the item works for: the user its process runs as.
    pub subject: SubjectRef,
    /// A short title.
    pub title: String,
    /// Why the work is worth doing.
    pub rationale: String,
    /// Who proposed it.
    pub origin: AgendaOrigin,
    /// Where it stands.
    pub state: AgendaState,
    /// The plan steps, when the proposer produced some (spec 3.1 §7.11).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<PlanRef>,
    /// The budget of the item's process (`adk-core/src/budget.rs`); its cost cap is what
    /// admission reserves against the daily budget.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::serde_util::option_run_budget"
    )]
    pub budget: Option<RunBudget>,
    /// When the item was proposed.
    pub created_at: DateTime<Utc>,
    /// When an item not yet running expires; `None` never.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    /// The wake the proposer asked for (`propose_followup`'s `when`), kept until the item
    /// is scheduled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<Wake>,
}

impl AgendaItem {
    /// A `Proposed` item in the default partition with no plan, budget, expiry or wake.
    pub fn new(
        id: impl Into<String>,
        subject: SubjectRef,
        title: impl Into<String>,
        rationale: impl Into<String>,
        origin: AgendaOrigin,
        created_at: DateTime<Utc>,
    ) -> Self {
        Self {
            id: id.into(),
            partition: PartitionId::default(),
            subject,
            title: title.into(),
            rationale: rationale.into(),
            origin,
            state: AgendaState::Proposed,
            plan: None,
            budget: None,
            created_at,
            expires_at: None,
            when: None,
        }
    }

    /// Sets the partition.
    #[must_use]
    pub fn with_partition(mut self, partition: impl Into<PartitionId>) -> Self {
        self.partition = partition.into();
        self
    }

    /// Sets the state.
    #[must_use]
    pub fn with_state(mut self, state: AgendaState) -> Self {
        self.state = state;
        self
    }

    /// Sets the plan.
    #[must_use]
    pub fn with_plan(mut self, plan: PlanRef) -> Self {
        self.plan = Some(plan);
        self
    }

    /// Sets the budget.
    #[must_use]
    pub fn with_budget(mut self, budget: RunBudget) -> Self {
        self.budget = Some(budget);
        self
    }

    /// Sets the expiry.
    #[must_use]
    pub fn with_expiry(mut self, at: DateTime<Utc>) -> Self {
        self.expires_at = Some(at);
        self
    }

    /// Sets the wake the proposer asked for.
    #[must_use]
    pub fn with_when(mut self, when: Wake) -> Self {
        self.when = Some(when);
        self
    }

    /// The run that proposed the item, for an agent's proposal.
    pub fn proposer(&self) -> Option<&str> {
        match &self.origin {
            AgendaOrigin::Agent { pid, .. } => Some(pid),
            AgendaOrigin::User | AgendaOrigin::Consolidation | AgendaOrigin::Observation { .. } => {
                None
            }
        }
    }
}

/// Who proposed an [`AgendaItem`] (spec 3.2 §7.5).
///
/// # Example
///
/// ```rust
/// use adk_kernel::AgendaOrigin;
///
/// assert_eq!(serde_json::to_value(AgendaOrigin::Consolidation).unwrap(), "consolidation");
/// assert_eq!(AgendaOrigin::Agent { pid: "run-1".into(), confidence: 0.8 }.to_string(), "agent:run-1");
/// assert!(AgendaOrigin::Consolidation.needs_a_person());
/// assert!(!AgendaOrigin::User.needs_a_person());
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
#[non_exhaustive]
pub enum AgendaOrigin {
    /// A person added it.
    User,
    /// An agent proposed it with `propose_followup`.
    Agent {
        /// The proposing run; the item's process is its child.
        pid: String,
        /// How sure the proposer is the work is wanted, 0 to 1.
        confidence: f32,
    },
    /// The consolidation tick proposed it; it runs only after a person approves it, in
    /// every mode (K-I23).
    Consolidation,
    /// A `/proc` observation proposed it (spec 3.2 §7.10).
    Observation {
        /// The observation.
        id: String,
    },
}

impl AgendaOrigin {
    /// Whether only a person may approve items of this origin, whatever the partition's
    /// mode and policy: true for [`Consolidation`](Self::Consolidation) (K-I23).
    pub fn needs_a_person(&self) -> bool {
        matches!(self, Self::Consolidation)
    }

    /// The origin's name: `user`, `agent`, `consolidation` or `observation`.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Agent { .. } => "agent",
            Self::Consolidation => "consolidation",
            Self::Observation { .. } => "observation",
        }
    }
}

impl fmt::Display for AgendaOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Agent { pid, .. } => write!(f, "agent:{pid}"),
            Self::Observation { id } => write!(f, "observation:{id}"),
            Self::User | Self::Consolidation => f.write_str(self.kind()),
        }
    }
}

/// Where an [`AgendaItem`] stands (spec 3.2 §7.5).
///
/// # Example
///
/// ```rust
/// use adk_kernel::{AgendaState, AgendaStateKind, Wake, WakeSet};
///
/// let scheduled = AgendaState::Scheduled { wake: WakeSet::any_of([Wake::Manual]) };
/// assert_eq!(scheduled.kind(), AgendaStateKind::Scheduled);
/// assert!(scheduled.kind().is_open());
/// assert!(!AgendaState::Done.kind().is_open());
/// assert_eq!(serde_json::to_value(AgendaState::Running { pid: "run-2".into() }).unwrap()["running"]["pid"], "run-2");
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
#[non_exhaustive]
pub enum AgendaState {
    /// Waiting for approval.
    Proposed,
    /// Approved and not scheduled yet.
    Approved,
    /// Its process sleeps on `wake`.
    Scheduled {
        /// The wake set of the item's process.
        wake: WakeSet,
    },
    /// Its process runs.
    Running {
        /// The item's process.
        pid: String,
    },
    /// Its process finished.
    Done,
    /// A person or the policy rejected it.
    Rejected,
    /// It expired before it ran.
    Expired,
}

impl AgendaState {
    /// The state's kind, without its payload.
    pub fn kind(&self) -> AgendaStateKind {
        match self {
            Self::Proposed => AgendaStateKind::Proposed,
            Self::Approved => AgendaStateKind::Approved,
            Self::Scheduled { .. } => AgendaStateKind::Scheduled,
            Self::Running { .. } => AgendaStateKind::Running,
            Self::Done => AgendaStateKind::Done,
            Self::Rejected => AgendaStateKind::Rejected,
            Self::Expired => AgendaStateKind::Expired,
        }
    }
}

impl fmt::Display for AgendaState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Running { pid } => write!(f, "running:{pid}"),
            other => f.write_str(other.kind().as_str()),
        }
    }
}

/// The kind of an [`AgendaState`], without its payload: what an [`AgendaFilter`] selects
/// and an [`AgendaStore::transition`] compares (spec 3.2 §7.5).
///
/// # Example
///
/// ```rust
/// use adk_kernel::AgendaStateKind;
///
/// assert_eq!("scheduled".parse::<AgendaStateKind>().unwrap(), AgendaStateKind::Scheduled);
/// assert_eq!(serde_json::to_value(AgendaStateKind::Proposed).unwrap(), "proposed");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum AgendaStateKind {
    /// [`AgendaState::Proposed`].
    Proposed,
    /// [`AgendaState::Approved`].
    Approved,
    /// [`AgendaState::Scheduled`].
    Scheduled,
    /// [`AgendaState::Running`].
    Running,
    /// [`AgendaState::Done`].
    Done,
    /// [`AgendaState::Rejected`].
    Rejected,
    /// [`AgendaState::Expired`].
    Expired,
}

impl AgendaStateKind {
    /// Every kind, in lifecycle order.
    pub const ALL: [Self; 7] = [
        Self::Proposed,
        Self::Approved,
        Self::Scheduled,
        Self::Running,
        Self::Done,
        Self::Rejected,
        Self::Expired,
    ];

    /// The kind's name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Proposed => "proposed",
            Self::Approved => "approved",
            Self::Scheduled => "scheduled",
            Self::Running => "running",
            Self::Done => "done",
            Self::Rejected => "rejected",
            Self::Expired => "expired",
        }
    }

    /// Whether an item of this kind is open: not done, rejected or expired. Open items
    /// count against [`ProactivityPolicy::max_proposed_open`].
    pub fn is_open(self) -> bool {
        matches!(self, Self::Proposed | Self::Approved | Self::Scheduled | Self::Running)
    }
}

impl fmt::Display for AgendaStateKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for AgendaStateKind {
    type Err = crate::KernelError;

    fn from_str(text: &str) -> std::result::Result<Self, Self::Err> {
        let text = text.trim().to_ascii_lowercase();
        Self::ALL.into_iter().find(|kind| kind.as_str() == text).ok_or_else(|| {
            crate::KernelError::Config {
                reason: format!(
                    "unknown agenda state `{text}`; use proposed, approved, scheduled, running, done, rejected or expired"
                ),
            }
        })
    }
}

/// The plan an agent proposed with its follow-up (spec 3.2 §7.5, spec 3.1 §7.11): the
/// steps as the harness writes them, each a spec 3.1 `PlanStep` in JSON, and their digest.
/// The kernel stores them without reading them (§6 D-1).
///
/// # Example
///
/// ```rust
/// use adk_kernel::PlanRef;
///
/// let plan = PlanRef::new("sha256:ab12", vec![serde_json::json!({"id": "1", "title": "Run the tests", "instruction": "cargo test"})]);
/// assert_eq!(plan.steps.len(), 1);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct PlanRef {
    /// The digest of the steps, as the harness computes a plan's hash.
    pub plan_hash: String,
    /// The steps.
    pub steps: Vec<Value>,
}

impl PlanRef {
    /// A plan of `steps` with digest `plan_hash`.
    pub fn new(plan_hash: impl Into<String>, steps: Vec<Value>) -> Self {
        Self { plan_hash: plan_hash.into(), steps }
    }
}

/// How agenda proposals are approved (spec 3.2 §7.5).
///
/// | Value | ReadOnly, Plan | Ask | Auto |
/// |---|---|---|---|
/// | [`FollowMode`](Self::FollowMode) (default) | Stays `Proposed` | An inbox approval | Scheduled |
/// | [`AlwaysAsk`](Self::AlwaysAsk) | An inbox approval | An inbox approval | An inbox approval |
/// | [`Never`](Self::Never) | Stays `Proposed` | Stays `Proposed` | Stays `Proposed` |
///
/// Under `Never` the kernel neither asks nor schedules; an item runs only when a person
/// approves it through the agenda (`adk-rust agenda approve`). Whatever the value, an item
/// of [`AgendaOrigin::Consolidation`] is never scheduled without a person (K-I23).
///
/// # Example
///
/// ```rust
/// use adk_kernel::ProactiveApproval;
///
/// assert_eq!(ProactiveApproval::default(), ProactiveApproval::FollowMode);
/// assert_eq!(serde_json::to_value(ProactiveApproval::AlwaysAsk).unwrap(), "always_ask");
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ProactiveApproval {
    /// The partition's mode decides: ReadOnly and Plan hold, Ask asks, Auto schedules.
    #[default]
    FollowMode,
    /// Every proposal goes to the inbox, whatever the mode.
    AlwaysAsk,
    /// No proposal is put to the inbox or scheduled by the kernel.
    Never,
}

/// The proactivity limits of a partition (spec 3.2 §7.5, §9.1 runaway proactivity).
///
/// A proposal past `max_proposed_open` open items, or made while the daily budget is
/// spent, is rejected with a reason on the event bus. `daily_budget` bounds the proactive
/// processes of a partition per UTC day: admission holds one whose budget's cost cap the
/// day's remaining proactive spend does not cover. Proactive processes never start inside
/// `quiet_hours`. `consolidation` turns the consolidation tick on; it is `None` in the
/// library and on servers, and the CLI sets it in its Ask and Auto partitions.
/// `review_period` bounds the daily review of standing rules, sleepers and freezes
/// ([`ReviewReport`](crate::ReviewReport)).
///
/// # Example
///
/// ```rust
/// use adk_core::RunBudget;
/// use adk_kernel::{ConsolidationTick, ProactiveApproval, ProactivityPolicy};
///
/// let policy = ProactivityPolicy::default()
///     .with_daily_budget(RunBudget::new().max_cost_micro_usd(2_000_000))
///     .with_quiet_hours("22:00-07:00".parse().unwrap())
///     .with_consolidation(ConsolidationTick::default());
/// assert_eq!(policy.max_proposed_open, 10);
/// assert_eq!(policy.approval, ProactiveApproval::FollowMode);
/// let json = serde_json::to_value(&policy).unwrap();
/// assert_eq!(json["dailyBudget"]["maxCostMicroUsd"], 2_000_000);
/// assert!(ProactivityPolicy::default().consolidation.is_none());
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[non_exhaustive]
pub struct ProactivityPolicy {
    /// Most open items per partition (10).
    pub max_proposed_open: usize,
    /// Model calls, tokens and cost for the partition's proactive processes per UTC day;
    /// unlimited by default.
    #[serde(with = "crate::serde_util::run_budget")]
    pub daily_budget: RunBudget,
    /// Proactive processes never start inside these.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quiet_hours: Option<QuietHours>,
    /// How proposals are approved.
    pub approval: ProactiveApproval,
    /// The consolidation tick; the model and effort come from the harness.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consolidation: Option<ConsolidationTick>,
    /// How old a standing rule without an expiry, a sleeper or a freeze-parked process gets
    /// before the maintenance tick's daily review reports it (spec 3.2 §7.4, §9.1); 90 days.
    #[serde(rename = "reviewPeriodMs", with = "crate::serde_util::millis")]
    pub review_period: std::time::Duration,
}

impl ProactivityPolicy {
    /// The default `max_proposed_open`.
    pub const DEFAULT_MAX_PROPOSED_OPEN: usize = 10;

    /// The default `review_period`, 90 days; the spec names the period without a number.
    pub const DEFAULT_REVIEW_PERIOD: std::time::Duration =
        std::time::Duration::from_secs(90 * 24 * 60 * 60);

    /// The default policy: ten open items, no daily limit, no quiet hours, approval
    /// following the mode, no consolidation tick.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the most open items per partition.
    #[must_use]
    pub fn with_max_proposed_open(mut self, max: usize) -> Self {
        self.max_proposed_open = max;
        self
    }

    /// Sets the daily proactive budget.
    #[must_use]
    pub fn with_daily_budget(mut self, budget: RunBudget) -> Self {
        self.daily_budget = budget;
        self
    }

    /// Sets the quiet hours.
    #[must_use]
    pub fn with_quiet_hours(mut self, quiet_hours: QuietHours) -> Self {
        self.quiet_hours = Some(quiet_hours);
        self
    }

    /// Sets how proposals are approved.
    #[must_use]
    pub fn with_approval(mut self, approval: ProactiveApproval) -> Self {
        self.approval = approval;
        self
    }

    /// Turns the consolidation tick on.
    #[must_use]
    pub fn with_consolidation(mut self, tick: ConsolidationTick) -> Self {
        self.consolidation = Some(tick);
        self
    }

    /// Sets the review period.
    #[must_use]
    pub fn with_review_period(mut self, period: std::time::Duration) -> Self {
        self.review_period = period;
        self
    }
}

impl Default for ProactivityPolicy {
    fn default() -> Self {
        Self {
            max_proposed_open: Self::DEFAULT_MAX_PROPOSED_OPEN,
            daily_budget: RunBudget::new(),
            quiet_hours: None,
            approval: ProactiveApproval::FollowMode,
            consolidation: None,
            review_period: Self::DEFAULT_REVIEW_PERIOD,
        }
    }
}

/// The kernel's store of agenda items (spec 3.2 §7.5, §13 Q8), on every backend the
/// [`ProcessStore`](crate::ProcessStore) has.
///
/// State changes go through [`transition`](Self::transition), a compare-and-set on the
/// stored state's kind, so two replicas approving or admitting one item move it once.
///
/// # Example
///
/// ```rust
/// use adk_core::principal::SubjectRef;
/// use adk_kernel::{AgendaFilter, AgendaItem, AgendaOrigin, AgendaState, AgendaStateKind, AgendaStore, InMemoryAgendaStore};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_kernel::Result<()> {
/// let store = InMemoryAgendaStore::new();
/// let item = AgendaItem::new("item-1", SubjectRef::human("alice"), "Tidy", "Weekly", AgendaOrigin::User, chrono::Utc::now());
/// store.put(item.clone()).await?;
/// assert!(store.transition(item.clone().with_state(AgendaState::Approved), AgendaStateKind::Proposed).await?);
/// assert!(!store.transition(item.with_state(AgendaState::Rejected), AgendaStateKind::Proposed).await?);
/// let open = store.list(&AgendaFilter::default().states([AgendaStateKind::Approved])).await?;
/// assert_eq!(open.len(), 1);
/// # Ok(())
/// # }
/// ```
#[async_trait]
pub trait AgendaStore: Send + Sync {
    /// Inserts `item`, or replaces the item with the same id.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when the backend fails.
    async fn put(&self, item: AgendaItem) -> Result<()>;

    /// The item `id`, or `None`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when the backend fails.
    async fn get(&self, id: &str) -> Result<Option<AgendaItem>>;

    /// The items `filter` selects, oldest first (by `created_at`, then id), at most
    /// `filter.limit`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when the backend fails.
    async fn list(&self, filter: &AgendaFilter) -> Result<Vec<AgendaItem>>;

    /// Writes `item` over the stored item with its id when the stored item's state is of
    /// kind `from`; `false`, writing nothing, when there is no such item or its state is
    /// of another kind.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when the backend fails.
    async fn transition(&self, item: AgendaItem, from: AgendaStateKind) -> Result<bool>;

    /// Deletes the item `id`; `false` when there was none.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when the backend fails.
    async fn delete(&self, id: &str) -> Result<bool>;
}

/// Which agenda items [`AgendaStore::list`] returns (spec 3.2 §7.5, §8.1 `adk-rust agenda
/// list`). An empty field matches everything.
///
/// # Example
///
/// ```rust
/// use adk_core::principal::SubjectRef;
/// use adk_kernel::{AgendaFilter, AgendaItem, AgendaOrigin, AgendaStateKind};
///
/// let filter = AgendaFilter::default().partition("work").open();
/// let item = AgendaItem::new("item-1", SubjectRef::human("alice"), "t", "r", AgendaOrigin::User, chrono::Utc::now()).with_partition("work");
/// assert!(filter.matches(&item));
/// assert!(!AgendaFilter::default().states([AgendaStateKind::Done]).matches(&item));
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct AgendaFilter {
    /// Only items of this partition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition: Option<PartitionId>,
    /// Only items in one of these states.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub states: Vec<AgendaStateKind>,
    /// At most this many items.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

impl AgendaFilter {
    /// Only items of `partition`.
    #[must_use]
    pub fn partition(mut self, partition: impl Into<PartitionId>) -> Self {
        self.partition = Some(partition.into());
        self
    }

    /// Only items in one of `states`.
    #[must_use]
    pub fn states(mut self, states: impl IntoIterator<Item = AgendaStateKind>) -> Self {
        self.states = states.into_iter().collect();
        self
    }

    /// Only open items ([`AgendaStateKind::is_open`]).
    #[must_use]
    pub fn open(self) -> Self {
        self.states(AgendaStateKind::ALL.into_iter().filter(|kind| kind.is_open()))
    }

    /// At most `limit` items.
    #[must_use]
    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Whether `item` passes the partition and state fields; `limit` is not checked here.
    pub fn matches(&self, item: &AgendaItem) -> bool {
        self.partition.as_ref().is_none_or(|partition| *partition == item.partition)
            && (self.states.is_empty() || self.states.contains(&item.state.kind()))
    }
}

/// The approval-inbox tool name of agenda proposals (spec 3.2 §7.5): an Ask partition's
/// proposal waits in the inbox as an `ApprovalRequest` with this `tool_name`, the item as
/// `args_redacted` and `allowed_actions = [Approve, Edit, Reject]`.
pub const AGENDA_TOOL_NAME: &str = "kernel:agenda";

/// What [`Agenda::propose`] did with a proposal (spec 3.2 §7.5).
///
/// # Example
///
/// ```rust
/// use adk_kernel::ProposalOutcome;
///
/// let outcome = ProposalOutcome::Rejected { reason: "ten open items".into() };
/// assert!(outcome.item().is_none());
/// assert_eq!(serde_json::to_value(&outcome).unwrap()["outcome"], "rejected");
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "camelCase", rename_all_fields = "camelCase")]
#[non_exhaustive]
pub enum ProposalOutcome {
    /// Recorded `Proposed` and held for a person: a ReadOnly or Plan partition, or
    /// [`ProactiveApproval::Never`].
    Held {
        /// The item.
        item: AgendaItem,
    },
    /// Recorded `Proposed` and put to the approval inbox.
    AwaitingApproval {
        /// The item.
        item: AgendaItem,
        /// The inbox request.
        request_id: String,
    },
    /// Approved by an Auto partition's policy and scheduled.
    Scheduled {
        /// The item.
        item: AgendaItem,
        /// Its process.
        pid: String,
    },
    /// Refused by the proactivity policy; nothing was recorded.
    Rejected {
        /// Which limit the proposal would exceed.
        reason: String,
    },
}

impl ProposalOutcome {
    /// The recorded item, `None` for a rejection.
    pub fn item(&self) -> Option<&AgendaItem> {
        match self {
            Self::Held { item }
            | Self::AwaitingApproval { item, .. }
            | Self::Scheduled { item, .. } => Some(item),
            Self::Rejected { .. } => None,
        }
    }
}

/// Where a new proposal goes (spec 3.2 §7.5, K-I10, K-I23).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Route {
    /// Stays `Proposed` for a person.
    Hold,
    /// To the approval inbox.
    Ask,
    /// Approved by the policy and scheduled.
    Schedule,
}

/// Routes a proposal of `origin` from a partition in `mode` under `approval`: only an Auto
/// partition following its mode schedules without a person, and never an item the
/// consolidation tick proposed (K-I23).
pub(crate) fn route(
    approval: ProactiveApproval,
    mode: PartitionMode,
    origin: &AgendaOrigin,
) -> Route {
    let follow = match mode {
        PartitionMode::ReadOnly | PartitionMode::Plan => Route::Hold,
        PartitionMode::Ask => Route::Ask,
        PartitionMode::Auto if origin.needs_a_person() => Route::Ask,
        PartitionMode::Auto => Route::Schedule,
    };
    match approval {
        ProactiveApproval::FollowMode => follow,
        ProactiveApproval::AlwaysAsk => Route::Ask,
        ProactiveApproval::Never => Route::Hold,
    }
}

/// The agenda of a kernel (spec 3.2 §7.5): proposals, approvals and the processes items
/// become. [`Kernel::agenda`](crate::Kernel::agenda) returns it; cloning shares it.
///
/// | Call | Effect |
/// |---|---|
/// | [`propose`](Self::propose) | Checks the [`ProactivityPolicy`], records the item `Proposed`, and routes it: held, to the inbox, or — in an Auto partition following its mode — approved and scheduled |
/// | [`approve`](Self::approve) | `Proposed` → `Approved` → `Scheduled`: a [`ProcessKind::Proposed`] process whose parent is the proposing run |
/// | [`reject`](Self::reject) | `Proposed` or `Approved` → `Rejected` |
/// | [`schedule`](Self::schedule) | Puts the item's process to sleep on a wake set, approving a proposed item first |
///
/// Every change is published as a [`KernelEvent::Agenda`](crate::KernelEvent::Agenda) and
/// audited as `kernel.agenda.*`. Inbox decisions, expiry and finished processes are applied
/// on every scheduler tick.
///
/// # Example
///
/// ```rust,no_run
/// use adk_core::principal::SubjectRef;
/// use adk_kernel::{AgendaItem, AgendaOrigin, Kernel, PartitionMode, ProposalOutcome};
///
/// # async fn example(kernel: Kernel) -> adk_kernel::Result<()> {
/// let item = AgendaItem::new(
///     "item-1",
///     SubjectRef::human("alice"),
///     "Re-run the tests",
///     "The dependency bump lands tonight",
///     AgendaOrigin::User,
///     chrono::Utc::now(),
/// );
/// if let ProposalOutcome::AwaitingApproval { request_id, .. } = kernel.agenda().propose(item, PartitionMode::Ask).await? {
///     println!("approve {request_id} in the inbox");
/// }
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Agenda {
    inner: Arc<AgendaInner>,
}

struct AgendaInner {
    sup: Arc<Supervision>,
    processes: ProcessTable,
    approvals: Option<Arc<dyn ApprovalService>>,
}

impl fmt::Debug for Agenda {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Agenda")
            .field("policy", &self.inner.sup.proactivity)
            .field("inbox", &self.inner.approvals.is_some())
            .finish_non_exhaustive()
    }
}

/// A handle on a kernel's agenda that does not keep the kernel alive, bound when the kernel
/// is built (spec 3.2 §7.5, §10 HK1): the harness's `propose_followup` tool exists before the
/// kernel that hosts it.
///
/// # Example
///
/// ```rust
/// use adk_kernel::AgendaHandle;
///
/// let handle = AgendaHandle::new();
/// assert!(handle.get().is_none(), "no kernel was built with it");
/// ```
#[derive(Clone, Default)]
pub struct AgendaHandle {
    slot: Arc<std::sync::RwLock<std::sync::Weak<AgendaInner>>>,
}

impl fmt::Debug for AgendaHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgendaHandle").field("bound", &self.get().is_some()).finish()
    }
}

impl AgendaHandle {
    /// An unbound handle.
    pub fn new() -> Self {
        Self::default()
    }

    /// The agenda of the kernel last built with this handle, while that kernel lives.
    pub fn get(&self) -> Option<Agenda> {
        let slot = self.slot.read().unwrap_or_else(|e| e.into_inner());
        slot.upgrade().map(|inner| Agenda { inner })
    }

    pub(crate) fn bind(&self, agenda: &Agenda) {
        *self.slot.write().unwrap_or_else(|e| e.into_inner()) = Arc::downgrade(&agenda.inner);
    }
}

/// The session an item's process runs in: `agenda-<item id>`, a session of its own.
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_kernel::agenda::item_session("item-1"), "agenda-item-1");
/// ```
pub fn item_session(item_id: &str) -> String {
    format!("agenda-{item_id}")
}

/// The approver group of the approvals inbox `adk-rust up` serves: the control plane's
/// `operator` scope.
const OPERATOR_GROUP: &str = "operator";

/// The id of the inbox request of item `item_id`.
fn request_id(item_id: &str) -> String {
    format!("agenda_{item_id}")
}

/// The user turn an item's process starts from.
fn prompt(item: &AgendaItem) -> String {
    let mut text = format!(
        "Follow-up from the agenda (item {}): {}\n\nWhy: {}",
        item.id, item.title, item.rationale
    );
    if let Some(plan) = &item.plan
        && !plan.steps.is_empty()
    {
        text.push_str("\n\nSteps:");
        for (index, step) in plan.steps.iter().enumerate() {
            let title = step.get("title").and_then(Value::as_str).unwrap_or_default();
            let instruction = step.get("instruction").and_then(Value::as_str).unwrap_or_default();
            text.push_str(&format!("\n{}. {title}: {instruction}", index + 1));
        }
    }
    text
}

fn not_found(id: &str) -> KernelError {
    KernelError::AgendaNotFound { item: id.to_string() }
}

fn wrong_state(item: &AgendaItem, action: &'static str) -> KernelError {
    KernelError::AgendaState { item: item.id.clone(), state: item.state.kind(), action }
}

impl Agenda {
    pub(crate) fn new(
        sup: Arc<Supervision>,
        processes: ProcessTable,
        approvals: Option<Arc<dyn ApprovalService>>,
    ) -> Self {
        Self { inner: Arc::new(AgendaInner { sup, processes, approvals }) }
    }

    /// The store the items live in.
    pub fn store(&self) -> &Arc<dyn AgendaStore> {
        &self.inner.sup.agenda
    }

    /// Every partition's proactivity limits.
    pub fn policy(&self) -> &ProactivityPolicy {
        &self.inner.sup.proactivity
    }

    /// The item `id`, or `None`.
    ///
    /// # Errors
    ///
    /// Returns a store error.
    pub async fn get(&self, id: &str) -> Result<Option<AgendaItem>> {
        self.store().get(id).await
    }

    /// The items `filter` selects, oldest first.
    ///
    /// # Errors
    ///
    /// Returns a store error.
    pub async fn list(&self, filter: &AgendaFilter) -> Result<Vec<AgendaItem>> {
        self.store().list(filter).await
    }

    /// Proposes `item` from a partition in `mode` (spec 3.2 §7.5, §9.1 runaway
    /// proactivity).
    ///
    /// The policy refuses a proposal — published `proposed` → `rejected` with the reason
    /// and audited, nothing stored — past `max_proposed_open` open items in the partition,
    /// once the day's proactive budget is spent, or whose own budget exceeds the daily one.
    /// Otherwise the item is stored `Proposed` and routed by [`ProactiveApproval`]: held in
    /// ReadOnly and Plan, put to the inbox in Ask, approved and scheduled in Auto. An item
    /// of [`AgendaOrigin::Consolidation`] goes to the inbox in Auto too (K-I23). Without an
    /// approval service an Ask proposal is held.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Config`] for an empty title or an item not `Proposed`, or a
    /// store error.
    pub async fn propose(
        &self,
        mut item: AgendaItem,
        mode: PartitionMode,
    ) -> Result<ProposalOutcome> {
        if item.title.trim().is_empty() {
            return Err(KernelError::Config { reason: "an agenda item needs a title".into() });
        }
        if item.state != AgendaState::Proposed {
            return Err(KernelError::Config {
                reason: format!("a new agenda item is proposed, not {}", item.state.kind()),
            });
        }
        let sup = &self.inner.sup;
        if let Some(reason) = self.refusal(&item).await? {
            item.state = AgendaState::Rejected;
            sup.agenda_changed(&item, &AgendaState::Proposed, Some(&reason), None).await;
            tracing::info!(agenda.item = %item.id, reason = %reason, "proposal refused by the proactivity policy");
            return Ok(ProposalOutcome::Rejected { reason });
        }
        sup.agenda.put(item.clone()).await?;
        let by =
            item.proposer().map(|pid| SubjectRef::new(SubjectKind::Agent, format!("run:{pid}")));
        sup.agenda_changed(&item, &AgendaState::Proposed, Some("proposed"), by.as_ref()).await;
        let mut route = route(sup.proactivity.approval, mode, &item.origin);
        if route == Route::Ask && self.inner.approvals.is_none() {
            route = Route::Hold;
        }
        match route {
            Route::Hold => Ok(ProposalOutcome::Held { item }),
            Route::Ask => {
                let request_id = self.submit(&item).await?;
                Ok(ProposalOutcome::AwaitingApproval { item, request_id })
            }
            Route::Schedule => {
                let policy = SubjectRef::service("kernel:proactivity-policy");
                let item = self
                    .approve_as(item, &policy, "admitted by the Auto partition's policy")
                    .await?;
                let pid = match &item.state {
                    AgendaState::Scheduled { .. } => self.process_of(&item.id).await?,
                    _ => None,
                };
                match pid {
                    Some(pid) => Ok(ProposalOutcome::Scheduled { item, pid }),
                    None => Ok(ProposalOutcome::Held { item }),
                }
            }
        }
    }

    /// Why the policy refuses `item`, or `None`.
    async fn refusal(&self, item: &AgendaItem) -> Result<Option<String>> {
        let sup = &self.inner.sup;
        let policy = &sup.proactivity;
        let open = self
            .store()
            .list(&AgendaFilter::default().partition(item.partition.clone()).open())
            .await?
            .len();
        if open >= policy.max_proposed_open {
            return Ok(Some(format!(
                "partition {} already has {open} open agenda items, the most max_proposed_open ({}) allows",
                item.partition, policy.max_proposed_open
            )));
        }
        if let Some(cap) = policy.daily_budget.max_cost_micro_usd {
            let need =
                item.budget.as_ref().and_then(|budget| budget.max_cost_micro_usd).unwrap_or(0);
            if need > cap {
                return Ok(Some(format!(
                    "its budget of {need} micro-USD exceeds the daily proactive budget of {cap}"
                )));
            }
            let spent = sup.proactive_spent(&item.partition).await;
            if spent >= cap {
                return Ok(Some(format!(
                    "partition {} spent its daily proactive budget ({spent} of {cap} micro-USD)",
                    item.partition
                )));
            }
        }
        Ok(None)
    }

    /// Puts `item` to the approval inbox; returns the request id.
    async fn submit(&self, item: &AgendaItem) -> Result<String> {
        let Some(approvals) = &self.inner.approvals else {
            return Err(KernelError::Config {
                reason: "the kernel has no approval service".into(),
            });
        };
        let sup = &self.inner.sup;
        let session = match item.proposer() {
            Some(pid) => match sup.runs.get(pid).await? {
                Some(run) => crate::supervise::run_session(&run),
                None => item_session(&item.id),
            },
            None => item_session(&item.id),
        };
        let scope = ApprovalScope::new(sup.app_name.clone(), item.subject.id.clone(), session);
        let args = serde_json::to_value(item)?;
        let call = ToolConfirmationRequest::new(AGENDA_TOOL_NAME, args.clone());
        // The item's user, or the operators the control plane's socket and loopback serve,
        // decide it; either is a person.
        let policy = ApprovalPolicy::new([
            ApproverSelector::human(item.subject.id.clone()),
            ApproverSelector::group(OPERATOR_GROUP),
        ])
        .allow_self_approval();
        let id = request_id(&item.id);
        let mut request = ApprovalRequest::new(id.clone(), scope, &call, policy)
            .with_summary(format!("Agenda follow-up: {}", item.title))
            .with_risk(ApprovalRisk::Medium)
            .with_redacted_args(args)
            .with_allowed_actions([
                ApprovalAction::Approve,
                ApprovalAction::Edit,
                ApprovalAction::Reject,
            ])
            .with_requester(item.subject.clone());
        if let Some(at) = item.expires_at {
            request = request.with_expires_at(at);
        }
        if let Some(pid) = item.proposer() {
            request = request.with_run_id(pid.to_string());
        }
        approvals.submit(request).await?;
        tracing::info!(agenda.item = %item.id, approval.id = %id, "agenda proposal put to the approval inbox");
        Ok(id)
    }

    /// Approves the proposed item `id` as `by`, then schedules it: `Proposed` → `Approved` →
    /// `Scheduled` (spec 3.2 §7.5, K-I10). An item of [`AgendaOrigin::Consolidation`] needs
    /// a person (K-I23). A pending inbox request for the item is withdrawn.
    ///
    /// Returns the item as it stands: `Scheduled`, or `Approved` when its process could not
    /// be created yet (a freeze, a full queue), which the next tick retries.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::AgendaNotFound`], [`KernelError::AgendaState`] when the item is
    /// not `Proposed`, [`KernelError::Unauthorized`] when `by` is not a person and the item
    /// needs one, or a store error.
    pub async fn approve(&self, id: &str, by: &SubjectRef) -> Result<AgendaItem> {
        let item = self.get(id).await?.ok_or_else(|| not_found(id))?;
        let approved = self.approve_as(item, by, "approved").await?;
        self.withdraw(id, "approved through the agenda").await;
        Ok(approved)
    }

    async fn approve_as(
        &self,
        item: AgendaItem,
        by: &SubjectRef,
        reason: &str,
    ) -> Result<AgendaItem> {
        if item.state != AgendaState::Proposed {
            return Err(wrong_state(&item, "approved"));
        }
        if item.origin.needs_a_person() && by.kind != SubjectKind::Human {
            return Err(KernelError::Unauthorized {
                reason: format!(
                    "agenda item {} came from the consolidation tick, so only a person approves it (K-I23)",
                    item.id
                ),
            });
        }
        let from = item.state.clone();
        let approved = item.with_state(AgendaState::Approved);
        if !self.inner.sup.move_item(&approved, &from, Some(reason), Some(by)).await? {
            let current = self.get(&approved.id).await?.ok_or_else(|| not_found(&approved.id))?;
            return Err(wrong_state(&current, "approved"));
        }
        Ok(self.start(approved, by).await)
    }

    /// Creates the process of the approved `item`, asleep on its wake (or on the end of
    /// quiet hours, or now), and marks it `Scheduled`; leaves it `Approved` when the
    /// process cannot be created yet.
    async fn start(&self, item: AgendaItem, by: &SubjectRef) -> AgendaItem {
        match self.spawn(&item, None).await {
            Ok((pid, wake)) => {
                let from = item.state.clone();
                let scheduled = item.clone().with_state(AgendaState::Scheduled { wake });
                match self
                    .inner
                    .sup
                    .move_item(&scheduled, &from, Some(&format!("process {pid}")), Some(by))
                    .await
                {
                    Ok(true) => scheduled,
                    Ok(false) | Err(_) => self.get(&item.id).await.ok().flatten().unwrap_or(item),
                }
            }
            Err(error) => {
                tracing::warn!(agenda.item = %item.id, error = %error, "could not create the agenda item's process yet; the next tick retries");
                item
            }
        }
    }

    /// Creates or finds the process of `item` (spec 3.2 §7.5, §7.9): asleep on `wake`, or on
    /// the item's own `when`; otherwise awake, with `not_before` at the end of quiet hours
    /// when they hold now. Returns the pid and the wake set the item is scheduled on.
    async fn spawn(&self, item: &AgendaItem, wake: Option<WakeSet>) -> Result<(String, WakeSet)> {
        let sup = &self.inner.sup;
        let now = sup.clock.now();
        let asleep = wake.or_else(|| item.when.clone().map(|when| WakeSet::any_of([when])));
        let asleep = match (asleep, item.expires_at) {
            (Some(set), Some(at)) => Some(set.with_expiry(at, crate::wake::ExpiryAction::Cancel)),
            (set, _) => set,
        };
        let release = sup.quiet_release(now);
        let proposer = match item.proposer() {
            Some(pid) => sup.runs.get(pid).await?,
            None => None,
        };
        let target =
            proposer.as_ref().map_or_else(|| sup.app_name.clone(), |run| run.target.clone());
        let mut run = NewRun::new(
            QueueClass::BACKGROUND,
            sup.app_name.clone(),
            item.subject.id.clone(),
            target,
            Value::String(prompt(item)),
        );
        run.session_id = Some(item_session(&item.id));
        run.idempotency_key = Some(format!("agenda:{}", item.id));
        if asleep.is_none() && release > now {
            run.not_before = Some(release);
        }
        if let Some(proposer) = &proposer {
            run.principal = proposer.principal.clone();
            // Auto and standing rules apply only when the proposing run's did (K-I7).
            run.authenticated = proposer.authenticated;
        }
        let mut spec = ProcessSpec::new(ProcessKind::Proposed)
            .with_partition(item.partition.clone())
            .with_agenda_item(item.id.clone());
        if let Some(set) = &asleep {
            spec = spec.with_wake(set.clone());
        }
        if let Some(pid) = item.proposer() {
            spec = spec.with_parent(pid);
        }
        let process = self.inner.processes.spawn(run, spec).await?;
        let scheduled = asleep.unwrap_or_else(|| WakeSet::any_of([Wake::At(release)]));
        Ok((process.meta.pid, scheduled))
    }

    /// The process of item `id`, the run keyed `agenda:<id>`.
    async fn process_of(&self, id: &str) -> Result<Option<String>> {
        let item = self.get(id).await?;
        let filter = crate::ProcessFilter::default();
        let filter = match &item {
            Some(item) => filter.partition(item.partition.clone()),
            None => filter,
        };
        Ok(self
            .inner
            .sup
            .processes
            .list(&filter)
            .await?
            .into_iter()
            .find(|meta| {
                meta.agenda_item.as_deref() == Some(id) && meta.kind == ProcessKind::Proposed
            })
            .map(|meta| meta.pid))
    }

    /// Rejects item `id` as `by`: a `Proposed` or `Approved` item becomes `Rejected`, and a
    /// pending inbox request for it is withdrawn.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::AgendaNotFound`], [`KernelError::AgendaState`] when the item is
    /// past approval, or a store error.
    pub async fn reject(
        &self,
        id: &str,
        by: &SubjectRef,
        reason: Option<&str>,
    ) -> Result<AgendaItem> {
        let item = self.get(id).await?.ok_or_else(|| not_found(id))?;
        let rejected = self.reject_as(item, by, reason.unwrap_or("rejected")).await?;
        self.withdraw(id, "rejected through the agenda").await;
        Ok(rejected)
    }

    async fn reject_as(
        &self,
        item: AgendaItem,
        by: &SubjectRef,
        reason: &str,
    ) -> Result<AgendaItem> {
        if !matches!(item.state, AgendaState::Proposed | AgendaState::Approved) {
            return Err(wrong_state(&item, "rejected"));
        }
        let from = item.state.clone();
        let rejected = item.with_state(AgendaState::Rejected);
        if !self.inner.sup.move_item(&rejected, &from, Some(reason), Some(by)).await? {
            let current = self.get(&rejected.id).await?.ok_or_else(|| not_found(&rejected.id))?;
            return Err(wrong_state(&current, "rejected"));
        }
        Ok(rejected)
    }

    /// Schedules item `id` on `wake` as `by` (spec 3.2 §7.5, §8.1 `adk-rust agenda schedule
    /// ID --at|--cron`): a `Proposed` item is approved as by [`approve`](Self::approve) and
    /// its process created asleep on `wake`; an `Approved` one gets its process asleep on
    /// `wake`; a `Scheduled` one whose process has not started sleeps on `wake` instead —
    /// a queued process parks on it when it is claimed.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::AgendaNotFound`], [`KernelError::AgendaState`] for an item that
    /// is past scheduling or whose process already started, [`KernelError::Unauthorized`]
    /// when `by` is not a person and the item needs one, or a store error.
    pub async fn schedule(&self, id: &str, wake: WakeSet, by: &SubjectRef) -> Result<AgendaItem> {
        let mut item = self.get(id).await?.ok_or_else(|| not_found(id))?;
        let sup = &self.inner.sup;
        if item.state == AgendaState::Proposed {
            if item.origin.needs_a_person() && by.kind != SubjectKind::Human {
                return Err(KernelError::Unauthorized {
                    reason: format!(
                        "agenda item {} came from the consolidation tick, so only a person schedules it (K-I23)",
                        item.id
                    ),
                });
            }
            let approved = item.clone().with_state(AgendaState::Approved);
            if !sup
                .move_item(&approved, &item.state, Some("approved to schedule"), Some(by))
                .await?
            {
                return Err(wrong_state(&item, "scheduled"));
            }
            self.withdraw(id, "scheduled through the agenda").await;
            item = approved;
        }
        match &item.state {
            AgendaState::Approved => {
                let (pid, wake) = self.spawn(&item, Some(wake)).await?;
                let scheduled = item.clone().with_state(AgendaState::Scheduled { wake });
                if !sup
                    .move_item(&scheduled, &item.state, Some(&format!("process {pid}")), Some(by))
                    .await?
                {
                    return Err(wrong_state(&item, "scheduled"));
                }
                Ok(scheduled)
            }
            AgendaState::Scheduled { .. } => {
                let pid =
                    self.process_of(id).await?.ok_or_else(|| wrong_state(&item, "rescheduled"))?;
                let state = sup.runs.get(&pid).await?.map(|run| run.state);
                if !matches!(state, Some(RunState::Waiting | RunState::Queued)) {
                    return Err(wrong_state(&item, "rescheduled (its process already started)"));
                }
                let wake = match item.expires_at {
                    Some(at) => wake.with_expiry(at, crate::wake::ExpiryAction::Cancel),
                    None => wake,
                };
                sup.processes.set_wake(&pid, Some(wake.clone())).await?;
                if state == Some(RunState::Queued) {
                    // Parks at its claim, before its first model call, on the new wake set.
                    sup.processes.set_suspend_requested(&pid, true).await?;
                }
                sup.roots_changed.notify_one();
                let scheduled = item.clone().with_state(AgendaState::Scheduled { wake });
                if !sup.move_item(&scheduled, &item.state, Some("rescheduled"), Some(by)).await? {
                    return Err(wrong_state(&item, "rescheduled"));
                }
                Ok(scheduled)
            }
            _ => Err(wrong_state(&item, "scheduled")),
        }
    }

    /// Withdraws the pending inbox request of item `id`, if any.
    async fn withdraw(&self, id: &str, note: &str) {
        let Some(approvals) = &self.inner.approvals else { return };
        if let Ok(Some(request)) = approvals.get(&request_id(id)).await
            && request.state == ApprovalState::Pending
            && let Err(error) = approvals.cancel(&request.id, Some(note.to_string())).await
        {
            tracing::debug!(approval.id = %request.id, error = %error, "could not withdraw an agenda request");
        }
    }

    /// One scheduler tick of the agenda (spec 3.2 §7.5): applies inbox decisions, expires
    /// items, retries approved items without a process, and finishes items whose process
    /// ended while not running.
    pub(crate) async fn tick(&self) {
        if let Err(error) = self.tick_inner().await {
            tracing::debug!(error = %error, "agenda tick failed");
        }
    }

    async fn tick_inner(&self) -> Result<()> {
        let sup = &self.inner.sup;
        let now = sup.clock.now();
        for item in self.store().list(&AgendaFilter::default().open()).await? {
            match &item.state {
                AgendaState::Proposed => {
                    if item.expires_at.is_some_and(|at| at <= now) {
                        self.expire(item).await?;
                        continue;
                    }
                    self.apply_decision(item).await?;
                }
                AgendaState::Approved => {
                    if item.expires_at.is_some_and(|at| at <= now) {
                        self.expire(item).await?;
                    } else {
                        let by = SubjectRef::service("kernel");
                        self.start(item, &by).await;
                    }
                }
                AgendaState::Scheduled { .. } | AgendaState::Running { .. } => {
                    let Some(pid) = self.process_of(&item.id).await? else { continue };
                    let Some(run) = sup.runs.get(&pid).await? else { continue };
                    if !run.state.is_terminal() {
                        // A process still queued past the item's expiry never starts; the
                        // next tick finds it cancelled and expires the item.
                        if run.state == RunState::Queued
                            && matches!(item.state, AgendaState::Scheduled { .. })
                            && item.expires_at.is_some_and(|at| at <= now)
                        {
                            self.inner.processes.kill(&pid, "its agenda item expired").await?;
                        }
                        continue;
                    }
                    let from = item.state.clone();
                    let never_ran = matches!(from, AgendaState::Scheduled { .. })
                        && run.state == RunState::Cancelled
                        && item.expires_at.is_some_and(|at| at <= now);
                    let next = item.with_state(if never_ran {
                        AgendaState::Expired
                    } else {
                        AgendaState::Done
                    });
                    let reason = format!("process {pid} ended");
                    sup.move_item(&next, &from, Some(&reason), None).await?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    async fn expire(&self, item: AgendaItem) -> Result<()> {
        let from = item.state.clone();
        let id = item.id.clone();
        let expired = item.with_state(AgendaState::Expired);
        if self.inner.sup.move_item(&expired, &from, Some("expired before it ran"), None).await? {
            self.withdraw(&id, "the agenda item expired").await;
        }
        Ok(())
    }

    /// Applies the inbox decision on the proposed `item`, if one was made.
    async fn apply_decision(&self, item: AgendaItem) -> Result<()> {
        let Some(approvals) = &self.inner.approvals else { return Ok(()) };
        let Some(request) = approvals.get(&request_id(&item.id)).await? else { return Ok(()) };
        let last = request.decisions.last();
        let by =
            last.map_or_else(|| SubjectRef::service("kernel:approvals"), |d| d.approver.clone());
        match request.state {
            ApprovalState::Pending => Ok(()),
            ApprovalState::Approved => {
                let mut item = item;
                if let Some(edited) = last
                    .filter(|d| d.kind == ApprovalAction::Edit)
                    .and_then(|d| d.edited_args.as_ref())
                {
                    let from = item.state.clone();
                    let edited_item = edit(item.clone(), edited);
                    if edited_item != item
                        && self
                            .inner
                            .sup
                            .move_item(&edited_item, &from, Some("edited in the inbox"), Some(&by))
                            .await?
                    {
                        item = edited_item;
                    }
                }
                match self.approve_as(item.clone(), &by, "approved in the inbox").await {
                    Ok(_) => Ok(()),
                    Err(KernelError::Unauthorized { reason }) => {
                        tracing::warn!(agenda.item = %item.id, reason = %reason, "an inbox approval by someone other than a person was ignored");
                        Ok(())
                    }
                    Err(KernelError::AgendaState { .. }) => Ok(()),
                    Err(error) => Err(error),
                }
            }
            ApprovalState::Rejected | ApprovalState::Responded => {
                let reason = last
                    .and_then(|d| d.reason.clone())
                    .unwrap_or_else(|| "rejected in the inbox".into());
                match self.reject_as(item, &by, &reason).await {
                    Ok(_) | Err(KernelError::AgendaState { .. }) => Ok(()),
                    Err(error) => Err(error),
                }
            }
            ApprovalState::Expired => self.expire(item).await,
            ApprovalState::Cancelled => {
                let note = request.note.unwrap_or_else(|| "the inbox request was withdrawn".into());
                match self.reject_as(item, &by, &note).await {
                    Ok(_) | Err(KernelError::AgendaState { .. }) => Ok(()),
                    Err(error) => Err(error),
                }
            }
        }
    }
}

/// `item` with the fields an inbox `Edit` may change: `title`, `rationale`, `when` and
/// `expiresAt`; anything else in `edited` is ignored, so an edit cannot change the origin,
/// partition, subject or budget.
fn edit(mut item: AgendaItem, edited: &Value) -> AgendaItem {
    if let Some(title) =
        edited.get("title").and_then(Value::as_str).filter(|t| !t.trim().is_empty())
    {
        item.title = title.to_string();
    }
    if let Some(rationale) = edited.get("rationale").and_then(Value::as_str) {
        item.rationale = rationale.to_string();
    }
    if let Some(when) = edited.get("when") {
        item.when = serde_json::from_value(when.clone()).ok();
    }
    if let Some(expires) = edited.get("expiresAt") {
        item.expires_at = serde_json::from_value(expires.clone()).ok();
    }
    item
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use serde_json::json;

    use super::*;

    const MODES: [PartitionMode; 4] =
        [PartitionMode::ReadOnly, PartitionMode::Plan, PartitionMode::Ask, PartitionMode::Auto];

    #[test]
    fn follow_mode_holds_asks_or_schedules_by_the_partitions_mode() {
        let agent = AgendaOrigin::Agent { pid: "run-1".into(), confidence: 1.0 };
        let routes: Vec<Route> =
            MODES.iter().map(|mode| route(ProactiveApproval::FollowMode, *mode, &agent)).collect();
        assert_eq!(routes, [Route::Hold, Route::Hold, Route::Ask, Route::Schedule]);
        for mode in MODES {
            assert_eq!(route(ProactiveApproval::AlwaysAsk, mode, &agent), Route::Ask);
            assert_eq!(route(ProactiveApproval::Never, mode, &agent), Route::Hold);
        }
    }

    #[test]
    fn a_consolidation_item_never_schedules_without_a_person() {
        let routes: Vec<Route> = MODES
            .iter()
            .map(|mode| route(ProactiveApproval::FollowMode, *mode, &AgendaOrigin::Consolidation))
            .collect();
        assert_eq!(
            routes,
            [Route::Hold, Route::Hold, Route::Ask, Route::Ask],
            "Auto asks too (K-I23)"
        );
    }

    fn origin() -> impl Strategy<Value = AgendaOrigin> {
        prop_oneof![
            Just(AgendaOrigin::User),
            Just(AgendaOrigin::Consolidation),
            (0.0f32..=1.0)
                .prop_map(|confidence| AgendaOrigin::Agent { pid: "run-1".into(), confidence }),
            Just(AgendaOrigin::Observation { id: "obs-1".into() }),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// Only an Auto partition following its mode schedules a proposal without a person
        /// (K-I10), and never one of the consolidation tick's (K-I23).
        #[test]
        fn only_auto_follow_mode_schedules_and_never_consolidation(
            mode in prop::sample::select(MODES.to_vec()),
            approval in prop::sample::select(vec![ProactiveApproval::FollowMode, ProactiveApproval::AlwaysAsk, ProactiveApproval::Never]),
            origin in origin(),
        ) {
            let routed = route(approval, mode, &origin);
            if routed == Route::Schedule {
                prop_assert_eq!(mode, PartitionMode::Auto);
                prop_assert_eq!(approval, ProactiveApproval::FollowMode);
                prop_assert!(!origin.needs_a_person());
            }
        }
    }

    #[test]
    fn an_inbox_edit_changes_only_title_rationale_when_and_expiry() {
        let item = AgendaItem::new(
            "item-1",
            SubjectRef::human("alice"),
            "Old",
            "Because",
            AgendaOrigin::Consolidation,
            Utc::now(),
        );
        let edited = edit(
            item.clone(),
            &json!({
                "title": "New",
                "rationale": "Better reason",
                "when": { "at": "2026-10-24T07:00:00Z" },
                "origin": "user",
                "partition": "home",
                "subject": { "kind": "human", "id": "mallory" },
                "budget": { "maxCostMicroUsd": 1 }
            }),
        );
        assert_eq!((edited.title.as_str(), edited.rationale.as_str()), ("New", "Better reason"));
        assert!(matches!(edited.when, Some(Wake::At(_))));
        assert_eq!(edited.origin, item.origin, "an edit cannot launder the origin (K-I23)");
        assert_eq!(
            edit(item.clone(), &json!({ "title": "  " })).title,
            "Old",
            "a blank title is kept"
        );
        assert_eq!(
            (edited.partition, edited.subject, edited.budget),
            (item.partition, item.subject, item.budget)
        );
    }

    #[test]
    fn the_prompt_names_the_item_and_its_steps() {
        let item = AgendaItem::new(
            "item-9",
            SubjectRef::human("alice"),
            "Re-run the tests",
            "The bump landed",
            AgendaOrigin::User,
            Utc::now(),
        )
        .with_plan(PlanRef::new(
            "sha256:1",
            vec![json!({"id": "1", "title": "Test", "instruction": "cargo test"})],
        ));
        let text = prompt(&item);
        assert!(
            text.contains("item item-9")
                && text.contains("Re-run the tests")
                && text.contains("1. Test: cargo test"),
            "{text}"
        );
        assert_eq!(request_id("item-9"), "agenda_item-9");
    }
}
