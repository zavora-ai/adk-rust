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

use adk_core::RunBudget;
use adk_core::principal::SubjectRef;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::consolidation::ConsolidationTick;
use crate::error::Result;
use crate::partition::PartitionId;
use crate::sched::QuietHours;
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
}

impl ProactivityPolicy {
    /// The default `max_proposed_open`.
    pub const DEFAULT_MAX_PROPOSED_OPEN: usize = 10;

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
}

impl Default for ProactivityPolicy {
    fn default() -> Self {
        Self {
            max_proposed_open: Self::DEFAULT_MAX_PROPOSED_OPEN,
            daily_budget: RunBudget::new(),
            quiet_hours: None,
            approval: ProactiveApproval::FollowMode,
            consolidation: None,
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
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the backend fails.
    async fn put(&self, item: AgendaItem) -> Result<()>;

    /// The item `id`, or `None`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the backend fails.
    async fn get(&self, id: &str) -> Result<Option<AgendaItem>>;

    /// The items `filter` selects, oldest first (by `created_at`, then id), at most
    /// `filter.limit`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the backend fails.
    async fn list(&self, filter: &AgendaFilter) -> Result<Vec<AgendaItem>>;

    /// Writes `item` over the stored item with its id when the stored item's state is of
    /// kind `from`; `false`, writing nothing, when there is no such item or its state is
    /// of another kind.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the backend fails.
    async fn transition(&self, item: AgendaItem, from: AgendaStateKind) -> Result<bool>;

    /// Deletes the item `id`; `false` when there was none.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the backend fails.
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
