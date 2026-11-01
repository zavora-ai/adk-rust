//! The site's charter, its KPIs, its jobs' earned autonomy and the signals the public
//! partition sends the back office (spec 3.5 §7.6, §7.10, §7.11, §7.21; feature `charter`).
//!
//! The charter format is AWP 2.0's (`awp_types::Charter`); this module keeps it, measures
//! against it and enforces what it says:
//!
//! | Item | What it does | Spec |
//! |---|---|---|
//! | [`CharterStore`], [`CharterVersion`], [`CommitDecision`], [`RoleApproval`], [`check_chain`], [`commit_audited`] | Every charter version with the decision that committed it; a commit refuses a draft that fails `Charter::validate()` and a decision whose approval chain distinct people did not satisfy | §7.6, §7.11, A-I23 |
//! | [`KpiStore`], [`KpiSample`], [`KpiAggregate`], [`KpiPeriod`], [`KpiState`], [`GuardrailState`] | Measurements per KPI with daily and weekly aggregates, and the tracker's state | §7.11 |
//! | [`KpiSource`], [`KpiTracker`], [`KpiTrackerExecutor`], [`JobControl`], [`JobPauses`] | The KPI tracker: reads each KPI's source on its cadence, keeps the measure-only period, checks guardrails and pauses a breaching job before its next action | §7.11, A-I14, A-I15 |
//! | [`AutonomyStore`], [`AutonomyLadder`], [`JobAutonomy`], [`AutonomyChange`], [`JobDecision`], [`OwnerDecision`], [`PromotionPolicy`] | Each job's autonomy level and history: promotion only by an owner's decision, demotion one level on a breach, two rejections in a row or an incident | §7.11, A-I13 |
//! | [`SignalStore`], [`SignalSink`], [`SignalRecord`], [`SignalSourceTool`] | Typed signals filed in the back office's agenda as [`AgendaOrigin::Signal`](crate::AgendaOrigin::Signal) items, counted by kind and topic, rate-limited per source, waking [`Wake::Signal`](crate::Wake::Signal) sleepers; never a crossing | §7.10, A-I3 |
//! | [`SiteProcessKind`] | The site's process kinds, `site.probe` to `site.digest` | §7.11, §7.19 |
//! | [`CharterEvent`], [`CharterEventSink`] | Commits, baselines, breaches, pauses and autonomy changes, each with its urgency | §7.21 |
//!
//! Every store has in-memory, SQLite, PostgreSQL and Redis backends that pass one
//! conformance suite each (`conformance::charter`, feature `conformance`):
//!
//! | Store | In-memory | SQLite (`sqlite`) | PostgreSQL (`postgres`) | Redis (`redis`) |
//! |---|---|---|---|---|
//! | [`CharterStore`] | [`InMemoryCharterStore`] | `SqliteCharterStore` | `PostgresCharterStore` | `RedisCharterStore` |
//! | [`KpiStore`] | [`InMemoryKpiStore`] | `SqliteKpiStore` | `PostgresKpiStore` | `RedisKpiStore` |
//! | [`AutonomyStore`] | [`InMemoryAutonomyStore`] | `SqliteAutonomyStore` | `PostgresAutonomyStore` | `RedisAutonomyStore` |
//! | [`SignalStore`] | [`InMemorySignalStore`] | `SqliteSignalStore` | `PostgresSignalStore` | `RedisSignalStore` |
//!
//! # Example
//!
//! ```rust
//! use adk_kernel::charter::{SiteProcessKind, SITE_PROCESS_LABEL};
//!
//! assert_eq!(SiteProcessKind::Probe.as_str(), "site.probe");
//! assert_eq!(SITE_PROCESS_LABEL, "site.process");
//! ```

use adk_core::audit_log::{AuditActor, AuditRecordDraft, AuditRecordKind, AuditTrail};
use async_trait::async_trait;
use awp_types::{AutonomyLevel, Urgency};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

mod autonomy;
mod kpi;
mod signal;
mod site;
mod store;
mod tracker;

