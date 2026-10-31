//! The consolidation tick (spec 3.2 §7.5, §13 Q1, Q12).
//!
//! The tick is a `Maintenance` process on the distiller's model that distils the sessions
//! the spec 3.1 distiller never reached and consolidates a partition's `Assumed` facts in
//! two passes. [`ConsolidationTick`] holds its admission gates, its spend caps and the
//! thresholds of both passes; [`ProactivityPolicy::consolidation`](crate::ProactivityPolicy::consolidation)
//! turns it on for a partition. It is on in the CLI's Ask and Auto partitions and off in
//! the library and on servers, so the policy's default is `None`.
//!
//! | Part | Role |
//! |---|---|
//! | [`Consolidator`] | What the harness gives the kernel (spec 3.2 §6 D-1, §10 HK2): a partition's new material since the last tick, and one tick's work on its memory |
//! | [`admit_tick`] | The admission gate: new material **and** `min_interval` **and** `debounce` idle, outside quiet hours, under the daily proactive budget and `budget_share` of it; a bare `schedule` only when set |
//! | `kernel.consolidation` ([`CONSOLIDATION_TARGET`]) | The workflow a tick runs as: a [`ProcessKind::Maintenance`] durable run on the `maintenance` queue with no tools (K-I1); its spend counts toward the partition's daily proactive budget, and its proposals go to the agenda as [`AgendaOrigin::Consolidation`] items, which a person approves in every mode (K-I23) |
//!
//! The kernel evaluates the gate on its scheduler tick, at most once a minute of the kernel's
//! clock per partition, when [`KernelBuilder::consolidator`](crate::KernelBuilder::consolidator)
//! registered a consolidator for the partition and the policy turns the tick on.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_core::principal::SubjectRef;
use adk_core::{NewRun, OnConflict, RunBudget};
use adk_server::background::{WorkflowExecutor, WorkflowRunContext, WorkflowState};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::agenda::{AgendaHandle, AgendaItem, AgendaOrigin};
use crate::bus::SpendDelta;
use crate::error::Result;
use crate::partition::{PartitionId, PartitionMode};
use crate::process::{ProcessKind, ProcessSpec, ProcessTable};
use crate::sched::{QueueClass, QuietHours};
use crate::supervise::Supervision;
use crate::wake::KERNEL_WAKE_TARGET;

/// The consolidation tick of one partition (spec 3.2 §7.5, §13 Q1, Q12).
///
/// | Field | Default | Meaning |
/// |---|---|---|
/// | `min_new_sessions` | 3 | Distilled sessions since the last tick that admit one, or |
/// | `min_new_facts` | 25 | new facts since the last tick that admit one |
/// | `min_interval` | 6 h | Least time between two ticks |
/// | `debounce` | 30 min | Least idle time in the partition before a tick |
/// | `schedule` | `None` | A bare cron schedule, only as an explicit setting |
/// | `budget_share` | 0.10 | Most of the partition's daily proactive budget one day's ticks spend |
/// | `max_tokens_per_tick` | 50 000 | Hard token cap of one tick |
/// | `dedup_score` | 0.92 | Pass 1 collapses `Assumed` near-duplicates at or above this cosine |
/// | `expire_confidence` | 0.5 | Pass 1 expires unused `Assumed` facts under this confidence |
/// | `assumed_ttl` | 90 days | and older than this |
/// | `max_clusters_per_tick` | 20 | Most clusters pass 2 sends to the model in one tick |
/// | `max_loss_fraction` | 0.25 | Most of a scope's live facts one tick may lose, rounded down, and at least one; a merge loses the facts it supersedes less the one it writes |
///
/// A tick is admitted only when the partition has new material **and** `min_interval` has
/// passed **and** the partition has been idle for `debounce`, outside quiet hours and under
/// the daily budget. Durations serialize as whole milliseconds (`minIntervalMs`,
/// `debounceMs`, `assumedTtlMs`); a missing field takes its default.
///
/// # Example
///
/// ```rust
/// use std::time::Duration;
/// use adk_kernel::ConsolidationTick;
///
/// let tick = ConsolidationTick::default();
/// assert_eq!((tick.min_new_sessions, tick.min_new_facts), (3, 25));
/// assert_eq!(tick.min_interval, Duration::from_secs(6 * 60 * 60));
/// assert_eq!(tick.debounce, Duration::from_secs(30 * 60));
/// assert_eq!(tick.assumed_ttl, Duration::from_secs(90 * 24 * 60 * 60));
/// assert!(tick.schedule.is_none());
///
/// let nightly = ConsolidationTick::new().with_schedule("0 3 * * *").with_max_tokens_per_tick(20_000);
/// let json = serde_json::to_value(&nightly).unwrap();
/// assert_eq!(json["schedule"], "0 3 * * *");
/// assert_eq!(json["debounceMs"], 1_800_000);
/// let back: ConsolidationTick = serde_json::from_value(serde_json::json!({ "minNewFacts": 10 })).unwrap();
/// assert_eq!((back.min_new_facts, back.min_new_sessions), (10, 3));
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[non_exhaustive]
pub struct ConsolidationTick {
    /// Distilled sessions since the last tick that admit a tick (3).
    pub min_new_sessions: usize,
    /// New facts since the last tick that admit a tick (25).
    pub min_new_facts: usize,
    /// Least time since the last tick (6 h).
    #[serde(rename = "minIntervalMs", with = "crate::serde_util::millis")]
    pub min_interval: Duration,
    /// Least idle time in the partition before a tick (30 min).
    #[serde(rename = "debounceMs", with = "crate::serde_util::millis")]
    pub debounce: Duration,
    /// A bare cron schedule; only as an explicit setting, never the default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,
    /// The share of the partition's daily proactive budget the ticks may spend (0.10).
    pub budget_share: f32,
    /// The hard token cap of one tick (50 000).
    pub max_tokens_per_tick: u64,
    /// Pass 1 marks the older of two `Assumed` facts whose cosine is at or above this
    /// `Superseded` by the newer one (0.92).
    pub dedup_score: f32,
    /// Pass 1 expires an unused `Assumed` fact whose confidence is under this (0.5).
    pub expire_confidence: f32,
    /// and whose age is over this (90 days).
    #[serde(rename = "assumedTtlMs", with = "crate::serde_util::millis")]
    pub assumed_ttl: Duration,
    /// The most clusters pass 2 sends to the model in one tick (20).
    pub max_clusters_per_tick: usize,
    /// The most of a scope's live facts one tick may lose (0.25), rounded down and at least
    /// one fact, so a small scope can still collapse one duplicate; a merge loses the facts
    /// it supersedes less the one it writes. A result over it is refused whole.
    pub max_loss_fraction: f32,
}

