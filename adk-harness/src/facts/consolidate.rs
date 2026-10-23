//! Fact consolidation: the consolidation tick's two passes over long-term facts, the loss
//! cap, the tick record and `undo` (spec 3.2 §7.5, §10 HK2, §9.2 K-I12, K-I22, §13 Q12).
//!
//! | Step | Model call | Writes |
//! |---|---|---|
//! | Distillation of the sessions the per-session distiller never reached ([`Harness::distill_sessions`]) | one per session | `Assumed` facts only: verified-quote promotion is off |
//! | Pass 1 | none | the older of two `Assumed` facts at cosine ≥ `dedup_score` → `Superseded { by: newer }`; an unused, low-confidence `Assumed` fact older than `assumed_ttl` → `Forgotten` (reason `expired`) |
//! | Pass 2 | one per cluster, at most `max_clusters_per_tick` | a merged `Assumed` fact, source `AgentInferred`, its provenance listing the sources; the sources it names in `supersedes` → `Superseded { by: merged }` |
//! | Loss cap | — | nothing from either pass when they would change the status of more than `max_loss_fraction` of a scope's live facts |
//!
//! `Known` facts are never superseded, merged or expired, and an `Assumed` fact never
//! supersedes a `Known` one: it is stored beside it and listed for a person to confirm
//! (K-I22). History is kept, so [`undo_consolidation`] is a status flip.
//!
//! [`Harness::distill_sessions`]: crate::Harness::distill_sessions

#[cfg(feature = "runner")]
use std::collections::{HashMap, HashSet};
use std::time::Duration;

use adk_core::principal::SubjectRef;
use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result, RunBudget};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[cfg(feature = "runner")]
use super::index::cosine;
use super::store::FactStore;
#[cfg(feature = "runner")]
use super::types::{FactKind, MemoryFact, fact_digest};
use super::types::{FactScope, FactStatus};

/// Least cosine similarity of two facts in one pass-2 cluster (spec 3.2 §7.5): at or above
/// it and below `dedup_score`, facts are similar but not equivalent, so a model merges them.
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::CONSOLIDATION_CLUSTER_SCORE, 0.80);
/// ```
pub const CONSOLIDATION_CLUSTER_SCORE: f32 = 0.80;

/// Most facts in one pass-2 cluster, so a merge prompt stays small.
#[cfg(feature = "runner")]
const CLUSTER_MAX_FACTS: usize = 8;

/// The fact-consolidation settings of a consolidation tick (spec 3.2 §7.5, §13 Q12): the
/// thresholds of the two passes, the loss cap and the tick's token cap, with the defaults
/// spec 3.2 gives `ConsolidationTick`.
///
/// # Example
///
/// ```rust
/// use std::time::Duration;
/// use adk_harness::ConsolidationConfig;
///
/// let config = ConsolidationConfig::default();
/// assert_eq!((config.dedup_score, config.expire_confidence), (0.92, 0.5));
/// assert_eq!(config.assumed_ttl, Duration::from_secs(90 * 24 * 60 * 60));
/// assert_eq!((config.max_clusters_per_tick, config.max_loss_fraction), (20, 0.25));
/// ```
#[derive(Clone, Debug, PartialEq)]
pub struct ConsolidationConfig {
    /// Pass 1 collapses `Assumed` facts of one scope and kind at or above this cosine
    /// similarity: the older becomes `Superseded` by the newer.
    pub dedup_score: f32,
    /// Pass 1 expires an unused `Assumed` fact whose confidence is under this.
    pub expire_confidence: f32,
    /// Pass 1 expires an unused, low-confidence `Assumed` fact older than this.
    pub assumed_ttl: Duration,
    /// Most clusters pass 2 sends to the model in one tick.
    pub max_clusters_per_tick: usize,
    /// The passes apply nothing when they would change the status of more than this share
    /// of a scope's live facts, rounded down, and at least one fact.
    pub max_loss_fraction: f32,
    /// Most tokens the tick's model calls spend: distillation and pass 2 together.
    pub max_tokens_per_tick: u64,
}

impl Default for ConsolidationConfig {
    fn default() -> Self {
        Self {
            dedup_score: 0.92,
            expire_confidence: 0.5,
            assumed_ttl: Duration::from_secs(90 * 24 * 60 * 60),
            max_clusters_per_tick: 20,
            max_loss_fraction: 0.25,
            max_tokens_per_tick: 50_000,
        }
    }
}

/// One consolidation tick asked of [`Harness::consolidate_memory`] (spec 3.2 §7.5).
///
/// [`Harness::consolidate_memory`]: crate::Harness::consolidate_memory
///
/// # Example
///
/// ```rust
/// use adk_harness::ConsolidationRequest;
///
/// let request = ConsolidationRequest::new()
///     .with_tick_id("pid-01J")
///     .with_partition("work")
///     .with_sessions(["s1", "s2"]);
/// assert_eq!(request.sessions.len(), 2);
/// ```
#[derive(Clone, Debug, Default)]
pub struct ConsolidationRequest {
    /// The tick's id, the kernel process's pid under a kernel; a new ULID when `None`.
    pub tick_id: Option<String>,
    /// The partition the tick runs for; `default` when `None`.
    pub partition: Option<String>,
    /// Sessions to distil when the per-session distiller never reached them.
    pub sessions: Vec<String>,
    /// The passes' thresholds and caps.
    pub config: ConsolidationConfig,
    /// The tick's share of the partition's proactive budget; `max_tokens_per_tick` caps
    /// its tokens whatever it says.
    pub budget: Option<RunBudget>,
    /// The tick's clock reading; now when `None`.
    pub at: Option<DateTime<Utc>>,
}

impl ConsolidationRequest {
    /// A tick with the default settings and no session to distil.
    pub fn new() -> Self {
        Self::default()
    }

    /// This request with tick id `tick_id`.
    #[must_use]
    pub fn with_tick_id(mut self, tick_id: impl Into<String>) -> Self {
        self.tick_id = Some(tick_id.into());
        self
    }

    /// This request for partition `partition`.
    #[must_use]
    pub fn with_partition(mut self, partition: impl Into<String>) -> Self {
        self.partition = Some(partition.into());
        self
    }