pub(crate) use autonomy::check_apply as check_autonomy_apply;
pub use autonomy::{
    AutonomyChange, AutonomyLadder, AutonomyReason, AutonomyStore, DecisionOutcome, JobAutonomy,
    JobDecision, OwnerDecision, PromotionEligibility, PromotionPolicy, ladder_rank,
};
pub use kpi::{
    GuardrailState, KpiAggregate, KpiPeriod, KpiSample, KpiState, KpiStore, aggregate, period_start,
};
pub use signal::{
    SIGNAL_SOURCE_TOOL, SignalFilter, SignalKey, SignalOutcome, SignalRateLimit, SignalRecord,
    SignalSink, SignalSourceTool, SignalStore, signal_key, signal_topic,
};
pub use site::{SITE_PROCESS_LABEL, SiteProcessKind};
pub use store::{
    CHARTER_ACTION, ChainRefusal, CharterStore, CharterVersion, CommitDecision, RoleApproval,
    check_chain, commit_audited, is_agent_principal,
};
pub use tracker::{
    JobControl, JobPauses, KPI_TRACKER_TARGET, KpiPhase, KpiReview, KpiSource, KpiStatus,
    KpiTracker, KpiTrackerExecutor, TrackerReport,
};

pub use crate::stores::charter_memory::{
    InMemoryAutonomyStore, InMemoryCharterStore, InMemoryKpiStore, InMemorySignalStore,
};
#[cfg(feature = "redis")]
pub use crate::stores::charter_redis::{
    RedisAutonomyStore, RedisCharterStore, RedisKpiStore, RedisSignalStore,
};
#[cfg(feature = "postgres")]
pub use crate::stores::charter_sql::{
    PostgresAutonomyStore, PostgresCharterStore, PostgresKpiStore, PostgresSignalStore,
};
#[cfg(feature = "sqlite")]
pub use crate::stores::charter_sql::{
    SqliteAutonomyStore, SqliteCharterStore, SqliteKpiStore, SqliteSignalStore,
};
/// The four charter stores of one backend (spec 3.5 §7.6, §7.10, §7.11): the `charter`
/// field of [`KernelStores`](crate::stores::KernelStores), so a site's charter, KPIs,
/// autonomy and signals sit beside the kernel's other stores.
///
/// # Example
///
/// ```rust
/// use adk_kernel::charter::CharterStores;
///
/// let stores = CharterStores::in_memory();
/// let _ = (&stores.charters, &stores.kpis, &stores.autonomy, &stores.signals);
/// ```
#[derive(Clone)]
#[non_exhaustive]
pub struct CharterStores {
    /// Charter versions with the decisions that committed them.
    pub charters: std::sync::Arc<dyn CharterStore>,
    /// KPI readings, aggregates and the tracker's state.
    pub kpis: std::sync::Arc<dyn KpiStore>,
    /// Each job's autonomy level and history.
    pub autonomy: std::sync::Arc<dyn AutonomyStore>,
    /// Signals filed in the back office.
    pub signals: std::sync::Arc<dyn SignalStore>,
}

impl std::fmt::Debug for CharterStores {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CharterStores").finish_non_exhaustive()
    }
}

impl CharterStores {
    /// The four stores in memory.
    pub fn in_memory() -> Self {
        Self {
            charters: std::sync::Arc::new(InMemoryCharterStore::new()),
            kpis: std::sync::Arc::new(InMemoryKpiStore::new()),
            autonomy: std::sync::Arc::new(InMemoryAutonomyStore::new()),
            signals: std::sync::Arc::new(InMemorySignalStore::new()),
        }
    }

    /// The four stores on `pool`, migrated.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the migration fails.
    #[cfg(feature = "sqlite")]
    pub async fn sqlite(pool: sqlx::SqlitePool) -> crate::Result<Self> {
        let charters = SqliteCharterStore::new(pool.clone());
        charters.migrate().await?;
        Ok(Self {
            charters: std::sync::Arc::new(charters),
            kpis: std::sync::Arc::new(SqliteKpiStore::new(pool.clone())),
            autonomy: std::sync::Arc::new(SqliteAutonomyStore::new(pool.clone())),
            signals: std::sync::Arc::new(SqliteSignalStore::new(pool)),
        })
    }

    /// The four stores on `pool`, migrated.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the migration fails.
    #[cfg(feature = "postgres")]
    pub async fn postgres(pool: sqlx::PgPool) -> crate::Result<Self> {
        let charters = PostgresCharterStore::new(pool.clone());
        charters.migrate().await?;
        Ok(Self {
            charters: std::sync::Arc::new(charters),
            kpis: std::sync::Arc::new(PostgresKpiStore::new(pool.clone())),
            autonomy: std::sync::Arc::new(PostgresAutonomyStore::new(pool.clone())),
            signals: std::sync::Arc::new(PostgresSignalStore::new(pool)),
        })
    }