impl ConsolidationTick {
    /// The default `max_tokens_per_tick`; the spec names the cap without a number.
    pub const DEFAULT_MAX_TOKENS_PER_TICK: u64 = 50_000;

    /// The tick with the CLI's defaults (spec 3.2 §7.5).
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the distilled sessions that admit a tick.
    #[must_use]
    pub fn with_min_new_sessions(mut self, sessions: usize) -> Self {
        self.min_new_sessions = sessions;
        self
    }

    /// Sets the new facts that admit a tick.
    #[must_use]
    pub fn with_min_new_facts(mut self, facts: usize) -> Self {
        self.min_new_facts = facts;
        self
    }

    /// Sets the least time between two ticks.
    #[must_use]
    pub fn with_min_interval(mut self, interval: Duration) -> Self {
        self.min_interval = interval;
        self
    }

    /// Sets the least idle time in the partition before a tick.
    #[must_use]
    pub fn with_debounce(mut self, debounce: Duration) -> Self {
        self.debounce = debounce;
        self
    }

    /// Sets a bare cron schedule.
    #[must_use]
    pub fn with_schedule(mut self, schedule: impl Into<String>) -> Self {
        self.schedule = Some(schedule.into());
        self
    }

    /// Sets the share of the partition's daily proactive budget the ticks may spend.
    #[must_use]
    pub fn with_budget_share(mut self, share: f32) -> Self {
        self.budget_share = share;
        self
    }

    /// Sets the hard token cap of one tick.
    #[must_use]
    pub fn with_max_tokens_per_tick(mut self, tokens: u64) -> Self {
        self.max_tokens_per_tick = tokens;
        self
    }

    /// Sets pass 1's near-duplicate cosine threshold.
    #[must_use]
    pub fn with_dedup_score(mut self, score: f32) -> Self {
        self.dedup_score = score;
        self
    }

    /// Sets pass 1's expiry confidence threshold.
    #[must_use]
    pub fn with_expire_confidence(mut self, confidence: f32) -> Self {
        self.expire_confidence = confidence;
        self
    }

    /// Sets the age past which pass 1 expires an unused `Assumed` fact.
    #[must_use]
    pub fn with_assumed_ttl(mut self, ttl: Duration) -> Self {
        self.assumed_ttl = ttl;
        self
    }

    /// Sets the most clusters pass 2 sends to the model in one tick.
    #[must_use]
    pub fn with_max_clusters_per_tick(mut self, clusters: usize) -> Self {
        self.max_clusters_per_tick = clusters;
        self
    }

    /// Sets the most of a scope's live facts one tick may lose.
    #[must_use]
    pub fn with_max_loss_fraction(mut self, fraction: f32) -> Self {
        self.max_loss_fraction = fraction;
        self
    }
}

impl Default for ConsolidationTick {
    fn default() -> Self {
        Self {
            min_new_sessions: 3,
            min_new_facts: 25,
            min_interval: Duration::from_secs(6 * 60 * 60),
            debounce: Duration::from_secs(30 * 60),
            schedule: None,
            budget_share: 0.10,
            max_tokens_per_tick: Self::DEFAULT_MAX_TOKENS_PER_TICK,
            dedup_score: 0.92,
            expire_confidence: 0.5,
            assumed_ttl: Duration::from_secs(90 * 24 * 60 * 60),
            max_clusters_per_tick: 20,
            max_loss_fraction: 0.25,
        }
    }
}

/// The workflow a consolidation tick runs as (spec 3.2 §7.5): a `Maintenance` durable run
/// on the `maintenance` queue.
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_kernel::CONSOLIDATION_TARGET, "kernel.consolidation");
/// ```
pub const CONSOLIDATION_TARGET: &str = "kernel.consolidation";

/// The label a tick's process carries, with the value `consolidation` (spec 3.2 §7.5).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_kernel::CONSOLIDATION_LABEL, "kernel.tick");
/// ```
pub const CONSOLIDATION_LABEL: &str = "kernel.tick";

/// How often, on the kernel's clock, the kernel evaluates a partition's tick gate.
const EVALUATE_EVERY: chrono::Duration = chrono::Duration::seconds(60);

/// What the consolidation ticks of a partition spent today (spec 3.2 §7.5): what
/// `budget_share` is measured against.
///
/// # Example
///
/// ```rust
/// let spend = adk_kernel::TickSpend::new(120, 4_000, 3);
/// assert_eq!(spend.total_tokens, 4_000);
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct TickSpend {
    /// Cost in millionths of a US dollar.
    pub cost_micro_usd: u64,
    /// Tokens.
    pub total_tokens: u64,
    /// Model calls.
    pub model_calls: u64,
}

impl TickSpend {
    /// A spend of `cost_micro_usd`, `total_tokens` and `model_calls`.
    pub fn new(cost_micro_usd: u64, total_tokens: u64, model_calls: u64) -> Self {
        Self { cost_micro_usd, total_tokens, model_calls }
    }
}

/// What a partition holds for a consolidation tick, as its [`Consolidator`] reports it
/// (spec 3.2 §7.5).
///
/// # Example
///
/// ```rust
/// use adk_kernel::{PartitionMode, TickMaterial};
///
/// let material = TickMaterial::new("alice", PartitionMode::Ask).with_new_material(3, 0);
/// assert_eq!((material.new_sessions, material.new_facts), (3, 0));
/// ```
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct TickMaterial {
    /// The user whose memory the tick consolidates; its process runs as this user.
    pub user: String,
    /// The partition's mode: a ReadOnly or Plan partition is never consolidated.
    pub mode: PartitionMode,
    /// When the last tick ran.
    pub last_tick: Option<DateTime<Utc>>,
    /// Sessions distilled since the last tick.
    pub new_sessions: usize,
    /// Facts written since the last tick, the ticks' own aside.
    pub new_facts: usize,
    /// The latest activity of the partition's sessions.
    pub last_activity: Option<DateTime<Utc>>,
    /// Sessions the tick distils when the per-session distiller never reached them.
    pub sessions: Vec<String>,
    /// What the partition's ticks spent today.
    pub spent_today: TickSpend,
}