    /// This request distilling `sessions` the per-session distiller never reached.
    #[must_use]
    pub fn with_sessions(mut self, sessions: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.sessions = sessions.into_iter().map(Into::into).collect();
        self
    }

    /// This request with `config`.
    #[must_use]
    pub fn with_config(mut self, config: ConsolidationConfig) -> Self {
        self.config = config;
        self
    }

    /// This request spending at most `budget`.
    #[must_use]
    pub fn with_budget(mut self, budget: RunBudget) -> Self {
        self.budget = Some(budget);
        self
    }

    /// This request at clock reading `at`.
    #[must_use]
    pub fn at(mut self, at: DateTime<Utc>) -> Self {
        self.at = Some(at);
        self
    }
}

/// Why a consolidation tick changed a fact's status (spec 3.2 §7.5).
///
/// # Example
///
/// ```rust
/// assert_eq!(serde_json::to_value(adk_harness::ChangeReason::Expired).unwrap(), "expired");
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeReason {
    /// Pass 1: a newer `Assumed` fact at or above `dedup_score` says the same.
    Duplicate,
    /// Pass 1: unused, low-confidence and older than `assumed_ttl`.
    Expired,
    /// Pass 2: merged into a new fact.
    Merged,
    /// Distillation: a distilled fact names it in `supersedes`, or corrects it.
    Superseded,
    /// Distillation: the least-used `Assumed` fact of a full scope.
    Evicted,
}

impl ChangeReason {
    /// The wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Duplicate => "duplicate",
            Self::Expired => "expired",
            Self::Merged => "merged",
            Self::Superseded => "superseded",
            Self::Evicted => "evicted",
        }
    }
}

/// One status change a consolidation tick made: always from `Assumed`, which `undo`
/// restores (spec 3.2 §7.5, K-I22).
///
/// # Example
///
/// ```rust
/// use adk_harness::{ChangeReason, FactChange, FactScope, FactStatus};
///
/// let change = FactChange {
///     fact: "01JA".into(),
///     scope: FactScope::new("app", "alice", None),
///     to: FactStatus::Forgotten,
///     reason: ChangeReason::Expired,
/// };
/// assert!(!change.to.is_live());
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FactChange {
    /// The fact.
    pub fact: String,
    /// Its scope.
    pub scope: FactScope,
    /// The status the tick gave it.
    pub to: FactStatus,
    /// Why.
    pub reason: ChangeReason,
}

/// An `Assumed` fact a tick stored beside the `Known` fact it would have superseded, for a
/// person to confirm or forget (spec 3.2 §7.5, K-I22).
///
/// # Example
///
/// ```rust
/// let beside = adk_harness::BesideKnown { fact: "01JB".into(), known: "01JA".into() };
/// assert_ne!(beside.fact, beside.known);
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BesideKnown {
    /// The `Assumed` fact.
    pub fact: String,
    /// The `Known` fact it stands beside.
    pub known: String,
}

/// What one consolidation tick did, kept in the [`FactStore`] so `undo` can revert it
/// (spec 3.2 §7.5): fact ids and counts, never fact text.
///
/// # Example
///
/// ```rust
/// use adk_harness::ConsolidationRecord;
///
/// let record = ConsolidationRecord::new("01JT", "app", "alice", "default", chrono::Utc::now());
/// assert_eq!((record.superseded().count(), record.forgotten().count()), (0, 0));
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsolidationRecord {
    /// The tick's id.
    pub tick_id: String,
    /// The application.
    pub app: String,
    /// The user whose facts it consolidated.
    pub user: String,
    /// The partition it ran for.
    pub partition: String,
    /// When it ran.
    pub at: DateTime<Utc>,
    /// The scopes the passes covered.
    #[serde(default)]
    pub scopes: Vec<FactScope>,
    /// The sessions it distilled.
    #[serde(default)]
    pub sessions: Vec<String>,
    /// The facts its distillation stored, all `Assumed`.
    #[serde(default)]
    pub distilled: Vec<String>,
    /// The facts pass 2 stored, all `Assumed`.
    #[serde(default)]
    pub merged: Vec<String>,
    /// Every status change it made.
    #[serde(default)]
    pub changes: Vec<FactChange>,
    /// The `Assumed` facts it stored beside a `Known` one.
    #[serde(default)]
    pub beside_known: Vec<BesideKnown>,
    /// Why the passes applied nothing: the loss cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused: Option<String>,
    /// Model calls made.
    #[serde(default)]
    pub model_calls: u64,
    /// Tokens spent.
    #[serde(default)]
    pub total_tokens: u64,
    /// Cost in millionths of a US dollar.
    #[serde(default)]
    pub cost_micro_usd: u64,
    /// When the tick finished; `None` while it runs, and for one cut off, which its kernel
    /// runs again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<Utc>>,
    /// When `undo` reverted it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undone_at: Option<DateTime<Utc>>,
}

impl ConsolidationRecord {
    /// An empty record of tick `tick_id` for `user` of `app` in `partition` at `at`.
    pub fn new(
        tick_id: impl Into<String>,
        app: impl Into<String>,
        user: impl Into<String>,
        partition: impl Into<String>,
        at: DateTime<Utc>,
    ) -> Self {
        Self {
            tick_id: tick_id.into(),
            app: app.into(),
            user: user.into(),
            partition: partition.into(),
            at,
            scopes: Vec::new(),
            sessions: Vec::new(),
            distilled: Vec::new(),
            merged: Vec::new(),
            changes: Vec::new(),
            beside_known: Vec::new(),
            refused: None,
            model_calls: 0,
            total_tokens: 0,
            cost_micro_usd: 0,
            finished_at: None,
            undone_at: None,
        }
    }

    /// The facts it superseded.
    pub fn superseded(&self) -> impl Iterator<Item = &FactChange> {
        self.changes.iter().filter(|change| matches!(change.to, FactStatus::Superseded { .. }))
    }

    /// The facts it forgot.
    pub fn forgotten(&self) -> impl Iterator<Item = &FactChange> {
        self.changes.iter().filter(|change| change.to == FactStatus::Forgotten)
    }

