//! The harness's side of the kernel's consolidation tick (spec 3.2 §7.5, §10 HK2; features
//! `kernel` and `memory`): [`HarnessConsolidator`] reports a partition's new material and
//! runs a tick through [`Harness::consolidate_memory`]. [`HarnessObservations`] is the
//! person the kernel's `/proc` observations are put to, and remembers the ones they confirm
//! (spec 3.2 §7.10).

use std::sync::Arc;

use adk_core::UserId;
use adk_core::principal::SubjectRef;
use adk_kernel::{
    AgendaItem, Consolidator, ObservationAudience, PartitionId, PartitionMode, TickMaterial,
    TickOutcome, TickProposal, TickRun, TickSpend,
};
use adk_session::{ListRequest, SessionService};
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use super::consolidate::{ConsolidationConfig, ConsolidationRecord, ConsolidationRequest};
use super::runtime::FactsRuntime;
use super::types::{FactFilter, FactKind, FactScope};
use crate::harness::Harness;
use crate::mode::HarnessMode;

/// Which sessions a [`HarnessConsolidator`] distils: true keeps one, read from its state.
pub type SessionStateFilter = Arc<dyn Fn(&dyn adk_session::State) -> bool + Send + Sync>;

/// The [`Consolidator`] of one user of a harness (spec 3.2 §7.5, §10 HK2), registered with
/// `KernelBuilder::consolidator`.
///
/// | Material | Read from |
/// |---|---|
/// | The last tick and what today's ticks spent | the user's consolidation records in the fact store |
/// | Distilled sessions since the last tick | the store's distillation cursor: sessions read past the last tick |
/// | New facts since the last tick | the project's and the user-wide facts, the ticks' own aside |
/// | Latest activity and the sessions to distil | the user's sessions updated since the last tick, kept by the session filter |
/// | Mode | the harness's mode, as the partition's |
///
/// A tick runs [`Harness::consolidate_memory`] with the tick's thresholds and budget, and
/// proposes one agenda item when it stored assumptions beside known facts, so a person
/// confirms or forgets them (K-I22, K-I23).
///
/// # Example
///
/// ```rust,ignore
/// let consolidator = harness
///     .consolidator(&user)?
///     .with_session_filter(Arc::new(|state| state.get("harness:workspace").is_some()));
/// let kernel = harness
///     .kernel()
///     .config(KernelConfig::default().with_proactivity(
///         ProactivityPolicy::default().with_consolidation(ConsolidationTick::default()),
///     ))
///     .consolidator(PartitionId::default(), Arc::new(consolidator))
///     .build()?;
/// ```
#[derive(Clone)]
pub struct HarnessConsolidator {
    facts: Arc<FactsRuntime>,
    sessions: Arc<dyn SessionService>,
    app: String,
    user: String,
    filter: Option<SessionStateFilter>,
}

impl std::fmt::Debug for HarnessConsolidator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HarnessConsolidator")
            .field("app", &self.app)
            .field("filter", &self.filter.is_some())
            .finish_non_exhaustive()
    }
}

impl HarnessConsolidator {
    /// Distils only the sessions whose state `filter` keeps; the CLI keeps the sessions of
    /// its workspace.
    #[must_use]
    pub fn with_session_filter(mut self, filter: SessionStateFilter) -> Self {
        self.filter = Some(filter);
        self
    }

    /// The user's sessions updated after `since`, kept by the filter, with their last update.
    async fn sessions_since(
        &self,
        since: Option<DateTime<Utc>>,
    ) -> adk_core::Result<Vec<(String, DateTime<Utc>)>> {
        let listed = self
            .sessions
            .list(ListRequest {
                app_name: self.app.clone(),
                user_id: self.user.clone(),
                limit: None,
                offset: None,
            })
            .await?;
        let mut out = Vec::new();
        for session in listed {
            let updated = session.last_update_time();
            if since.is_some_and(|since| updated <= since) {
                continue;
            }
            if let Some(filter) = &self.filter {
                let full = self
                    .sessions
                    .get(adk_session::GetRequest {
                        app_name: self.app.clone(),
                        user_id: self.user.clone(),
                        session_id: session.id().to_string(),
                        num_recent_events: Some(1),
                        after: None,
                    })
                    .await?;
                if !filter(full.state()) {
                    continue;
                }
            }
            out.push((session.id().to_string(), updated));
        }
        Ok(out)
    }
}

fn partition_mode(mode: HarnessMode) -> PartitionMode {
    match mode {
        HarnessMode::ReadOnly => PartitionMode::ReadOnly,
        HarnessMode::Plan => PartitionMode::Plan,
        HarnessMode::Ask => PartitionMode::Ask,
        HarnessMode::Auto => PartitionMode::Auto,
    }
}