    /// The four stores on `client`, an initialised Redis client, migrated.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the migration fails.
    #[cfg(feature = "redis")]
    pub async fn redis(client: fred::prelude::Client) -> crate::Result<Self> {
        let charters = RedisCharterStore::from_client(client.clone());
        charters.migrate().await?;
        Ok(Self {
            charters: std::sync::Arc::new(charters),
            kpis: std::sync::Arc::new(RedisKpiStore::from_client(client.clone())),
            autonomy: std::sync::Arc::new(RedisAutonomyStore::from_client(client.clone())),
            signals: std::sync::Arc::new(RedisSignalStore::from_client(client)),
        })
    }
}

/// How far back a store keeps signal rate windows: a day, in microseconds.
pub(crate) const RATE_HORIZON_MICROS: i64 = 24 * 60 * 60 * 1_000_000;

/// The kind of the charter's `website.*` audit records, as
/// `AuditRecordKind::Other(WEBSITE_AUDIT_KIND.into())` (spec 3.5 §9.3); `kernel.signal` is
/// a kernel record of kind [`KERNEL_AUDIT_KIND`](crate::audit::KERNEL_AUDIT_KIND).
pub const WEBSITE_AUDIT_KIND: &str = "website";

/// Audit actions of the charter module (spec 3.5 §9.3).
///
/// | Action | Fields |
/// |---|---|
/// | [`SIGNAL`](audit::SIGNAL) | `partition`, `kind`, `topic`, `item`, `count`, `source`, `outcome` — no field text |
/// | [`CHARTER_COMMITTED`](audit::CHARTER_COMMITTED) | `site`, `charterVersion`, `decision`, `approvers`, `signedNote` |
/// | [`APPROVAL_CHAIN_SATISFIED`](audit::APPROVAL_CHAIN_SATISFIED), [`APPROVAL_CHAIN_REFUSED`](audit::APPROVAL_CHAIN_REFUSED) | `site`, `action`, `decision`, `approvers`, `reason` |
/// | [`JOB_PROMOTED`](audit::JOB_PROMOTED), [`JOB_DEMOTED`](audit::JOB_DEMOTED) | `site`, `job`, `from`, `to`, `reason`, `decision`, `charterVersion` |
/// | [`JOB_PAUSED`](audit::JOB_PAUSED) | `site`, `job`, `guardrail`, `metric`, `value`, `threshold`, `charterVersion` |
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_kernel::charter::audit::SIGNAL, "kernel.signal");
/// ```
pub mod audit {
    /// A signal was filed, counted or refused by the rate limit (spec 3.5 §7.10).
    pub const SIGNAL: &str = "kernel.signal";
    /// A charter version took effect (spec 3.5 §7.6).
    pub const CHARTER_COMMITTED: &str = "website.charter.committed";
    /// An approval chain was satisfied by distinct people (A-I23).
    pub const APPROVAL_CHAIN_SATISFIED: &str = "website.approval_chain.satisfied";
    /// An approval chain was not satisfied, so the action was refused (A-I23).
    pub const APPROVAL_CHAIN_REFUSED: &str = "website.approval_chain.refused";
    /// An owner raised a job's autonomy (A-I13).
    pub const JOB_PROMOTED: &str = "website.job.promoted";
    /// A job's autonomy was lowered (spec 3.5 §7.11).
    pub const JOB_DEMOTED: &str = "website.job.demoted";
    /// A guardrail breach paused a job (A-I15).
    pub const JOB_PAUSED: &str = "website.job.paused";
}