    /// The facts it wrote: distilled and merged.
    pub fn written(&self) -> impl Iterator<Item = &String> {
        self.distilled.iter().chain(&self.merged)
    }
}

/// What [`undo_consolidation`] reverted (spec 3.2 §7.5).
///
/// # Example
///
/// ```rust
/// let report = adk_harness::ConsolidationUndo::default();
/// assert!(report.restored.is_empty() && report.kept.is_empty());
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsolidationUndo {
    /// The tick.
    pub tick_id: String,
    /// Facts back to `Assumed`.
    pub restored: Vec<String>,
    /// Facts the tick wrote, now `Forgotten`.
    pub forgotten: Vec<String>,
    /// Facts left as they are: a person or a later write changed them since the tick.
    pub kept: Vec<String>,
}

fn invalid(code: &'static str, message: String) -> AdkError {
    AdkError::new(ErrorComponent::Agent, ErrorCategory::InvalidInput, code, message)
}

/// The consolidation record `tick_id` of `user` of `app`: an exact id, or a unique prefix.
///
/// # Errors
///
/// Returns `harness.memory.tick_not_found` when no record matches, or more than one does,
/// or the store's error.
pub async fn find_consolidation(
    store: &dyn FactStore,
    app: &str,
    user: &str,
    tick_id: &str,
) -> Result<ConsolidationRecord> {
    let wanted = tick_id.trim();
    let records = store.consolidations(app, user).await?;
    if let Some(record) = records.iter().find(|record| record.tick_id == wanted) {
        return Ok(record.clone());
    }
    let upper = wanted.to_ascii_uppercase();
    let mut matches = records.into_iter().filter(|record| {
        !wanted.is_empty()
            && (record.tick_id.starts_with(wanted) || record.tick_id.starts_with(&upper))
    });
    match (matches.next(), matches.next()) {
        (Some(record), None) => Ok(record),
        (None, _) => Err(AdkError::new(
            ErrorComponent::Agent,
            ErrorCategory::NotFound,
            "harness.memory.tick_not_found",
            format!(
                "there is no consolidation tick '{wanted}'; `adk-rust memory list --source consolidation` lists them"
            ),
        )),
        (Some(_), Some(_)) => Err(invalid(
            "harness.memory.tick_not_found",
            format!("'{wanted}' matches more than one consolidation tick; give more of the id"),
        )),
    }
}

/// Reverts consolidation tick `tick_id` of `user` of `app` (spec 3.2 §7.5): each fact the
/// tick superseded or forgot goes back to `Assumed` with `valid_to` cleared, and each fact
/// it wrote is forgotten, its history kept. A change a person or a later write has made
/// since the tick is kept and reported in [`ConsolidationUndo::kept`]. Run right after the
/// tick, `undo` is the identity on the statuses the facts had before it.
///
/// Records the tick as undone; a tick is undone once, and only once it finished, so no
/// change it makes after the undo escapes it.
///
/// # Example
///
/// ```rust,ignore
/// let report = adk_harness::undo_consolidation(store.as_ref(), "adk-rust", "alice", &tick_id, &by).await?;
/// println!("{} facts restored", report.restored.len());
/// ```
///
/// # Errors
///
/// Returns `harness.memory.tick_not_found` for an unknown tick,
/// `harness.memory.tick_unfinished` for one that has not finished,
/// `harness.memory.tick_undone` for one already undone, or the store's error.
pub async fn undo_consolidation(
    store: &dyn FactStore,
    app: &str,
    user: &str,
    tick_id: &str,
    by: &SubjectRef,
) -> Result<ConsolidationUndo> {
    let mut record = find_consolidation(store, app, user, tick_id).await?;
    if record.finished_at.is_none() {
        return Err(AdkError::new(
            ErrorComponent::Agent,
            ErrorCategory::Conflict,
            "harness.memory.tick_unfinished",
            format!(
                "consolidation tick '{}' has not finished; undo it once it has (a tick cut off \
                 is run again by its kernel)",
                record.tick_id
            ),
        ));
    }
    if record.undone_at.is_some() {
        return Err(AdkError::new(
            ErrorComponent::Agent,
            ErrorCategory::Conflict,
            "harness.memory.tick_undone",
            format!("consolidation tick '{}' was undone already", record.tick_id),
        ));
    }
    let mut report = ConsolidationUndo { tick_id: record.tick_id.clone(), ..Default::default() };
    // Restore first, then forget what the tick wrote, so no fact is lost in between.
    for change in record.changes.iter().rev() {
        let current = store.get(&change.fact).await?;
        match current {
            Some(fact) if fact.status == change.to => {
                store.restore_assumed(&change.fact, by).await?;
                report.restored.push(change.fact.clone());
            }
            _ => report.kept.push(change.fact.clone()),
        }
    }
    for id in record.written() {
        match store.get(id).await? {
            Some(fact) if fact.status == FactStatus::Assumed => {
                store.set_status(id, FactStatus::Forgotten, by).await?;
                report.forgotten.push(id.clone());
            }
            _ => report.kept.push(id.clone()),
        }
    }
    record.undone_at = Some(Utc::now());
    store.put_consolidation(&record).await?;
    Ok(report)
}

/// What [`Harness::distill_sessions`] did (spec 3.2 §10 HK2): the sessions it distilled
/// and skipped, the facts it stored — every one `Assumed` — and what it spent.
///
/// [`Harness::distill_sessions`]: crate::Harness::distill_sessions
///
/// # Example
///
/// ```rust
/// let report = adk_harness::SessionDistillation::default();
/// assert!(report.stored.is_empty());
/// ```
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionDistillation {
    /// Sessions distilled.
    pub sessions: Vec<String>,
    /// Sessions skipped: already distilled up to their last event, still active, due to
    /// the per-session distiller, in ReadOnly or Plan mode, or not readable.
    pub skipped: Vec<String>,
    /// Facts stored, all `Assumed`.
    pub stored: Vec<String>,
    /// Candidates refused.
    pub rejected: usize,
    /// Status changes the stored facts made: a named `supersedes`, a correction, an
    /// eviction.
    pub changes: Vec<FactChange>,
    /// Facts stored beside a `Known` fact they named.
    pub beside_known: Vec<BesideKnown>,
    /// Model calls made.
    pub model_calls: u64,
    /// Tokens spent.
    pub total_tokens: u64,
    /// Cost in millionths of a US dollar.
    pub cost_micro_usd: u64,
}