/// The agenda item for the assumptions a tick stored beside known facts: ids only.
fn beside_known_proposal(record: &ConsolidationRecord) -> Option<TickProposal> {
    if record.beside_known.is_empty() {
        return None;
    }
    let pairs: Vec<String> = record
        .beside_known
        .iter()
        .map(|beside| format!("{} beside {}", beside.fact, beside.known))
        .collect();
    Some(TickProposal::new(
        format!(
            "Confirm or forget {} remembered assumption(s) that disagree with known facts",
            pairs.len()
        ),
        format!(
            "Consolidation tick {} stored assumptions beside the known facts they would \
             replace: {}. `adk-rust memory accept ID` confirms one; `adk-rust memory forget ID` \
             drops it.",
            record.tick_id,
            pairs.join("; ")
        ),
    ))
}

#[async_trait]
impl Consolidator for HarnessConsolidator {
    async fn material(
        &self,
        _partition: &PartitionId,
        now: DateTime<Utc>,
    ) -> adk_kernel::Result<TickMaterial> {
        let records = self.facts.store.consolidations(&self.app, &self.user).await?;
        let last_tick = records.iter().map(|record| record.at).max();
        let today = now.date_naive();
        let spent = records.iter().filter(|record| record.at.date_naive() == today).fold(
            TickSpend::default(),
            |spent, record| {
                TickSpend::new(
                    spent.cost_micro_usd.saturating_add(record.cost_micro_usd),
                    spent.total_tokens.saturating_add(record.total_tokens),
                    spent.model_calls.saturating_add(record.model_calls),
                )
            },
        );
        let new_sessions = self
            .facts
            .store
            .distilled_sessions(&self.app, &self.user)
            .await?
            .into_iter()
            .filter(|(_, upto)| last_tick.is_none_or(|last| *upto > last))
            .count();
        let scope = self.facts.scope(&self.app, &self.user);
        let readable = if self.facts.confined {
            FactFilter::exactly(&scope)
        } else {
            FactFilter::recall(&scope)
        };
        let new_facts = self
            .facts
            .store
            .list(FactFilter { live_only: false, since: last_tick, ..readable })
            .await?
            .into_iter()
            .filter(|fact| fact.provenance.tick.is_none())
            .filter(|fact| last_tick.is_none_or(|last| fact.valid_from > last))
            .count();
        let sessions = self.sessions_since(last_tick).await?;
        let mut material = TickMaterial::new(&self.user, partition_mode(self.facts.default_mode))
            .with_new_material(new_sessions, new_facts)
            .with_sessions(sessions.iter().map(|(id, _)| id.clone()))
            .with_spent_today(spent);
        if let Some(last) = last_tick {
            material = material.with_last_tick(last);
        }
        if let Some(active) = sessions.iter().map(|(_, at)| *at).max() {
            material = material.with_last_activity(active);
        }
        Ok(material)
    }

    async fn consolidate(&self, run: TickRun) -> adk_kernel::Result<TickOutcome> {
        let tick = &run.tick;
        let config = ConsolidationConfig {
            dedup_score: tick.dedup_score,
            expire_confidence: tick.expire_confidence,
            assumed_ttl: tick.assumed_ttl,
            max_clusters_per_tick: tick.max_clusters_per_tick,
            max_loss_fraction: tick.max_loss_fraction,
            max_tokens_per_tick: tick.max_tokens_per_tick,
        };
        let request = ConsolidationRequest::new()
            .with_tick_id(&run.tick_id)
            .with_partition(run.partition.as_str())
            .with_sessions(run.sessions.clone())
            .with_config(config)
            .with_budget(run.budget.clone())
            .at(run.at);
        let record = self.facts.consolidate(&self.app, &self.user, request).await?;
        let mut outcome = TickOutcome::new(&record.tick_id)
            .with_counts(
                record.superseded().count(),
                record.forgotten().count(),
                record.merged.len(),
                record.distilled.len(),
            )
            .with_spend(TickSpend::new(
                record.cost_micro_usd,
                record.total_tokens,
                record.model_calls,
            ));
        if let Some(refused) = &record.refused {
            outcome = outcome.with_refused(refused.clone());
        }
        if let Some(proposal) = beside_known_proposal(&record) {
            outcome = outcome.with_proposal(proposal);
        }
        Ok(outcome)
    }
}

impl Harness {
    /// The kernel's [`Consolidator`] for `user`'s memory in this harness (spec 3.2 §7.5,
    /// §10 HK2): register it with `KernelBuilder::consolidator` and turn the tick on with
    /// `ProactivityPolicy::consolidation`. The tick is off unless both are set: it is on in
    /// the CLI's Ask and Auto partitions, off in the library and on servers.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let kernel = harness
    ///     .kernel()
    ///     .consolidator(adk_kernel::PartitionId::default(), Arc::new(harness.consolidator(&user)?))
    ///     .build()?;
    /// ```
    ///
    /// # Errors
    ///
    /// Returns a configuration error when long-term memory is off.
    pub fn consolidator(&self, user: &UserId) -> adk_core::Result<HarnessConsolidator> {
        let facts = self.agent().runtime().toolset().facts().cloned().ok_or_else(|| {
            adk_core::AdkError::config(
                "the consolidation tick works on long-term memory, which is off for this \
                 harness; turn it on with HarnessAgentBuilder::long_term_memory",
            )
        })?;
        Ok(HarnessConsolidator {
            facts,
            sessions: self.sessions().clone(),
            app: self.app_name().to_string(),
            user: user.to_string(),
            filter: None,
        })
    }
}

