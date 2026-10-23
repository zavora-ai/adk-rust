//! The in-memory [`LedgerStore`] (spec 3.2 §7.10, §13 Q8, feature `proc`).

use std::collections::BTreeMap;

use async_trait::async_trait;
use tokio::sync::RwLock;

use crate::error::Result;
use crate::partition::PartitionId;
use crate::proc::{
    LedgerEntry, LedgerQuery, LedgerStore, Observation, ObservationQuery, PassState,
    order_observations,
};

/// A [`LedgerStore`] in process memory, for tests and a kernel with no durable store (spec
/// 3.2 §7.10). Its rows end with the process.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_kernel::proc::{Activity, InMemoryLedgerStore, Ledger, LedgerKey, LedgerKind, LedgerQuery, LedgerSink, LedgerStore};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_kernel::Result<()> {
/// let store = Arc::new(InMemoryLedgerStore::new());
/// let ledger = Ledger::new(store.clone(), LedgerKey::from_bytes([1; 32]));
/// ledger.record(Activity::new(LedgerKind::Wake, "kernel")).await?;
/// assert_eq!(store.list(&LedgerQuery::default()).await?.len(), 1);
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Default)]
pub struct InMemoryLedgerStore {
    rows: RwLock<Vec<LedgerEntry>>,
    observations: RwLock<BTreeMap<String, Observation>>,
    states: RwLock<BTreeMap<PartitionId, PassState>>,
}

impl InMemoryLedgerStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl LedgerStore for InMemoryLedgerStore {
    async fn append(&self, mut entry: LedgerEntry) -> Result<LedgerEntry> {
        let mut rows = self.rows.write().await;
        entry.seq = rows.last().map_or(1, |last| last.seq + 1);
        rows.push(entry.clone());
        Ok(entry)
    }

    async fn list(&self, query: &LedgerQuery) -> Result<Vec<LedgerEntry>> {
        Ok(self
            .rows
            .read()
            .await
            .iter()
            .filter(|entry| query.matches(entry))
            .take(query.limit.unwrap_or(usize::MAX))
            .cloned()
            .collect())
    }

    async fn put_observation(&self, observation: Observation) -> Result<()> {
        self.observations.write().await.insert(observation.id.clone(), observation);
        Ok(())
    }

    async fn observations(&self, query: &ObservationQuery) -> Result<Vec<Observation>> {
        let mut observations: Vec<Observation> = self
            .observations
            .read()
            .await
            .values()
            .filter(|observation| query.matches(observation))
            .cloned()
            .collect();
        order_observations(&mut observations);
        observations.truncate(query.limit.unwrap_or(usize::MAX));
        Ok(observations)
    }

    async fn pass_state(&self, partition: &PartitionId) -> Result<Option<PassState>> {
        Ok(self.states.read().await.get(partition).cloned())
    }

    async fn pass_states(&self) -> Result<Vec<PassState>> {
        Ok(self.states.read().await.values().cloned().collect())
    }

    async fn put_pass_state(&self, state: PassState) -> Result<()> {
        self.states.write().await.insert(state.partition.clone(), state);
        Ok(())
    }
}