/// One scope's live facts and their stored vectors, as the passes read them.
#[cfg(feature = "runner")]
#[derive(Clone, Debug, Default)]
pub(crate) struct ScopeFacts {
    pub(crate) live: Vec<MemoryFact>,
    pub(crate) vectors: HashMap<String, Vec<f32>>,
}

#[cfg(feature = "runner")]
impl ScopeFacts {
    fn similarity(&self, a: &MemoryFact, b: &MemoryFact) -> Option<f32> {
        cosine(self.vectors.get(&a.id)?, self.vectors.get(&b.id)?)
    }
}

/// Newest first: by `valid_from`, then id (ULIDs sort by time).
#[cfg(feature = "runner")]
fn newest_first(facts: &mut [&MemoryFact]) {
    facts.sort_by(|a, b| b.valid_from.cmp(&a.valid_from).then_with(|| b.id.cmp(&a.id)));
}

/// Whether pass 1 expires `fact` at `now`: `Assumed`, never used or recalled, a confidence
/// under `expire_confidence`, and older than `assumed_ttl`. A fact without a confidence is
/// not expired: its confidence is unknown, not low.
#[cfg(feature = "runner")]
pub(crate) fn expires(fact: &MemoryFact, config: &ConsolidationConfig, now: DateTime<Utc>) -> bool {
    let ttl = chrono::Duration::from_std(config.assumed_ttl).unwrap_or(chrono::Duration::MAX);
    fact.status == FactStatus::Assumed
        && fact.uses == 0
        && fact.last_recalled.is_none()
        && fact.confidence.is_some_and(|confidence| confidence < config.expire_confidence)
        && now.signed_duration_since(fact.valid_from) > ttl
}

/// Pass 1 over one scope (spec 3.2 §7.5): no model call. Expiry first, so an expiring
/// newer fact never takes a used older one with it; then, per kind and newest first, each
/// `Assumed` fact at or above `dedup_score` to a newer survivor becomes `Superseded` by the
/// most similar one. Returns the changes and the `Assumed` survivors.
#[cfg(feature = "runner")]
pub(crate) fn pass_one(
    facts: &ScopeFacts,
    config: &ConsolidationConfig,
    now: DateTime<Utc>,
) -> (Vec<FactChange>, Vec<MemoryFact>) {
    let mut changes = Vec::new();
    let mut assumed: Vec<&MemoryFact> =
        facts.live.iter().filter(|fact| fact.status == FactStatus::Assumed).collect();
    newest_first(&mut assumed);
    let mut remaining = Vec::new();
    for fact in assumed {
        if expires(fact, config, now) {
            changes.push(FactChange {
                fact: fact.id.clone(),
                scope: fact.scope.clone(),
                to: FactStatus::Forgotten,
                reason: ChangeReason::Expired,
            });
        } else {
            remaining.push(fact);
        }
    }
    let mut survivors: Vec<&MemoryFact> = Vec::new();
    for fact in remaining {
        let best = survivors
            .iter()
            .filter(|survivor| survivor.kind == fact.kind)
            .filter_map(|survivor| Some((*survivor, facts.similarity(survivor, fact)?)))
            .filter(|(_, score)| *score >= config.dedup_score)
            .max_by(|a, b| a.1.total_cmp(&b.1));
        match best {
            Some((survivor, _)) => changes.push(FactChange {
                fact: fact.id.clone(),
                scope: fact.scope.clone(),
                to: FactStatus::Superseded { by: survivor.id.clone() },
                reason: ChangeReason::Duplicate,
            }),
            None => survivors.push(fact),
        }
    }
    (changes, survivors.into_iter().cloned().collect())
}

/// The pass-2 clusters of one scope's survivors (spec 3.2 §7.5): per kind and newest
/// first, a fact and the older unclustered facts whose similarity to it is at least
/// [`CONSOLIDATION_CLUSTER_SCORE`] and below `dedup_score`; two facts at least, at most
/// `limit` clusters.
#[cfg(feature = "runner")]
pub(crate) fn clusters(
    facts: &ScopeFacts,
    survivors: &[MemoryFact],
    config: &ConsolidationConfig,
    limit: usize,
) -> Vec<Vec<MemoryFact>> {
    let mut ordered: Vec<&MemoryFact> = survivors.iter().collect();
    newest_first(&mut ordered);
    let mut taken = HashSet::new();
    let mut out = Vec::new();
    for (i, head) in ordered.iter().enumerate() {
        if out.len() >= limit {
            break;
        }
        if taken.contains(&head.id) {
            continue;
        }
        let mut cluster = vec![(*head).clone()];
        for other in &ordered[i + 1..] {
            if cluster.len() >= CLUSTER_MAX_FACTS {
                break;
            }
            if taken.contains(&other.id) || other.kind != head.kind {
                continue;
            }
            let similar = facts.similarity(head, other).is_some_and(|score| {
                (CONSOLIDATION_CLUSTER_SCORE..config.dedup_score).contains(&score)
            });
            if similar {
                cluster.push((*other).clone());
            }
        }
        if cluster.len() >= 2 {
            taken.extend(cluster.iter().map(|fact| fact.id.clone()));
            out.push(cluster);
        }
    }
    out
}

/// A model's answer for one pass-2 cluster: `{ merged_text, source_ids, supersedes }`.
#[cfg(feature = "runner")]
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub(crate) struct MergeAnswer {
    #[serde(default)]
    pub(crate) merged_text: String,
    #[serde(default)]
    pub(crate) source_ids: Vec<String>,
    #[serde(default)]
    pub(crate) supersedes: Vec<String>,
}

/// A merge pass 2 will apply.
#[cfg(feature = "runner")]
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PlannedMerge {
    /// A new `Assumed` fact with `text` from `sources`, superseding `supersedes`.
    New {
        kind: FactKind,
        scope: FactScope,
        text: String,
        sources: Vec<MemoryFact>,
        supersedes: Vec<String>,
    },
    /// The answer repeated source `into`'s text: it supersedes the others.
    Into { into: String, supersedes: Vec<String> },
}