impl TickMaterial {
    /// No material of `user` in a partition in `mode`.
    pub fn new(user: impl Into<String>, mode: PartitionMode) -> Self {
        Self {
            user: user.into(),
            mode,
            last_tick: None,
            new_sessions: 0,
            new_facts: 0,
            last_activity: None,
            sessions: Vec::new(),
            spent_today: TickSpend::default(),
        }
    }

    /// Sets the last tick's time.
    #[must_use]
    pub fn with_last_tick(mut self, at: DateTime<Utc>) -> Self {
        self.last_tick = Some(at);
        self
    }

    /// Sets the sessions distilled and the facts written since the last tick.
    #[must_use]
    pub fn with_new_material(mut self, sessions: usize, facts: usize) -> Self {
        self.new_sessions = sessions;
        self.new_facts = facts;
        self
    }

    /// Sets the latest activity.
    #[must_use]
    pub fn with_last_activity(mut self, at: DateTime<Utc>) -> Self {
        self.last_activity = Some(at);
        self
    }

    /// Sets the sessions to distil.
    #[must_use]
    pub fn with_sessions(mut self, sessions: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.sessions = sessions.into_iter().map(Into::into).collect();
        self
    }

    /// Sets what the ticks spent today.
    #[must_use]
    pub fn with_spent_today(mut self, spend: TickSpend) -> Self {
        self.spent_today = spend;
        self
    }
}

/// One tick a [`Consolidator`] runs (spec 3.2 §7.5).
///
/// # Example
///
/// ```rust
/// use adk_kernel::{ConsolidationTick, TickRun};
///
/// let run = TickRun::new("pid-1", "default", "alice", ConsolidationTick::default(), chrono::Utc::now());
/// assert!(run.sessions.is_empty());
/// ```
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct TickRun {
    /// The tick's id: its process's pid.
    pub tick_id: String,
    /// The partition.
    pub partition: PartitionId,
    /// The user whose memory it consolidates.
    pub user: String,
    /// The sessions to distil when the per-session distiller never reached them.
    pub sessions: Vec<String>,
    /// The thresholds of the passes and the per-tick token cap.
    pub tick: ConsolidationTick,
    /// The tick's budget: its share of the daily proactive budget, `max_tokens_per_tick`
    /// at most.
    pub budget: RunBudget,
    /// The kernel's clock reading.
    pub at: DateTime<Utc>,
}

impl TickRun {
    /// A tick `tick_id` of `user` in `partition`, with no session to distil and an
    /// unlimited budget.
    pub fn new(
        tick_id: impl Into<String>,
        partition: impl Into<PartitionId>,
        user: impl Into<String>,
        tick: ConsolidationTick,
        at: DateTime<Utc>,
    ) -> Self {
        Self {
            tick_id: tick_id.into(),
            partition: partition.into(),
            user: user.into(),
            sessions: Vec::new(),
            tick,
            budget: RunBudget::new(),
            at,
        }
    }

    /// Sets the sessions to distil.
    #[must_use]
    pub fn with_sessions(mut self, sessions: Vec<String>) -> Self {
        self.sessions = sessions;
        self
    }

    /// Sets the budget.
    #[must_use]
    pub fn with_budget(mut self, budget: RunBudget) -> Self {
        self.budget = budget;
        self
    }
}

/// A follow-up a tick proposes; the kernel files it as an
/// [`AgendaOrigin::Consolidation`] item, which a person approves in every mode (K-I23).
///
/// # Example
///
/// ```rust
/// let proposal = adk_kernel::TickProposal::new("Confirm two assumptions", "They disagree with known facts");
/// assert_eq!(proposal.title, "Confirm two assumptions");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct TickProposal {
    /// A short title.
    pub title: String,
    /// Why it is worth doing.
    pub rationale: String,
}

impl TickProposal {
    /// A proposal titled `title` for `rationale`.
    pub fn new(title: impl Into<String>, rationale: impl Into<String>) -> Self {
        Self { title: title.into(), rationale: rationale.into() }
    }
}

/// What one tick did, in counts, and what it proposes (spec 3.2 §7.5).
///
/// # Example
///
/// ```rust
/// let outcome = adk_kernel::TickOutcome::new("pid-1").with_counts(2, 1, 0, 3);
/// assert_eq!(outcome.superseded, 2);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct TickOutcome {
    /// The tick's id.
    pub tick_id: String,
    /// Facts superseded.
    pub superseded: usize,
    /// Facts forgotten.
    pub forgotten: usize,
    /// Facts merged by pass 2.
    pub merged: usize,
    /// Facts distilled from sessions the distiller never reached.
    pub distilled: usize,
    /// Why the passes applied nothing, when the loss cap refused them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused: Option<String>,
    /// What it spent.
    pub spend: TickSpend,
    /// What it proposes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub proposals: Vec<TickProposal>,
}

impl TickOutcome {
    /// An outcome of tick `tick_id` that changed nothing.
    pub fn new(tick_id: impl Into<String>) -> Self {
        Self { tick_id: tick_id.into(), ..Self::default() }
    }

    /// Sets the facts superseded, forgotten, merged and distilled.
    #[must_use]
    pub fn with_counts(
        mut self,
        superseded: usize,
        forgotten: usize,
        merged: usize,
        distilled: usize,
    ) -> Self {
        self.superseded = superseded;
        self.forgotten = forgotten;
        self.merged = merged;
        self.distilled = distilled;
        self
    }

    /// Sets the loss cap's refusal.
    #[must_use]
    pub fn with_refused(mut self, reason: impl Into<String>) -> Self {
        self.refused = Some(reason.into());
        self
    }

