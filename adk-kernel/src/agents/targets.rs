//! The kernel's live target table (spec 3.3 §5.7, §10 K7, S4): the targets the host
//! registered at build, and the agents a person added, hosted while the kernel runs.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, RwLock};

use adk_core::{AdkError, ErrorCategory, ErrorComponent};
use adk_runner::Runner;
use adk_server::background::AgentTable;

use crate::agents::record::{AgentRecord, AgentRecordState, check_agent_id};
use crate::agents::store::AgentStore;
use crate::error::{AGENT_REFUSED_CODE, KernelError, Result};
use crate::partition::PartitionId;
use crate::process::ProgressSource;

/// An added agent this replica hosts.
#[derive(Clone)]
pub(crate) struct Added {
    /// Its area.
    pub(crate) area: PartitionId,
    /// Its harness's progress source, attached when it was hosted.
    pub(crate) progress: Option<Arc<dyn ProgressSource>>,
    /// The version of the record it was built from.
    pub(crate) version: u64,
    /// Its own daily spending limit in micro-USD.
    pub(crate) daily_limit: Option<u64>,
}

/// Every target the kernel serves: the partition agents the host bound at build, and the
/// added agents, whose runners live in the S4 [`AgentTable`] every worker reads.
pub(crate) struct Targets {
    statics: BTreeMap<String, PartitionId>,
    pub(crate) table: AgentTable,
    store: Option<Arc<dyn AgentStore>>,
    // Std locks: read from synchronous paths, never held across an await.
    added: RwLock<BTreeMap<String, Added>>,
    paused: RwLock<BTreeSet<String>>,
}

impl std::fmt::Debug for Targets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Targets")
            .field("statics", &self.statics)
            .field("table", &self.table)
            .finish_non_exhaustive()
    }
}

impl Targets {
    /// The partition agents `statics` bound at build, and the agents `store` keeps.
    pub(crate) fn new(
        statics: BTreeMap<String, PartitionId>,
        store: Option<Arc<dyn AgentStore>>,
    ) -> Self {
        Self {
            statics,
            table: AgentTable::new(),
            store,
            added: RwLock::default(),
            paused: RwLock::default(),
        }
    }

    /// The partition agents the host bound at build (spec 3.2 §7.8): never an added agent,
    /// so a follow-up without a proposing run never goes to one.
    pub(crate) fn statics(&self) -> &BTreeMap<String, PartitionId> {
        &self.statics
    }

    /// Whether any target is bound to a partition.
    pub(crate) fn binds_partitions(&self) -> bool {
        !self.statics.is_empty() || !self.read_added().is_empty()
    }

    /// The partition `target` is bound to: a partition agent's, or an added agent's area.
    pub(crate) fn partition_of(&self, target: &str) -> Option<PartitionId> {
        self.statics
            .get(target)
            .cloned()
            .or_else(|| self.read_added().get(target).map(|added| added.area.clone()))
    }

    /// The progress source of an added agent's processes.
    pub(crate) fn progress_of(&self, target: &str) -> Option<Arc<dyn ProgressSource>> {
        self.read_added().get(target).and_then(|added| added.progress.clone())
    }

    /// The daily spending limit of the added agent `target`, when it has one.
    pub(crate) fn daily_limit(&self, target: &str) -> Option<u64> {
        self.read_added().get(target).and_then(|added| added.daily_limit)
    }

    /// The added agent hosted as `target` here, if any.
    pub(crate) fn hosted(&self, target: &str) -> Option<Added> {
        self.read_added().get(target).cloned()
    }

    /// The added agents hosted here, sorted.
    pub(crate) fn hosted_ids(&self) -> Vec<String> {
        self.read_added().keys().cloned().collect()
    }

    /// Hosts `runner` as the added agent `id`, replacing what it had; a claim already
    /// running finishes on the runner it started on.
    pub(crate) fn host(&self, id: &str, runner: Arc<Runner>, added: Added) {
        self.table.expect(id);
        self.table.insert(id, runner);
        self.added.write().unwrap_or_else(|e| e.into_inner()).insert(id.to_string(), added);
    }

    /// Marks `id` as a target the kernel serves before it is built, so a claim of it is
    /// handed back rather than failed.
    pub(crate) fn expect(&self, id: &str) {
        self.table.expect(id);
    }

    /// Stops serving the added agent `id`: a claim of it fails from now on.
    pub(crate) fn retire(&self, id: &str) {
        self.table.remove(id);
        self.table.forget(id);
        self.added.write().unwrap_or_else(|e| e.into_inner()).remove(id);
        self.paused.write().unwrap_or_else(|e| e.into_inner()).remove(id);
    }

    /// Marks the added agent `id` paused or not on this replica.
    pub(crate) fn set_paused(&self, id: &str, paused: bool) {
        let mut set = self.paused.write().unwrap_or_else(|e| e.into_inner());
        if paused {
            set.insert(id.to_string());
        } else {
            set.remove(id);
        }
    }

    /// Whether `target` is an added agent a person paused.
    pub(crate) fn is_paused(&self, target: &str) -> bool {
        self.paused.read().unwrap_or_else(|e| e.into_inner()).contains(target)
    }

    /// The record of the added agent `target`, read from the store; `None` for a target
    /// the host registered, an id no added agent can have, or a kernel without a store.
    ///
    /// # Errors
    ///
    /// Returns the store's error.
    pub(crate) async fn record(&self, target: &str) -> Result<Option<AgentRecord>> {
        let Some(store) = &self.store else { return Ok(None) };
        if self.statics.contains_key(target) || check_agent_id(target).is_err() {
            return Ok(None);
        }
        store.get(target).await
    }

    /// The added agent `target` a new process may run on, or why not: it is paused, being
    /// removed, removed, or never added (spec 3.3 §5.7). `None` for any other target.
    ///
    /// # Errors
    ///
    /// Returns a `kernel.agent.refused` error naming the agent, or the store's error.
    pub(crate) async fn admit_spawn(&self, target: &str) -> Result<Option<AgentRecord>> {
        let Some(record) = self.record(target).await? else { return Ok(None) };
        let name = &record.display_name;
        let refusal = match record.state {
            AgentRecordState::Active => return Ok(Some(record)),
            AgentRecordState::Paused => {
                format!("{name} is paused; resume it in its settings to give it work")
            }
            AgentRecordState::Removing | AgentRecordState::Removed => {
                format!("{name} was removed, so it takes no more work")
            }
            AgentRecordState::Proposed | AgentRecordState::Refused => {
                format!("{name} was not added, so it takes no work")
            }
        };
        Err(KernelError::Core(AdkError::new(
            ErrorComponent::Server,
            ErrorCategory::Conflict,
            AGENT_REFUSED_CODE,
            refusal,
        )))
    }

    fn read_added(&self) -> std::sync::RwLockReadGuard<'_, BTreeMap<String, Added>> {
        self.added.read().unwrap_or_else(|e| e.into_inner())
    }
}