#[cfg(feature = "runner")]
impl PlannedMerge {
    pub(crate) fn supersedes(&self) -> &[String] {
        match self {
            Self::New { supersedes, .. } | Self::Into { supersedes, .. } => supersedes,
        }
    }
}

/// Validates a pass-2 answer against its cluster (spec 3.2 §7.5, K-I22): sources and
/// superseded facts must be the cluster's (`Assumed`, by construction), a merge supersedes
/// at least one fact, the text passes the fact checks, and a text equal to a live fact
/// outside the cluster is no merge. `text` is the merged text after redaction, or `None`
/// when it was refused.
#[cfg(feature = "runner")]
pub(crate) fn plan_merge(
    cluster: &[MemoryFact],
    answer: &MergeAnswer,
    text: Option<String>,
    scope_live: &[MemoryFact],
) -> Option<PlannedMerge> {
    let text = text?;
    let in_cluster = |id: &String| {
        cluster.iter().any(|fact| fact.id == *id && fact.status == FactStatus::Assumed)
    };
    let mut sources: Vec<MemoryFact> = Vec::new();
    for id in answer.source_ids.iter().filter(|id| in_cluster(id)) {
        if !sources.iter().any(|fact| fact.id == *id) {
            sources.extend(cluster.iter().find(|fact| fact.id == *id).cloned());
        }
    }
    let mut supersedes: Vec<String> = Vec::new();
    for id in &answer.supersedes {
        if sources.iter().any(|fact| fact.id == *id) && !supersedes.contains(id) {
            supersedes.push(id.clone());
        }
    }
    let head = cluster.first()?;
    let digest = fact_digest(&text);
    if let Some(same) = sources.iter().find(|fact| fact.digest == digest) {
        supersedes.retain(|id| *id != same.id);
        return (!supersedes.is_empty())
            .then(|| PlannedMerge::Into { into: same.id.clone(), supersedes });
    }
    if supersedes.is_empty() || scope_live.iter().any(|fact| fact.digest == digest) {
        return None;
    }
    Some(PlannedMerge::New {
        kind: head.kind,
        scope: head.scope.clone(),
        text,
        sources,
        supersedes,
    })
}

/// The most status changes the passes may make in a scope of `live` facts (spec 3.2
/// §7.5): `max(1, ⌊max_loss_fraction × live⌋)`. The cap stops mass loss; a scope too small
/// for one change in its share may still collapse one duplicate.
#[cfg(feature = "runner")]
pub(crate) fn loss_cap(live: usize, max_loss_fraction: f32) -> usize {
    ((max_loss_fraction * live as f32).floor() as usize).max(1)
}

#[cfg(all(test, feature = "runner"))]
mod tests {
    use super::*;
    use crate::facts::{FactSource, Provenance};

    const DAY: i64 = 24 * 60 * 60;

    fn fact(kind: FactKind, status: FactStatus, age_days: i64, now: DateTime<Utc>) -> MemoryFact {
        let mut fact = MemoryFact::new(
            FactScope::new("app", "alice", Some("p".into())),
            kind,
            format!("fact {}", uuid::Uuid::new_v4()),
            status,
            Provenance::new(FactSource::AgentInferred, "s", "i"),
        );
        fact.valid_from = now - chrono::Duration::days(age_days);
        fact
    }

    /// Two unit vectors at cosine `score`.
    fn pair(score: f32) -> (Vec<f32>, Vec<f32>) {
        (vec![1.0, 0.0], vec![score, (1.0 - score * score).max(0.0).sqrt()])
    }

    fn scope(facts: Vec<(MemoryFact, Vec<f32>)>) -> ScopeFacts {
        let mut out = ScopeFacts::default();
        for (fact, vector) in facts {
            out.vectors.insert(fact.id.clone(), vector);
            out.live.push(fact);
        }
        out
    }

    #[test]
    fn pass_one_supersedes_the_older_at_the_dedup_score_and_not_below() {
        let now = Utc::now();
        let config = ConsolidationConfig::default();
        for (score, superseded) in [(0.92f32, true), (0.9199, false), (0.99, true)] {
            let (a, b) = pair(score);
            let older = fact(FactKind::ProjectFact, FactStatus::Assumed, 2, now);
            let newer = fact(FactKind::ProjectFact, FactStatus::Assumed, 1, now);
            let facts = scope(vec![(older.clone(), a), (newer.clone(), b)]);
            let (changes, survivors) = pass_one(&facts, &config, now);
            if superseded {
                assert_eq!(
                    changes,
                    vec![FactChange {
                        fact: older.id.clone(),
                        scope: older.scope.clone(),
                        to: FactStatus::Superseded { by: newer.id.clone() },
                        reason: ChangeReason::Duplicate,
                    }],
                    "score {score}"
                );
                assert_eq!(survivors, vec![newer]);
            } else {
                assert!(changes.is_empty(), "score {score}: {changes:?}");
            }
        }
    }

    #[test]
    fn pass_one_never_touches_known_facts_or_other_kinds() {
        let now = Utc::now();
        let (a, b) = pair(1.0);
        let known = fact(FactKind::ProjectFact, FactStatus::Known, 400, now);
        let twin = fact(FactKind::ProjectFact, FactStatus::Assumed, 1, now);
        let other_kind = fact(FactKind::Decision, FactStatus::Assumed, 2, now);
        let facts =
            scope(vec![(known, a.clone()), (twin.clone(), b.clone()), (other_kind.clone(), a)]);
        let (changes, survivors) = pass_one(&facts, &ConsolidationConfig::default(), now);
        assert!(changes.is_empty(), "{changes:?}");
        assert_eq!(survivors.len(), 2);
    }