/// The [`ObservationAudience`] of one user of a harness (spec 3.2 §7.10), registered with
/// `KernelBuilder::observation_audience`: the kernel puts the partition's observations to
/// this user, in the harness's mode, and a person's confirmation becomes a user-wide
/// `Preference` fact, `Known` with source `UserConfirmed` (spec 3.1 §7.15). The same text
/// already remembered is promoted to `Known` rather than stored twice; a rejected
/// observation records nothing, so any related fact stays as it was.
///
/// A partition's harness is the audience of its own partition only (spec 3.2 §7.8,
/// K-I15): it answers no one for another partition's observations, refuses to confirm one,
/// and remembers a confirmed observation in the partition's `memory_scope`, where no other
/// partition reads it.
///
/// # Example
///
/// ```rust,ignore
/// let kernel = harness
///     .kernel()
///     .observation_audience(PartitionId::default(), Arc::new(harness.observation_audience(&user)?))
///     .build()?;
/// ```
#[derive(Clone)]
pub struct HarnessObservations {
    facts: Arc<FactsRuntime>,
    app: String,
    user: String,
    /// The partition of a partition's harness.
    partition: Option<PartitionId>,
}

impl HarnessObservations {
    /// Whether `partition`'s observations are this audience's: any partition's for a
    /// harness without one, else its own only.
    fn serves(&self, partition: &PartitionId) -> bool {
        self.partition.as_ref().is_none_or(|own| own == partition)
    }
}

impl std::fmt::Debug for HarnessObservations {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HarnessObservations").field("app", &self.app).finish_non_exhaustive()
    }
}

#[async_trait]
impl ObservationAudience for HarnessObservations {
    async fn audience(
        &self,
        partition: &PartitionId,
    ) -> adk_kernel::Result<Option<(SubjectRef, PartitionMode)>> {
        if !self.serves(partition) {
            return Ok(None);
        }
        Ok(Some((SubjectRef::human(&self.user), partition_mode(self.facts.default_mode))))
    }

    async fn confirm(&self, item: &AgendaItem, by: &SubjectRef) -> adk_kernel::Result<String> {
        if !self.serves(&item.partition) {
            return Err(adk_kernel::KernelError::Config {
                reason: format!(
                    "agenda item {} is an observation of partition `{}`, which this harness \
                     does not run; only its own partition's harness remembers it (spec 3.2 \
                     K-I15)",
                    item.id, item.partition
                ),
            });
        }
        if item.subject.id != self.user {
            return Err(adk_kernel::KernelError::Config {
                reason: format!(
                    "agenda item {} asks {}, but this harness remembers for {}; register the \
                     audience of that user",
                    item.id, item.subject.id, self.user
                ),
            });
        }
        // User-wide, or a partition's memory scope (spec 3.2 §7.8).
        let scope = FactScope::new(
            self.app.clone(),
            self.user.clone(),
            self.facts.project_of(FactKind::Preference),
        );
        let fact = self.facts.confirm(by, scope, FactKind::Preference, &item.rationale).await?;
        Ok(fact.id)
    }
}

impl Harness {
    /// The kernel's [`ObservationAudience`] for `user` in this harness (spec 3.2 §7.10):
    /// register it with `KernelBuilder::observation_audience` so the daily baseline pass
    /// puts its observations to `user` as agenda questions and a confirmed one is
    /// remembered as known.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let audience = harness.observation_audience(&user)?;
    /// let kernel = harness
    ///     .kernel()
    ///     .observation_audience(adk_kernel::PartitionId::default(), Arc::new(audience))
    ///     .build()?;
    /// ```
    ///
    /// # Errors
    ///
    /// Returns a configuration error when long-term memory is off.
    pub fn observation_audience(&self, user: &UserId) -> adk_core::Result<HarnessObservations> {
        let facts = self.agent().runtime().toolset().facts().cloned().ok_or_else(|| {
            adk_core::AdkError::config(
                "a confirmed observation is remembered in long-term memory, which is off for \
                 this harness; turn it on with HarnessAgentBuilder::long_term_memory",
            )
        })?;
        Ok(HarnessObservations {
            facts,
            app: self.app_name().to_string(),
            user: user.to_string(),
            partition: self.partition().map(|partition| partition.id.clone()),
        })
    }
}
