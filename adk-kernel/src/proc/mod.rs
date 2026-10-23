//! `/proc`: the kernel's self-observation (spec 3.2 §7.10, §13 Q4; feature `proc`).
//!
//! The kernel keeps a content-free activity ledger and, behind `proc-baselines`, learns a
//! per-partition baseline from it, in three tiers:
//!
//! | Tier | Where | What |
//! |---|---|---|
//! | Ledger and live aggregates | the kernel, feature `proc` | one append-only table of [`LedgerEntry`] rows; the live `ps`/`top` view and the OTel GenAI histograms are the process table's ([`ProcessView`](crate::ProcessView)) |
//! | Statistics | `adk-eval`'s `ledger` module, pure functions | rolling 28-day median and MAD per dimension and day class, the 14-day warm-up, the windowed deviation test; `cargo adk eval --ledger <export.jsonl>` runs them in CI |
//! | Baselines and observations | the kernel's `Maintenance` process, feature `proc-baselines` | the daily pass (`baselines`) calls the statistics, applies the "meaningful" rule and the seven-day cooldown, and offers each observation as a neutral question |
//!
//! # The ledger
//!
//! A row is `ts, partition, agent, kind, effect, duration_ms, subject_hash, meta, trace_id`
//! ([`LedgerEntry`]): never a payload, a prompt, a file's content or a secret (K-I19). A
//! writer submits an [`Activity`]; the [`Ledger`] checks that every field is a label, a
//! count or a duration, stores what the activity was about only as an HMAC under the
//! kernel's [`LedgerKey`], and stamps the user's local offset.
//!
//! | Writer | Through | Rows |
//! |---|---|---|
//! | The kernel | its event bus: every [`KernelEvent`](crate::KernelEvent) but `Health` ([`activity_for_event`]) | `process`, `wake`, `held`, `decided`, `rule_hit`, `spend`, `governance`, `observation` |
//! | The harness | a [`LedgerSink`] the kernel hands its `ProgressSource` at start ([`ProgressSource::attach_ledger`](crate::ProgressSource::attach_ledger)) | `intent` and `tool_call` from its runs; `approval`, `rejection`, `denial`, `held`, `turn`, `loop_guard` and `audit` from its audit records ([`LedgerAuditTap`], [`activity_for_audit`]) |
//!
//! [`LedgerStore`] holds the rows, the observations and the pass state on every backend the
//! [`ProcessStore`](crate::ProcessStore) has: in-memory, SQLite, PostgreSQL and Redis, with
//! one conformance suite (`ledger_store_conformance_tests!`). The ledger never leaves the
//! host unless its user exports it: `GET /api/kernel/proc/ledger?format=jsonl`, an operator
//! route, writes the JSONL `cargo adk eval --ledger` reads.
//!
//! # Example
//!
//! ```rust
//! use std::sync::Arc;
//! use std::time::Duration;
//! use adk_kernel::proc::{Activity, InMemoryLedgerStore, Ledger, LedgerKey, LedgerKind, LedgerQuery, LedgerSink};
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> adk_kernel::Result<()> {
//! let ledger = Ledger::new(Arc::new(InMemoryLedgerStore::new()), LedgerKey::from_bytes([9; 32]));
//! ledger
//!     .record(
//!         Activity::new(LedgerKind::ToolCall, "urn:adk:agent:adk-rust/harness")
//!             .with_effect("read_only")
//!             .with_duration(Duration::from_millis(140))
//!             .with_subject("read_file\u{1f}{\"path\":\"notes/salary.md\"}"),
//!     )
//!     .await?;
//! let rows = ledger.store().list(&LedgerQuery::default().kind("tool_call")).await?;
//! let line = rows[0].to_jsonl()?;
//! assert!(line.contains("\"durationMs\":140") && !line.contains("salary"));
//! # Ok(())
//! # }
//! ```

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::partition::PartitionId;

#[cfg(feature = "proc-baselines")]
pub mod baselines;
mod ledger;
mod observation;
mod sources;

pub use crate::stores::InMemoryLedgerStore;
#[cfg(feature = "postgres")]
pub use crate::stores::PostgresLedgerStore;
#[cfg(feature = "redis")]
pub use crate::stores::RedisLedgerStore;
#[cfg(feature = "sqlite")]
pub use crate::stores::SqliteLedgerStore;
pub use ledger::{
    Activity, KERNEL_AGENT, LEDGER_INVALID_CODE, LateLedgerSink, Ledger, LedgerEntry, LedgerKey,
    LedgerKind, LedgerOffset, LedgerSink, MAX_META_ENTRIES, label,
};
pub use observation::{
    Direction, Observation, ObservedDimension, PassState, Phrasing, is_neutral_question,
    template_question,
};
pub(crate) use sources::LedgerEventSink;
pub use sources::{LedgerAuditTap, activity_for_audit, activity_for_event};

/// The kernel's ledger, observations and baseline pass state (spec 3.2 §7.10, §13 Q8).
///
/// The ledger is append-only: rows are added and read, never changed. Observations are
/// replaced by id, so two replicas recording one observation store one; the pass state is
/// one record per partition.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_kernel::proc::{Activity, InMemoryLedgerStore, Ledger, LedgerKey, LedgerKind, LedgerQuery, LedgerStore};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_kernel::Result<()> {
/// let store = InMemoryLedgerStore::new();
/// let ledger = Ledger::new(Arc::new(InMemoryLedgerStore::new()), LedgerKey::from_bytes([1; 32]));
/// let first = store.append(ledger.entry(Activity::new(LedgerKind::Wake, "kernel"))?).await?;
/// let second = store.append(ledger.entry(Activity::new(LedgerKind::Intent, "harness"))?).await?;
/// assert!(second.seq > first.seq);
/// let after = store.list(&LedgerQuery::default().after(first.seq)).await?;
/// assert_eq!(after, [second]);
/// # Ok(())
/// # }
/// ```
#[async_trait]
pub trait LedgerStore: Send + Sync {
    /// Appends `entry` with the next sequence number and returns it as stored; `entry.seq`
    /// is ignored.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the backend fails.
    async fn append(&self, entry: LedgerEntry) -> Result<LedgerEntry>;

    /// The rows `query` selects, in sequence order, at most `query.limit`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the backend fails.
    async fn list(&self, query: &LedgerQuery) -> Result<Vec<LedgerEntry>>;

    /// Inserts `observation`, or replaces the one with the same id.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the backend fails.
    async fn put_observation(&self, observation: Observation) -> Result<()>;

    /// The observations `query` selects, newest day first, then by id.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the backend fails.
    async fn observations(&self, query: &ObservationQuery) -> Result<Vec<Observation>>;

    /// The pass state of `partition`, or `None` before its first pass.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the backend fails.
    async fn pass_state(&self, partition: &PartitionId) -> Result<Option<PassState>>;

    /// Every partition's pass state, by partition.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the backend fails.
    async fn pass_states(&self) -> Result<Vec<PassState>>;

    /// Stores `state` as its partition's pass state.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`](crate::KernelError::Store) when the backend fails.
    async fn put_pass_state(&self, state: PassState) -> Result<()>;
}

/// Which ledger rows [`LedgerStore::list`] returns (spec 3.2 §7.10, §7.11
/// `GET /kernel/proc/ledger`). An empty field matches every row.
///
/// # Example
///
/// ```rust
/// use adk_kernel::proc::LedgerQuery;
///
/// let query = LedgerQuery::default().partition("work").kind("tool_call").after(41).limit(100);
/// assert_eq!(query.kinds, ["tool_call"]);
/// assert_eq!(query.after, Some(41));
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct LedgerQuery {
    /// Only rows of this partition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition: Option<PartitionId>,
    /// Only rows of these kinds.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kinds: Vec<String>,
    /// Only rows of this agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Only rows at or after this instant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<DateTime<Utc>>,
    /// Only rows before this instant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<DateTime<Utc>>,
    /// Only rows after this sequence number: the cursor of the previous page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<u64>,
    /// At most this many rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

impl LedgerQuery {
    /// Only rows of `partition`.
    #[must_use]
    pub fn partition(mut self, partition: impl Into<PartitionId>) -> Self {
        self.partition = Some(partition.into());
        self
    }

    /// Also rows of `kind`.
    #[must_use]
    pub fn kind(mut self, kind: impl Into<String>) -> Self {
        self.kinds.push(kind.into());
        self
    }

    /// Only rows of `agent`.
    #[must_use]
    pub fn agent(mut self, agent: impl Into<String>) -> Self {
        self.agent = Some(agent.into());
        self
    }

    /// Only rows at or after `from`.
    #[must_use]
    pub fn from(mut self, from: DateTime<Utc>) -> Self {
        self.from = Some(from);
        self
    }

    /// Only rows before `to`.
    #[must_use]
    pub fn to(mut self, to: DateTime<Utc>) -> Self {
        self.to = Some(to);
        self
    }

    /// Only rows after sequence number `after`.
    #[must_use]
    pub fn after(mut self, after: u64) -> Self {
        self.after = Some(after);
        self
    }

    /// At most `limit` rows.
    #[must_use]
    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Whether `entry` passes every field but `limit`.
    pub fn matches(&self, entry: &LedgerEntry) -> bool {
        let at = entry.utc();
        self.partition.as_ref().is_none_or(|partition| *partition == entry.partition)
            && (self.kinds.is_empty() || self.kinds.contains(&entry.kind))
            && self.agent.as_ref().is_none_or(|agent| *agent == entry.agent)
            && self.from.is_none_or(|from| at >= from)
            && self.to.is_none_or(|to| at < to)
            && self.after.is_none_or(|after| entry.seq > after)
    }
}

/// Which observations [`LedgerStore::observations`] returns (spec 3.2 §7.11
/// `GET /kernel/proc/observations`).
///
/// # Example
///
/// ```rust
/// use adk_kernel::proc::ObservationQuery;
///
/// let query = ObservationQuery::default().partition("work").limit(5);
/// assert_eq!(query.limit, Some(5));
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ObservationQuery {
    /// Only observations of this partition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition: Option<PartitionId>,
    /// Only observations of this day or later.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<NaiveDate>,
    /// At most this many.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

impl ObservationQuery {
    /// Only observations of `partition`.
    #[must_use]
    pub fn partition(mut self, partition: impl Into<PartitionId>) -> Self {
        self.partition = Some(partition.into());
        self
    }

    /// Only observations of `since` or later.
    #[must_use]
    pub fn since(mut self, since: NaiveDate) -> Self {
        self.since = Some(since);
        self
    }

    /// At most `limit` observations.
    #[must_use]
    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Whether `observation` passes every field but `limit`.
    pub fn matches(&self, observation: &Observation) -> bool {
        self.partition.as_ref().is_none_or(|partition| *partition == observation.partition)
            && self.since.is_none_or(|since| observation.as_of >= since)
    }
}

/// Orders observations newest day first, then by id, as every backend returns them.
pub(crate) fn order_observations(observations: &mut [Observation]) {
    observations.sort_by(|a, b| b.as_of.cmp(&a.as_of).then_with(|| a.id.cmp(&b.id)));
}