    #[test]
    fn expiry_holds_exactly_past_the_thresholds() {
        let now = Utc::now();
        let config = ConsolidationConfig::default();
        let stale = |confidence: f32, age: chrono::Duration| {
            let mut fact =
                fact(FactKind::Outcome, FactStatus::Assumed, 0, now).with_confidence(confidence);
            fact.valid_from = now - age;
            fact
        };
        let ninety = chrono::Duration::seconds(90 * DAY);
        let second = chrono::Duration::seconds(1);
        assert!(expires(&stale(0.49, ninety + second), &config, now));
        assert!(!expires(&stale(0.5, ninety + second), &config, now), "0.5 is not under 0.5");
        assert!(!expires(&stale(0.49, ninety), &config, now), "90 days is not over 90 days");
        let mut used = stale(0.1, ninety * 2);
        used.uses = 1;
        assert!(!expires(&used, &config, now));
        let mut recalled = stale(0.1, ninety * 2);
        recalled.last_recalled = Some(now);
        assert!(!expires(&recalled, &config, now));
        let mut unknown = stale(0.1, ninety * 2);
        unknown.confidence = None;
        assert!(!expires(&unknown, &config, now), "an unknown confidence is not a low one");
        let mut known = stale(0.1, ninety * 2);
        known.status = FactStatus::Known;
        assert!(!expires(&known, &config, now));
    }

    #[test]
    fn clusters_take_the_band_below_the_dedup_score() {
        let now = Utc::now();
        let config = ConsolidationConfig::default();
        let head = fact(FactKind::Procedure, FactStatus::Assumed, 1, now);
        let near = fact(FactKind::Procedure, FactStatus::Assumed, 2, now);
        let far = fact(FactKind::Procedure, FactStatus::Assumed, 3, now);
        let (a, b) = pair(0.85);
        let (_, c) = pair(0.79);
        let facts = scope(vec![(head.clone(), a), (near.clone(), b), (far, c)]);
        let found = clusters(&facts, &facts.live, &config, 20);
        assert_eq!(found, vec![vec![head, near]]);
        assert!(clusters(&facts, &facts.live, &config, 0).is_empty());
    }

    #[test]
    fn merges_stay_inside_their_cluster() {
        let now = Utc::now();
        let a = fact(FactKind::Procedure, FactStatus::Assumed, 1, now);
        let b = fact(FactKind::Procedure, FactStatus::Assumed, 2, now);
        let known = fact(FactKind::Procedure, FactStatus::Known, 3, now);
        let cluster = vec![a.clone(), b.clone()];
        let live = vec![a.clone(), b.clone(), known.clone()];
        let answer = |supersedes: Vec<&str>| MergeAnswer {
            merged_text: "merged".into(),
            source_ids: vec![a.id.clone(), b.id.clone(), known.id.clone()],
            supersedes: supersedes.into_iter().map(str::to_string).collect(),
        };
        let planned = plan_merge(
            &cluster,
            &answer(vec![&a.id, &b.id, &known.id]),
            Some("merged".into()),
            &live,
        )
        .unwrap();
        assert_eq!(planned.supersedes(), [a.id.clone(), b.id.clone()], "never the known fact");
        assert!(plan_merge(&cluster, &answer(vec![]), Some("merged".into()), &live).is_none());
        assert!(plan_merge(&cluster, &answer(vec![&a.id]), None, &live).is_none(), "refused text");
        let same = plan_merge(&cluster, &answer(vec![&a.id, &b.id]), Some(a.text.clone()), &live);
        assert_eq!(
            same,
            Some(PlannedMerge::Into { into: a.id.clone(), supersedes: vec![b.id.clone()] })
        );
        assert!(
            plan_merge(&cluster, &answer(vec![&a.id]), Some(known.text.clone()), &live).is_none(),
            "a text a live fact already holds"
        );
    }

    #[test]
    fn the_loss_cap_is_a_share_of_live_facts_and_at_least_one() {
        let caps: Vec<usize> =
            [0, 2, 3, 4, 7, 8, 10, 100].into_iter().map(|live| loss_cap(live, 0.25)).collect();
        assert_eq!(caps, vec![1, 1, 1, 1, 1, 2, 2, 25]);
        assert_eq!((loss_cap(10, 0.7), loss_cap(10, 0.0), loss_cap(10, 1.0)), (7, 1, 10));
    }
}

#[cfg(feature = "runner")]
mod tick {
    use std::sync::Arc;

    use adk_core::audit_log::AuditActor;
    use adk_core::{BudgetTracker, Result, RunBudget};
    use chrono::{DateTime, Utc};
    use serde_json::json;

    use super::{
        ChangeReason, ConsolidationRecord, ConsolidationRequest, FactChange, PlannedMerge,
        ScopeFacts, SessionDistillation, clusters, loss_cap, pass_one, plan_merge,
    };
    use crate::audit::HarnessAudit;
    use crate::facts::runtime::{FactsRuntime, harness_subject};
    use crate::facts::types::{
        FactFilter, FactScope, FactSource, FactStatus, MemoryFact, Provenance, new_fact_id,
    };
    use crate::facts::writer::TickWrites;

    /// One scope's facts, pass-1 changes, pass-2 clusters and planned merges.
    struct ScopePlan {
        facts: ScopeFacts,
        changes: Vec<FactChange>,
        clusters: Vec<Vec<MemoryFact>>,
        merges: Vec<PlannedMerge>,
    }

    fn not_live(error: &adk_core::AdkError) -> bool {
        error.code == "harness.memory.fact_not_live"
    }

    impl FactsRuntime {
        /// The public distillation entry (spec 3.2 §10 HK2) outside a tick.
        pub(crate) async fn distill_sessions(
            &self,
            app: &str,
            user: &str,
            sessions: &[String],
            now: DateTime<Utc>,
            budget: Option<RunBudget>,
        ) -> Result<SessionDistillation> {
            let _one_at_a_time = self.distilling.lock().await;
            let parent = budget.map(|budget| Arc::new(BudgetTracker::new(budget)));
            self.distill_unreached(app, user, sessions, now, &TickWrites::new(None), parent).await
        }