/// Something the charter machinery did that the owner hears about (spec 3.5 §7.11, §7.21):
/// the weekly review, alarms and the site view read these.
///
/// # Example
///
/// ```rust
/// use adk_kernel::charter::CharterEvent;
/// use awp_types::Urgency;
///
/// let event = CharterEvent::JobPaused {
///     site: uuid::Uuid::nil(),
///     job: "nurture".into(),
///     reason: "unsubscribes above 2%".into(),
///     at: chrono::Utc::now(),
/// };
/// assert_eq!(event.urgency(), Urgency::P2);
/// assert_eq!(serde_json::to_value(&event).unwrap()["event"], "jobPaused");
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "camelCase", rename_all_fields = "camelCase")]
#[non_exhaustive]
pub enum CharterEvent {
    /// A charter version took effect.
    Committed {
        /// The site.
        site: Uuid,
        /// The version.
        version: u32,
        /// The committing decision.
        decision: String,
        /// When.
        at: DateTime<Utc>,
    },
    /// A KPI's measure-only period ended and its baseline was locked (A-I14): the owner may
    /// now set a target.
    BaselineLocked {
        /// The site.
        site: Uuid,
        /// The KPI.
        kpi: String,
        /// The baseline's value.
        value: f64,
        /// When.
        at: DateTime<Utc>,
    },
    /// A guardrail's metric crossed its threshold (A-I15).
    GuardrailBreached {
        /// The site.
        site: Uuid,
        /// The guardrail.
        guardrail: String,
        /// The metric it watches.
        metric: String,
        /// The metric's value over the guardrail's window.
        value: f64,
        /// The threshold it crossed.
        threshold: f64,
        /// The jobs it affects.
        jobs: Vec<String>,
        /// When.
        at: DateTime<Utc>,
    },
    /// A job was paused before its next action (A-I15).
    JobPaused {
        /// The site.
        site: Uuid,
        /// The job.
        job: String,
        /// Why.
        reason: String,
        /// When.
        at: DateTime<Utc>,
    },
    /// A job's autonomy level changed (A-I13).
    AutonomyChanged {
        /// The site.
        site: Uuid,
        /// The job.
        job: String,
        /// The level it left.
        from: AutonomyLevel,
        /// The level it entered.
        to: AutonomyLevel,
        /// Why, in words the weekly review shows.
        reason: String,
        /// When.
        at: DateTime<Utc>,
    },
    /// A job's record earned it a promotion the weekly review offers (spec 3.5 §7.11).
    PromotionOffered {
        /// The site.
        site: Uuid,
        /// The job.
        job: String,
        /// The level it would enter.
        to: AutonomyLevel,
        /// The unchanged approvals in a row behind the offer.
        streak: usize,
        /// When.
        at: DateTime<Utc>,
    },
}

impl CharterEvent {
    /// How soon the owner hears of it (spec 3.5 §7.21): a guardrail breach and the pause it
    /// causes are P2; everything else waits for the weekly review, P3.
    pub fn urgency(&self) -> Urgency {
        match self {
            Self::GuardrailBreached { .. } | Self::JobPaused { .. } => Urgency::P2,
            Self::Committed { .. }
            | Self::BaselineLocked { .. }
            | Self::AutonomyChanged { .. }
            | Self::PromotionOffered { .. } => Urgency::P3,
        }
    }
}

/// Where [`CharterEvent`]s go: the product's alarms and weekly review (spec 3.5 §7.21).
///
/// # Example
///
/// ```rust
/// use std::sync::Mutex;
/// use adk_kernel::charter::{CharterEvent, CharterEventSink};
///
/// #[derive(Default)]
/// struct Collect(Mutex<Vec<CharterEvent>>);
///
/// #[async_trait::async_trait]
/// impl CharterEventSink for Collect {
///     async fn emit(&self, event: &CharterEvent) {
///         self.0.lock().unwrap().push(event.clone());
///     }
/// }
/// ```
#[async_trait]
pub trait CharterEventSink: Send + Sync {
    /// Receives `event`; a sink that fails logs and drops it, so it never blocks a pause.
    async fn emit(&self, event: &CharterEvent);
}

/// Writes one audit record of `kind` when `trail` is set, logging a failure: an audit write
/// never undoes the act it records.
pub(crate) async fn record(
    trail: Option<&AuditTrail>,
    kind: &str,
    action: &str,
    actor: AuditActor,
    fields: Value,
) {
    let Some(trail) = trail else { return };
    let mut draft =
        AuditRecordDraft::new(AuditRecordKind::Other(kind.to_string()), action).actor(actor);
    if let Value::Object(fields) = fields {
        for (key, value) in fields {
            draft = draft.ext(key, value);
        }
    }
    if let Err(error) = trail.record(draft).await {
        tracing::warn!(audit.action = action, error = %error, "could not write a charter audit record");
    }
}

/// Emits `event` on `sink` when one is set.
pub(crate) async fn emit(sink: Option<&std::sync::Arc<dyn CharterEventSink>>, event: CharterEvent) {
    if let Some(sink) = sink {
        sink.emit(&event).await;
    }
}