    /// Sets the spend.
    #[must_use]
    pub fn with_spend(mut self, spend: TickSpend) -> Self {
        self.spend = spend;
        self
    }

    /// Adds a proposal.
    #[must_use]
    pub fn with_proposal(mut self, proposal: TickProposal) -> Self {
        self.proposals.push(proposal);
        self
    }
}

/// The memory a consolidation tick works on (spec 3.2 §7.5, §6 D-1): the kernel reads no
/// harness type, so the harness implements this for a partition
/// (`adk_harness::Harness::consolidator`) and registers it with
/// [`KernelBuilder::consolidator`](crate::KernelBuilder::consolidator).
///
/// # Example
///
/// ```rust
/// use adk_kernel::{Consolidator, PartitionId, PartitionMode, TickMaterial, TickOutcome, TickRun};
///
/// struct Nothing;
///
/// #[async_trait::async_trait]
/// impl Consolidator for Nothing {
///     async fn material(&self, _: &PartitionId, _: chrono::DateTime<chrono::Utc>) -> adk_kernel::Result<TickMaterial> {
///         Ok(TickMaterial::new("alice", PartitionMode::Ask))
///     }
///     async fn consolidate(&self, run: TickRun) -> adk_kernel::Result<TickOutcome> {
///         Ok(TickOutcome::new(run.tick_id))
///     }
/// }
/// ```
#[async_trait]
pub trait Consolidator: Send + Sync {
    /// What `partition` holds at `now`: its new material since the last tick, its latest
    /// activity, the sessions to distil and what its ticks spent today.
    async fn material(&self, partition: &PartitionId, now: DateTime<Utc>) -> Result<TickMaterial>;

    /// Runs one tick: distillation of `run.sessions` the per-session distiller never
    /// reached, the two passes, the loss cap and the tick's record, inside `run.budget`.
    async fn consolidate(&self, run: TickRun) -> Result<TickOutcome>;
}

/// What the tick gate reads (spec 3.2 §7.5): a partition's [`TickMaterial`] with the
/// kernel's view of it.
///
/// # Example
///
/// ```rust
/// use adk_kernel::{PartitionMode, TickInputs, TickMaterial};
///
/// let inputs = TickInputs::new(chrono::Utc::now(), TickMaterial::new("alice", PartitionMode::Auto));
/// assert!(!inputs.busy);
/// ```
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct TickInputs {
    /// The kernel's clock reading.
    pub now: DateTime<Utc>,
    /// The partition's material.
    pub material: TickMaterial,
    /// Whether a process of the partition runs now, a tick included.
    pub busy: bool,
    /// Every quiet-hours window that applies.
    pub quiet_hours: Vec<QuietHours>,
    /// The partition's daily proactive budget.
    pub daily_budget: RunBudget,
    /// The partition's proactive spend today, in micro-USD, ticks included.
    pub proactive_spent: u64,
    /// The partition's proactive model calls today, ticks included.
    pub proactive_model_calls: u64,
    /// The partition's proactive tokens today, ticks included.
    pub proactive_tokens: u64,
}

impl TickInputs {
    /// The gate's inputs at `now` for `material`: not busy, no quiet hours, an unlimited
    /// daily budget.
    pub fn new(now: DateTime<Utc>, material: TickMaterial) -> Self {
        Self {
            now,
            material,
            busy: false,
            quiet_hours: Vec::new(),
            daily_budget: RunBudget::new(),
            proactive_spent: 0,
            proactive_model_calls: 0,
            proactive_tokens: 0,
        }
    }

    /// Sets whether a process of the partition runs.
    #[must_use]
    pub fn with_busy(mut self, busy: bool) -> Self {
        self.busy = busy;
        self
    }

    /// Sets the quiet-hours windows.
    #[must_use]
    pub fn with_quiet_hours(mut self, windows: impl IntoIterator<Item = QuietHours>) -> Self {
        self.quiet_hours = windows.into_iter().collect();
        self
    }

    /// Sets the daily proactive budget and what the partition's proactive processes spent
    /// of it today.
    #[must_use]
    pub fn with_daily_budget(mut self, budget: RunBudget, spent_micro_usd: u64) -> Self {
        self.daily_budget = budget;
        self.proactive_spent = spent_micro_usd;
        self
    }

    /// Sets the model calls and tokens the partition's proactive processes used today, which
    /// the daily budget's `max_model_calls` and `max_total_tokens` cap.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::RunBudget;
    /// use adk_kernel::{PartitionMode, TickInputs, TickMaterial};
    ///
    /// let now = chrono::Utc::now();
    /// let inputs = TickInputs::new(now, TickMaterial::new("alice", PartitionMode::Ask))
    ///     .with_daily_budget(RunBudget::new().max_total_tokens(50_000), 0)
    ///     .with_proactive_calls_and_tokens(4, 12_000);
    /// assert_eq!((inputs.proactive_model_calls, inputs.proactive_tokens), (4, 12_000));
    /// ```
    #[must_use]
    pub fn with_proactive_calls_and_tokens(mut self, model_calls: u64, tokens: u64) -> Self {
        self.proactive_model_calls = model_calls;
        self.proactive_tokens = tokens;
        self
    }
}

/// The tick gate's decision (spec 3.2 §7.5).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_kernel::TickAdmission::NoMaterial.reason(), "no_material");
/// ```
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum TickAdmission {
    /// Run a tick within `budget`.
    Admit {
        /// The tick's budget: its share of the day, `max_tokens_per_tick` at most.
        budget: RunBudget,
    },
    /// The partition is ReadOnly or Plan.
    Mode,
    /// Too few distilled sessions and new facts since the last tick.
    NoMaterial,
    /// A bare schedule is set and not due.
    NotDue {
        /// The schedule's next fire time, if it parses.
        next: Option<DateTime<Utc>>,
    },
    /// The last tick is less than `min_interval` ago.
    Interval {
        /// When the interval has passed.
        until: DateTime<Utc>,
    },
    /// A process of the partition runs.
    Busy,
    /// The partition has been active within `debounce`.
    Debounce {
        /// When it has been idle long enough, without more activity.
        until: DateTime<Utc>,
    },
    /// Inside quiet hours.
    QuietHours {
        /// When the windows end.
        until: DateTime<Utc>,
    },
    /// The partition's daily proactive budget is spent.
    Budget,
    /// The ticks' share of the daily proactive budget is spent.
    Share,
}