        /// The live facts of exactly `scope` and their vectors; `Assumed` facts without a
        /// vector the embedder's length are embedded first, when there is an embedder.
        async fn scope_facts(&self, scope: &FactScope) -> Result<ScopeFacts> {
            let live = self
                .store
                .list(FactFilter { live_only: true, ..FactFilter::exactly(scope) })
                .await?;
            let mut vectors: std::collections::HashMap<String, Vec<f32>> =
                self.store.embeddings(scope).await?.into_iter().collect();
            if let Some(embedder) = &self.embedder {
                let dimensions = embedder.dimensions();
                let missing: Vec<&MemoryFact> = live
                    .iter()
                    .filter(|fact| fact.status == FactStatus::Assumed)
                    .filter(|fact| {
                        vectors.get(&fact.id).is_none_or(|vector| {
                            vector.is_empty() || (dimensions > 0 && vector.len() != dimensions)
                        })
                    })
                    .collect();
                if !missing.is_empty() {
                    let texts: Vec<String> = missing.iter().map(|fact| fact.text.clone()).collect();
                    match embedder.embed(&texts).await {
                        Ok(embedded) => {
                            for (fact, vector) in missing.into_iter().zip(embedded) {
                                if let Err(error) =
                                    self.store.put_embedding(&fact.id, vector.clone()).await
                                {
                                    tracing::debug!(error = %error, "could not store a fact embedding");
                                }
                                vectors.insert(fact.id.clone(), vector);
                            }
                        }
                        Err(error) => tracing::warn!(
                            error = %error,
                            "fact embedding failed; consolidation compares the stored vectors only"
                        ),
                    }
                }
            }
            Ok(ScopeFacts { live, vectors })
        }

        /// The audit of a tick's writes: the harness's trail outside a run.
        fn tick_audit(&self, app: &str, user: &str, tick_id: &str) -> Option<HarnessAudit> {
            self.outside_trail().map(|trail| {
                HarnessAudit::new(
                    trail,
                    AuditActor::for_invocation(
                        app,
                        &format!("{}-consolidator", self.root_agent),
                        user,
                        tick_id,
                    ),
                    String::new(),
                )
            })
        }

        /// One consolidation tick over `user`'s facts (spec 3.2 §7.5, §10 HK2): the
        /// sessions the per-session distiller never reached are distilled with quote
        /// promotion off, then the two passes run over the project's and the user-wide
        /// scope, all or nothing under the loss cap. The record is written before any
        /// change and again after, and one `harness.memory.consolidated` record is audited.
        pub(crate) async fn consolidate(
            &self,
            app: &str,
            user: &str,
            request: ConsolidationRequest,
        ) -> Result<ConsolidationRecord> {
            let config = request.config.clone();
            let now = request.at.unwrap_or_else(Utc::now);
            let tick_id = request
                .tick_id
                .clone()
                .filter(|id| !id.trim().is_empty())
                .unwrap_or_else(new_fact_id);
            let partition = request.partition.clone().unwrap_or_else(|| "default".to_string());
            let _one_at_a_time = self.distilling.lock().await;
            let mut record = ConsolidationRecord::new(&tick_id, app, user, &partition, now);
            let project = self.scope(app, user);
            record.scopes = vec![project.clone(), project.user_wide()];
            // Write-ahead; a store that cannot keep the record refuses here, before any change.
            self.store.put_consolidation(&record).await?;

            let budget = request.budget.clone().unwrap_or_default();
            let tokens = budget
                .max_total_tokens
                .map_or(config.max_tokens_per_tick, |cap| cap.min(config.max_tokens_per_tick));
            let tracker = Arc::new(BudgetTracker::new(budget.max_total_tokens(tokens)));

            // Distillation of what the per-session distiller never reached (HK2).
            let writes = TickWrites::new(Some(tick_id.clone()));
            let distilled = self
                .distill_unreached(
                    app,
                    user,
                    &request.sessions,
                    now,
                    &writes,
                    Some(tracker.clone()),
                )
                .await?;
            record.sessions = distilled.sessions;
            record.distilled = distilled.stored;
            record.changes = distilled.changes;
            record.beside_known = distilled.beside_known;

            // Pass 1, and the clusters pass 2 may merge.
            let mut plans = Vec::with_capacity(record.scopes.len());
            let mut cluster_room = config.max_clusters_per_tick;
            for scope in &record.scopes {
                let facts = self.scope_facts(scope).await?;
                let (changes, survivors) = pass_one(&facts, &config, now);
                let clusters = clusters(&facts, &survivors, &config, cluster_room);
                cluster_room = cluster_room.saturating_sub(clusters.len());
                plans.push(ScopePlan { facts, changes, clusters, merges: Vec::new() });
            }

            // Pass 2: one bounded model call per cluster, inside the tick's budget.
            'pass_two: for plan in &mut plans {
                for cluster in std::mem::take(&mut plan.clusters) {
                    if tracker.check().is_err() {
                        break 'pass_two;
                    }
                    match self.run_merge(user, &cluster, Some(tracker.clone())).await {
                        Ok((answer, _)) => {
                            let text = Some(answer.merged_text.trim())
                                .filter(|text| !text.is_empty())
                                .and_then(|text| {
                                    crate::facts::redact::redact_fact_text(
                                        text,
                                        self.config.max_fact_chars,
                                    )
                                    .ok()
                                });
                            plan.merges.extend(plan_merge(
                                &cluster,
                                &answer,
                                text,
                                &plan.facts.live,
                            ));
                        }
                        Err(error) => {
                            tracing::warn!(error = %error, "a consolidation merge failed; its facts stay as they are");
                            if tracker.check().is_err() {
                                break 'pass_two;
                            }
                        }
                    }
                }
            }

            // The loss cap: all or nothing.
            for plan in &plans {
                let live = plan.facts.live.len();
                let changed = plan.changes.len()
                    + plan.merges.iter().map(|merge| merge.supersedes().len()).sum::<usize>();
                let cap = loss_cap(live, config.max_loss_fraction);
                if changed > cap {
                    record.refused = Some(format!(
                        "the passes would change the status of {changed} of the {live} live \
                         facts of one scope, more than the {cap} max_loss_fraction {} allows",
                        config.max_loss_fraction
                    ));
                    break;
                }
            }

