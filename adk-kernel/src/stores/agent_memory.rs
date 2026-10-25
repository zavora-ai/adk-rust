//! The in-memory [`AgentStore`] (spec 3.3 §5.7, §10 K7).

use std::collections::BTreeMap;

use async_trait::async_trait;
use tokio::sync::RwLock;

use crate::agents::{AgentRecord, AgentRecordState, AgentStore};
use crate::error::Result;

/// An [`AgentStore`] in process memory, for tests and a kernel with no durable store (spec
/// 3.3 §5.7). Its records end with the process.
///
/// # Example
///
/// ```rust
/// use adk_core::principal::SubjectRef;
/// use adk_kernel::agents::{AgentDefinition, AgentOrigin, AgentRecord, AgentRecordState, AgentStore};
/// use adk_kernel::stores::InMemoryAgentStore;
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_kernel::Result<()> {
/// let store = InMemoryAgentStore::new();
/// for (id, name) in [("travel", "Travel"), ("finance", "Finance")] {
///     let record = AgentRecord::new(id, AgentDefinition::new(name, "default"),
///         AgentRecordState::Active, AgentOrigin::ByHand, SubjectRef::human("alice"), chrono::Utc::now());
///     store.put_if_version(record, 0).await?;
/// }
/// let ids: Vec<_> = store.list().await?.into_iter().map(|record| record.id).collect();
/// assert_eq!(ids, ["finance", "travel"]);
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Default)]
pub struct InMemoryAgentStore {
    records: RwLock<BTreeMap<String, AgentRecord>>,
}

impl InMemoryAgentStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl AgentStore for InMemoryAgentStore {
    async fn put_if_version(&self, record: AgentRecord, expected: u64) -> Result<bool> {
        let mut records = self.records.write().await;
        let stored = records.get(&record.id).map_or(0, |stored| stored.version);
        if stored != expected {
            return Ok(false);
        }
        records.insert(record.id.clone(), record);
        Ok(true)
    }

    async fn get(&self, id: &str) -> Result<Option<AgentRecord>> {
        Ok(self.records.read().await.get(id).cloned())
    }

    async fn list(&self) -> Result<Vec<AgentRecord>> {
        Ok(self.records.read().await.values().cloned().collect())
    }

    async fn delete(&self, id: &str) -> Result<bool> {
        let mut records = self.records.write().await;
        if records.get(id).is_some_and(|record| record.state == AgentRecordState::Proposed) {
            records.remove(id);
            return Ok(true);
        }
        Ok(false)
    }
}