impl TickAdmission {
    /// The decision's name: `admit`, `mode`, `no_material`, `not_due`, `interval`, `busy`,
    /// `debounce`, `quiet_hours`, `budget` or `share`.
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Admit { .. } => "admit",
            Self::Mode => "mode",
            Self::NoMaterial => "no_material",
            Self::NotDue { .. } => "not_due",
            Self::Interval { .. } => "interval",
            Self::Busy => "busy",
            Self::Debounce { .. } => "debounce",
            Self::QuietHours { .. } => "quiet_hours",
            Self::Budget => "budget",
            Self::Share => "share",
        }
    }
}

fn chrono_of(duration: Duration) -> chrono::Duration {
    chrono::Duration::from_std(duration).unwrap_or(chrono::Duration::MAX)
}

/// `share` of `cap`, rounded down.
fn share_of(cap: u64, share: f32) -> u64 {
    (cap as f64 * f64::from(share.clamp(0.0, 1.0))).floor() as u64
}

/// The bare schedule's next fire time after `after`, in UTC.
fn next_fire(expr: &str, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
    adk_core::schedule::ScheduleSpec::Cron { expr: expr.to_string(), timezone: "UTC".into() }
        .next_after(after)
        .ok()
        .flatten()
}

/// Whether `tick` admits a consolidation tick on `inputs` (spec 3.2 §7.5, §13 Q1), and with
/// what budget.
///
/// | Gate | Holds when |
/// |---|---|
/// | Mode | the partition is Ask or Auto |
/// | Quiet hours | `now` is outside every window (K-I11) |
/// | Daily budget | the partition's proactive spend today is under the daily cost cap |
/// | Share | the ticks' spend today is under `budget_share` of each daily cap |
/// | New material | at least `min_new_sessions` distilled sessions or `min_new_facts` new facts since the last tick |
/// | Interval | `min_interval` has passed since the last tick |
/// | Debounce | no process of the partition runs and its latest activity is `debounce` ago |
///
/// With a bare `schedule` set, the schedule's fire time since the last tick replaces new
/// material, interval and debounce; quiet hours and the budgets still hold. The admitted
/// budget is `max_tokens_per_tick` tokens at most, and the share of each daily cap the
/// ticks have left, within what the day has left.
///
/// # Example
///
/// ```rust
/// use std::time::Duration;
/// use adk_kernel::{ConsolidationTick, PartitionMode, TickAdmission, TickInputs, TickMaterial, admit_tick};
///
/// let now = chrono::Utc::now();
/// let idle = now - chrono::Duration::hours(1);
/// let material = TickMaterial::new("alice", PartitionMode::Ask).with_new_material(3, 0).with_last_activity(idle);
/// let decision = admit_tick(&ConsolidationTick::default(), &TickInputs::new(now, material.clone()));
/// assert!(matches!(decision, TickAdmission::Admit { .. }));
/// let busy = admit_tick(&ConsolidationTick::default(), &TickInputs::new(now, material).with_busy(true));
/// assert_eq!(busy, TickAdmission::Busy);
/// ```
pub fn admit_tick(tick: &ConsolidationTick, inputs: &TickInputs) -> TickAdmission {
    let now = inputs.now;
    let material = &inputs.material;
    if matches!(material.mode, PartitionMode::ReadOnly | PartitionMode::Plan) {
        return TickAdmission::Mode;
    }
    let quiet: Vec<&QuietHours> =
        inputs.quiet_hours.iter().filter(|window| window.contains(now)).collect();
    if !quiet.is_empty() {
        // The windows can overlap: release once none holds.
        let mut until = now;
        for _ in 0..=inputs.quiet_hours.len() {
            let next = inputs
                .quiet_hours
                .iter()
                .map(|window| window.release_at(until))
                .max()
                .unwrap_or(until);
            if next == until {
                break;
            }
            until = next;
        }
        return TickAdmission::QuietHours { until };
    }
    let daily = &inputs.daily_budget;
    let spent = material.spent_today;
    if daily.max_cost_micro_usd.is_some_and(|cap| inputs.proactive_spent >= cap)
        || daily.max_model_calls.is_some_and(|cap| inputs.proactive_model_calls >= cap)
        || daily.max_total_tokens.is_some_and(|cap| inputs.proactive_tokens >= cap)
    {
        return TickAdmission::Budget;
    }
    let remaining = |cap: Option<u64>, used: u64| {
        cap.map(|cap| share_of(cap, tick.budget_share).saturating_sub(used))
    };
    let cost = remaining(daily.max_cost_micro_usd, spent.cost_micro_usd);
    let tokens = remaining(daily.max_total_tokens, spent.total_tokens);
    let calls = remaining(daily.max_model_calls, spent.model_calls);
    if [cost, tokens, calls].contains(&Some(0)) {
        return TickAdmission::Share;
    }
    match &tick.schedule {
        Some(expr) => {
            let anchor = material.last_tick.unwrap_or(now - chrono::Duration::days(1));
            let next = next_fire(expr, anchor);
            if next.is_none_or(|next| next > now) {
                return TickAdmission::NotDue { next };
            }
        }
        None => {
            if material.new_sessions < tick.min_new_sessions
                && material.new_facts < tick.min_new_facts
            {
                return TickAdmission::NoMaterial;
            }
            if let Some(last) = material.last_tick {
                let until = last + chrono_of(tick.min_interval);
                if now < until {
                    return TickAdmission::Interval { until };
                }
            }
            if inputs.busy {
                return TickAdmission::Busy;
            }
            if let Some(active) = material.last_activity {
                let until = active + chrono_of(tick.debounce);
                if now < until {
                    return TickAdmission::Debounce { until };
                }
            }
        }
    }
    // A tick spends at most its share and what the day leaves of the proactive budget.
    let day_left = |cap: Option<u64>, used: u64| cap.map(|cap| cap.saturating_sub(used));
    let tokens_left = day_left(daily.max_total_tokens, inputs.proactive_tokens);
    let mut budget = RunBudget::new().max_total_tokens(
        [tokens, tokens_left].into_iter().flatten().fold(tick.max_tokens_per_tick, u64::min),
    );
    if let Some(cost) = cost {
        let day_left =
            daily.max_cost_micro_usd.map_or(cost, |cap| cap.saturating_sub(inputs.proactive_spent));
        budget = budget.max_cost_micro_usd(cost.min(day_left));
    }
    if let Some(calls) = calls {
        let day_left = day_left(daily.max_model_calls, inputs.proactive_model_calls);
        budget = budget.max_model_calls(day_left.map_or(calls, |left| calls.min(left)));
    }
    if daily.allow_unpriced_models {
        budget = budget.allow_unpriced_models();
    }
    TickAdmission::Admit { budget }
}