            let audit = self.tick_audit(app, user, &tick_id);
            if record.refused.is_none() {
                self.apply(&mut record, plans, audit.as_ref(), now).await?;
            }
            let usage = tracker.usage();
            record.model_calls = usage.model_calls;
            record.total_tokens = usage.total_tokens;
            record.cost_micro_usd = usage.cost_micro_usd;
            record.finished_at = Some(Utc::now());
            self.store.put_consolidation(&record).await?;
            if let Some(audit) = &audit {
                let ids = |changes: Vec<&FactChange>| {
                    changes.into_iter().map(|change| change.fact.clone()).collect::<Vec<_>>()
                };
                let superseded = ids(record.superseded().collect());
                let forgotten = ids(record.forgotten().collect());
                audit
                    .record(
                        "harness.memory.consolidated",
                        json!({
                            "tickId": record.tick_id,
                            "partition": record.partition,
                            "scope": project.project,
                            "superseded": superseded,
                            "forgotten": forgotten,
                            "merged": record.merged,
                            "distilled": record.distilled,
                            "besideKnown": record
                                .beside_known
                                .iter()
                                .map(|beside| json!({ "fact": beside.fact, "known": beside.known }))
                                .collect::<Vec<_>>(),
                            "sessions": record.sessions.len(),
                            "counts": {
                                "superseded": superseded.len(),
                                "forgotten": forgotten.len(),
                                "merged": record.merged.len(),
                                "distilled": record.distilled.len(),
                            },
                            "refused": record.refused.is_some(),
                            "modelCalls": record.model_calls,
                            "tokens": record.total_tokens,
                            "cost": record.cost_micro_usd,
                        }),
                    )
                    .await?;
            }
            tracing::info!(
                memory.tick = %record.tick_id,
                memory.superseded = record.superseded().count(),
                memory.forgotten = record.forgotten().count(),
                memory.merged = record.merged.len(),
                memory.distilled = record.distilled.len(),
                memory.refused = record.refused.is_some(),
                "memory consolidated"
            );
            Ok(record)
        }

        /// Applies the passes' plans. The record lists the plan first, so `undo` reverts a
        /// tick a crash interrupted; a fact a concurrent write already changed is skipped.
        async fn apply(
            &self,
            record: &mut ConsolidationRecord,
            plans: Vec<ScopePlan>,
            audit: Option<&HarnessAudit>,
            now: DateTime<Utc>,
        ) -> Result<()> {
            let tick_id = record.tick_id.clone();
            let mut pass_one: Vec<(MemoryFact, FactChange)> = Vec::new();
            // (the new fact, or none when a source's text won; the target; what it supersedes)
            let mut merges: Vec<(Option<MemoryFact>, String, Vec<MemoryFact>)> = Vec::new();
            for plan in plans {
                let live =
                    |id: &String| plan.facts.live.iter().find(|fact| fact.id == *id).cloned();
                for change in plan.changes {
                    pass_one.extend(live(&change.fact).map(|fact| (fact, change)));
                }
                for merge in plan.merges {
                    match merge {
                        PlannedMerge::New { kind, scope, text, sources, supersedes } => {
                            let mut provenance =
                                Provenance::new(FactSource::AgentInferred, "", tick_id.clone());
                            provenance.at = now;
                            provenance.source_ids =
                                sources.iter().map(|source| source.id.clone()).collect();
                            provenance.tick = Some(tick_id.clone());
                            let mut fact =
                                MemoryFact::new(scope, kind, text, FactStatus::Assumed, provenance);
                            fact.confidence = sources
                                .iter()
                                .filter_map(|source| source.confidence)
                                .reduce(f32::max);
                            fact.uses = sources
                                .iter()
                                .fold(0u32, |uses, source| uses.saturating_add(source.uses));
                            fact.last_recalled =
                                sources.iter().filter_map(|source| source.last_recalled).max();
                            let target = fact.id.clone();
                            let superseded = supersedes.iter().filter_map(live).collect();
                            merges.push((Some(fact), target, superseded));
                        }
                        PlannedMerge::Into { into, supersedes } => {
                            merges.push((None, into, supersedes.iter().filter_map(live).collect()));
                        }
                    }
                }
            }
            let merged = |fact: &MemoryFact, target: &str| FactChange {
                fact: fact.id.clone(),
                scope: fact.scope.clone(),
                to: FactStatus::Superseded { by: target.to_string() },
                reason: ChangeReason::Merged,
            };

            // Write-ahead: the whole plan.
            let mut planned = record.clone();
            planned.changes.extend(pass_one.iter().map(|(_, change)| change.clone()));
            for (new, target, superseded) in &merges {
                planned.merged.extend(new.as_ref().map(|fact| fact.id.clone()));
                planned.changes.extend(superseded.iter().map(|fact| merged(fact, target)));
            }
            self.store.put_consolidation(&planned).await?;

            let by = harness_subject();
            let mut changes = pass_one;
            for (new, target, superseded) in merges {
                let target = match new {
                    Some(fact) => {
                        if let Some(audit) = audit {
                            audit
                                .record(
                                    "harness.memory.fact.stored",
                                    json!({
                                        "factId": fact.id,
                                        "kind": fact.kind.as_str(),
                                        "status": fact.status.label(),
                                        "source": fact.provenance.source.as_str(),
                                        "project": fact.scope.project,
                                        "digest": fact.digest,
                                        "sessionId": "",
                                        "tickId": tick_id,
                                        "sourceIds": fact.provenance.source_ids,
                                    }),
                                )
                                .await?;
                        }
                        let stored = self.store.put(fact).await?;
                        if let Err(error) = self.index.upsert(&stored).await {
                            tracing::warn!(error = %error, "could not index a fact; recall by search may miss it");
                        }
                        record.merged.push(stored.id.clone());
                        stored.id
                    }
                    None => target,
                };
                changes.extend(superseded.into_iter().map(|fact| {
                    let change = merged(&fact, &target);
                    (fact, change)
                }));
            }
            for (fact, change) in changes {
                let reason = change.reason.as_str();
                match self
                    .change_status_in(
                        audit,
                        &fact,
                        change.to.clone(),
                        &by,
                        Some((&tick_id, reason)),
                    )
                    .await
                {
                    Ok(()) => record.changes.push(change),
                    Err(error) if not_live(&error) => {}
                    Err(error) => return Err(error),
                }
            }
            Ok(())
        }
    }
}