/// The input of a `kernel.consolidation` run.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TickInput {
    partition: PartitionId,
    user: String,
    mode: PartitionMode,
    sessions: Vec<String>,
    tick: ConsolidationTick,
    #[serde(with = "crate::serde_util::run_budget")]
    budget: RunBudget,
}

/// Runs `kernel.consolidation`: one tick through the partition's [`Consolidator`], its
/// spend added to the process's daily total, its proposals filed with the agenda.
/// Workflow ids it does not run go to `user`, the caller's workflows.
pub(crate) struct ConsolidationExecutor {
    sup: Arc<Supervision>,
    consolidators: Arc<BTreeMap<PartitionId, Arc<dyn Consolidator>>>,
    agenda: AgendaHandle,
    user: Option<Arc<dyn WorkflowExecutor>>,
}

impl fmt::Debug for ConsolidationExecutor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConsolidationExecutor")
            .field("partitions", &self.consolidators.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl ConsolidationExecutor {
    pub(crate) fn new(
        sup: Arc<Supervision>,
        consolidators: Arc<BTreeMap<PartitionId, Arc<dyn Consolidator>>>,
        agenda: AgendaHandle,
        user: Option<Arc<dyn WorkflowExecutor>>,
    ) -> Self {
        Self { sup, consolidators, agenda, user }
    }

    async fn run_tick(
        &self,
        run_id: &str,
        input: WorkflowState,
    ) -> std::result::Result<Value, String> {
        let input: TickInput = serde_json::from_value(Value::Object(input.into_iter().collect()))
            .map_err(|error| {
            format!(
                "a `{CONSOLIDATION_TARGET}` run's input is not a tick the kernel spawned: {error}"
            )
        })?;
        let now = self.sup.clock.now();
        // Spawned just before a window opened: no proactive process starts inside it (K-I11).
        if self.sup.quiet_release(&input.partition, now).await > now {
            return Ok(json!({ "tickId": run_id, "skipped": "quiet_hours" }));
        }
        let consolidator = self.consolidators.get(&input.partition).ok_or_else(|| {
            format!("no consolidator is registered for partition {}", input.partition)
        })?;
        let run = TickRun::new(run_id, input.partition.clone(), &input.user, input.tick, now)
            .with_sessions(input.sessions)
            .with_budget(input.budget);
        let outcome = consolidator.consolidate(run).await.map_err(|error| error.to_string())?;
        // The tick's spend is proactive spend of the partition (spec 3.2 §7.5, §7.9).
        let spend = SpendDelta::new(outcome.spend.cost_micro_usd)
            .with_model_calls(outcome.spend.model_calls)
            .with_total_tokens(outcome.spend.total_tokens);
        if !spend.is_empty()
            && let Err(error) = self
                .sup
                .processes
                .add_spend(run_id, &input.partition, now.date_naive(), spend)
                .await
        {
            tracing::warn!(process.pid = run_id, error = %error, "could not record a consolidation tick's spend");
        }
        let mut proposed = Vec::new();
        if let Some(agenda) = self.agenda.get() {
            for proposal in &outcome.proposals {
                let item = AgendaItem::new(
                    format!("consolidation-{}", uuid::Uuid::new_v4()),
                    SubjectRef::human(input.user.clone()),
                    proposal.title.clone(),
                    proposal.rationale.clone(),
                    AgendaOrigin::Consolidation,
                    now,
                )
                .with_partition(input.partition.clone());
                let id = item.id.clone();
                match agenda.propose(item, input.mode).await {
                    Ok(_) => proposed.push(id),
                    Err(error) => {
                        tracing::warn!(error = %error, "could not file a consolidation proposal")
                    }
                }
            }
        }
        tracing::info!(
            process.pid = run_id,
            memory.superseded = outcome.superseded,
            memory.forgotten = outcome.forgotten,
            memory.merged = outcome.merged,
            memory.distilled = outcome.distilled,
            agenda.proposed = proposed.len(),
            "consolidation tick finished"
        );
        Ok(json!({ "outcome": outcome, "proposed": proposed }))
    }
}

#[async_trait]
impl WorkflowExecutor for ConsolidationExecutor {
    fn has_workflow(&self, workflow_id: &str) -> bool {
        workflow_id == CONSOLIDATION_TARGET
            || self.user.as_ref().is_some_and(|user| user.has_workflow(workflow_id))
    }

    async fn execute(
        &self,
        workflow_id: &str,
        input: WorkflowState,
        cancel_token: CancellationToken,
    ) -> std::result::Result<Value, String> {
        match &self.user {
            Some(user) if workflow_id != CONSOLIDATION_TARGET => {
                user.execute(workflow_id, input, cancel_token).await
            }
            _ => Err(format!("`{CONSOLIDATION_TARGET}` runs only as a durable run")),
        }
    }

    async fn execute_durable(
        &self,
        workflow_id: &str,
        input: WorkflowState,
        run: WorkflowRunContext,
    ) -> std::result::Result<Value, String> {
        if workflow_id != CONSOLIDATION_TARGET {
            return match &self.user {
                Some(user) => user.execute_durable(workflow_id, input, run).await,
                None => Err(format!("no workflow `{workflow_id}`")),
            };
        }
        tokio::select! {
            result = self.run_tick(&run.run_id, input) => result,
            () = run.cancel.cancelled() => Err("the consolidation tick was cancelled".to_string()),
        }
    }
}

/// Evaluates each registered partition's tick gate on the kernel's scheduler tick and spawns
/// an admitted tick (spec 3.2 §7.5).
#[derive(Clone)]
pub(crate) struct ConsolidationTicker {
    sup: Arc<Supervision>,
    processes: ProcessTable,
    consolidators: Arc<BTreeMap<PartitionId, Arc<dyn Consolidator>>>,
    evaluated: Arc<Mutex<BTreeMap<PartitionId, DateTime<Utc>>>>,
}

impl ConsolidationTicker {
    pub(crate) fn new(
        sup: Arc<Supervision>,
        processes: ProcessTable,
        consolidators: Arc<BTreeMap<PartitionId, Arc<dyn Consolidator>>>,
    ) -> Self {
        Self { sup, processes, consolidators, evaluated: Arc::default() }
    }

    /// Whether a process of `partition` runs, a tick included; the kernel's own wake runs
    /// aside.
    async fn busy(&self, partition: &PartitionId) -> bool {
        for run in self.sup.running().await {
            if run.target == KERNEL_WAKE_TARGET {
                continue;
            }
            let meta =
                self.sup.stored_meta(&run).await.unwrap_or_else(|| Supervision::infer_meta(&run));
            if meta.partition == *partition {
                return true;
            }
        }
        false
    }

    /// One evaluation of every partition whose last one is a minute old on the kernel's clock.
    pub(crate) async fn tick(&self) {
        let Some(tick) = self.sup.proactivity.consolidation.clone() else {
            return;
        };
        if self.sup.governance.is_frozen() {
            return;
        }
        let now = self.sup.clock.now();
        for (partition, consolidator) in self.consolidators.iter() {
            {
                let mut evaluated = self.evaluated.lock().unwrap_or_else(|e| e.into_inner());
                if evaluated.get(partition).is_some_and(|at| now - *at < EVALUATE_EVERY) {
                    continue;
                }
                evaluated.insert(partition.clone(), now);
            }
            if let Err(error) = self.evaluate(&tick, partition, consolidator.as_ref(), now).await {
                tracing::debug!(partition = %partition, error = %error, "consolidation tick not spawned");
            }
        }
    }

    async fn evaluate(
        &self,
        tick: &ConsolidationTick,
        partition: &PartitionId,
        consolidator: &dyn Consolidator,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let mut material = consolidator.material(partition, now).await?;
        // A mode tightened while the kernel runs holds the tick back (spec 3.3 §10 K3).
        material.mode = self.sup.area_mode(partition, material.mode).await;
        let daily = self.sup.proactivity.daily_budget.clone();
        let used = if daily.max_cost_micro_usd.is_some()
            || daily.max_model_calls.is_some()
            || daily.max_total_tokens.is_some()
        {
            self.sup.proactive_usage(partition).await
        } else {
            crate::stores::UsageTotals::default()
        };
        let quiet = self.sup.quiet_windows(partition).await;
        let inputs = TickInputs::new(now, material.clone())
            .with_busy(self.busy(partition).await)
            .with_quiet_hours(quiet)
            .with_daily_budget(daily, used.micro_usd)
            .with_proactive_calls_and_tokens(used.model_calls, used.total_tokens);
        let budget = match admit_tick(tick, &inputs) {
            TickAdmission::Admit { budget } => budget,
            held => {
                tracing::debug!(partition = %partition, admission = held.reason(), "consolidation tick held");
                return Ok(());
            }
        };
        let input = TickInput {
            partition: partition.clone(),
            user: material.user.clone(),
            mode: material.mode,
            sessions: material.sessions.clone(),
            tick: tick.clone(),
            budget,
        };
        // One tick per window since the last one, across replicas; a failed tick is not
        // retried in place, since its record would be overwritten, but by the next window.
        let window = now.timestamp() / EVALUATE_EVERY.num_seconds().max(1);
        let last = material.last_tick.map_or(0, |at| at.timestamp_millis());
        let run = NewRun::new(
            QueueClass::MAINTENANCE,
            &self.sup.app_name,
            &material.user,
            CONSOLIDATION_TARGET,
            serde_json::to_value(&input)?,
        )
        .with_idempotency_key(format!("consolidation:{partition}:{last}:{window}"))
        .with_concurrency_key(format!("consolidation:{partition}"), OnConflict::Reject)
        .with_max_attempts(1)
        .with_service_subject();
        let spec = ProcessSpec::new(ProcessKind::Maintenance)
            .with_partition(partition.clone())
            .with_label(CONSOLIDATION_LABEL, "consolidation");
        let process = self.processes.spawn(run, spec).await?;
        self.sup.nudge();
        tracing::info!(partition = %partition, process.pid = %process.meta.pid, "consolidation tick spawned");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn noon() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 23, 12, 0, 0).unwrap()
    }

    fn minutes(n: i64) -> chrono::Duration {
        chrono::Duration::minutes(n)
    }

    /// Material that admits a tick at noon: three distilled sessions, the last tick seven
    /// hours ago, the partition idle for an hour.
    fn ready() -> TickMaterial {
        TickMaterial::new("alice", PartitionMode::Ask)
            .with_new_material(3, 0)
            .with_last_tick(noon() - minutes(7 * 60))
            .with_last_activity(noon() - minutes(60))
    }

    fn decide(material: TickMaterial) -> TickAdmission {
        admit_tick(&ConsolidationTick::default(), &TickInputs::new(noon(), material))
    }

    fn admitted(decision: &TickAdmission) -> bool {
        matches!(decision, TickAdmission::Admit { .. })
    }

    #[test]
    fn the_admission_table() {
        let tick = ConsolidationTick::default();
        let six_hours = minutes(6 * 60);
        let cases: Vec<(&str, TickMaterial, TickAdmission)> = vec![
            (
                "new sessions at the threshold",
                ready(),
                TickAdmission::Admit {
                    budget: RunBudget::new().max_total_tokens(tick.max_tokens_per_tick),
                },
            ),
            (
                "one session short, no facts",
                ready().with_new_material(2, 24),
                TickAdmission::NoMaterial,
            ),
            (
                "facts at the threshold",
                ready().with_new_material(0, 25),
                TickAdmission::Admit {
                    budget: RunBudget::new().max_total_tokens(tick.max_tokens_per_tick),
                },
            ),
            (
                "interval one minute short",
                ready().with_last_tick(noon() - six_hours + minutes(1)),
                TickAdmission::Interval { until: noon() + minutes(1) },
            ),
            (
                "interval exactly passed",
                ready().with_last_tick(noon() - six_hours),
                TickAdmission::Admit {
                    budget: RunBudget::new().max_total_tokens(tick.max_tokens_per_tick),
                },
            ),
            (
                "active 29 minutes ago",
                ready().with_last_activity(noon() - minutes(29)),
                TickAdmission::Debounce { until: noon() + minutes(1) },
            ),
            (
                "idle exactly 30 minutes",
                ready().with_last_activity(noon() - minutes(30)),
                TickAdmission::Admit {
                    budget: RunBudget::new().max_total_tokens(tick.max_tokens_per_tick),
                },
            ),
            (
                "first tick ever",
                TickMaterial::new("alice", PartitionMode::Auto).with_new_material(3, 0),
                TickAdmission::Admit {
                    budget: RunBudget::new().max_total_tokens(tick.max_tokens_per_tick),
                },
            ),
            (
                "read-only partition",
                TickMaterial { mode: PartitionMode::ReadOnly, ..ready() },
                TickAdmission::Mode,
            ),
            (
                "plan partition",
                TickMaterial { mode: PartitionMode::Plan, ..ready() },
                TickAdmission::Mode,
            ),
        ];
        for (name, material, expected) in cases {
            assert_eq!(decide(material), expected, "{name}");
        }
        let busy = admit_tick(&tick, &TickInputs::new(noon(), ready()).with_busy(true));
        assert_eq!(busy, TickAdmission::Busy, "a running process is activity");
    }

    #[test]
    fn quiet_hours_hold_a_ready_tick_until_they_end() {
        let quiet: QuietHours = "11:00-13:00".parse().unwrap();
        let quiet = quiet.with_utc_offset_minutes(0);
        let inputs = TickInputs::new(noon(), ready()).with_quiet_hours([quiet]);
        assert_eq!(
            admit_tick(&ConsolidationTick::default(), &inputs),
            TickAdmission::QuietHours { until: noon() + minutes(60) }
        );
        let after = TickInputs::new(noon() + minutes(60), ready()).with_quiet_hours([quiet]);
        assert!(admitted(&admit_tick(&ConsolidationTick::default(), &after)));
    }

    #[test]
    fn the_budget_share_bounds_the_tick() {
        let tick = ConsolidationTick::default().with_max_tokens_per_tick(50_000);
        let daily = RunBudget::new().max_cost_micro_usd(1_000_000).max_total_tokens(200_000);
        // A tenth of the day: 100,000 micro-USD and 20,000 tokens for the ticks.
        let spent = TickSpend::new(30_000, 5_000, 2);
        let inputs = TickInputs::new(noon(), ready().with_spent_today(spent))
            .with_daily_budget(daily.clone(), 400_000);
        assert_eq!(
            admit_tick(&tick, &inputs),
            TickAdmission::Admit {
                budget: RunBudget::new().max_total_tokens(15_000).max_cost_micro_usd(70_000)
            }
        );
        // The day's proactive budget almost spent: the tick gets what is left of it.
        let tight = TickInputs::new(noon(), ready()).with_daily_budget(daily.clone(), 960_000);
        assert_eq!(
            admit_tick(&tick, &tight),
            TickAdmission::Admit {
                budget: RunBudget::new().max_total_tokens(20_000).max_cost_micro_usd(40_000)
            }
        );
        let spent_day =
            TickInputs::new(noon(), ready()).with_daily_budget(daily.clone(), 1_000_000);
        assert_eq!(admit_tick(&tick, &spent_day), TickAdmission::Budget);
        let share_spent =
            TickInputs::new(noon(), ready().with_spent_today(TickSpend::new(100_000, 0, 0)))
                .with_daily_budget(daily, 100_000);
        assert_eq!(admit_tick(&tick, &share_spent), TickAdmission::Share);
        // Without a daily token cap, the per-tick cap holds alone.
        let uncapped = TickInputs::new(noon(), ready());
        assert_eq!(
            admit_tick(&tick.with_max_tokens_per_tick(9_000), &uncapped),
            TickAdmission::Admit { budget: RunBudget::new().max_total_tokens(9_000) }
        );
    }

    #[test]
    fn a_bare_schedule_replaces_material_interval_and_debounce() {
        let nightly = ConsolidationTick::default().with_schedule("0 3 * * *");
        let quiet = TickMaterial::new("alice", PartitionMode::Ask)
            .with_last_tick(Utc.with_ymd_and_hms(2026, 10, 22, 3, 0, 5).unwrap())
            .with_last_activity(noon() - minutes(1));
        // Yesterday's tick: today's 03:00 fire has passed, so the tick is due, recent
        // activity and no material notwithstanding.
        assert_eq!(
            admit_tick(&nightly, &TickInputs::new(noon(), quiet.clone())),
            TickAdmission::Admit {
                budget: RunBudget::new().max_total_tokens(nightly.max_tokens_per_tick)
            },
            "today's 03:00 passed since yesterday's tick"
        );
        let ticked_today =
            quiet.with_last_tick(Utc.with_ymd_and_hms(2026, 10, 23, 3, 0, 5).unwrap());
        assert_eq!(
            admit_tick(&nightly, &TickInputs::new(noon(), ticked_today)),
            TickAdmission::NotDue {
                next: Some(Utc.with_ymd_and_hms(2026, 10, 24, 3, 0, 0).unwrap())
            }
        );
        let broken = ConsolidationTick::default().with_schedule("not cron");
        assert_eq!(
            admit_tick(&broken, &TickInputs::new(noon(), ready())),
            TickAdmission::NotDue { next: None }
        );
    }
}
