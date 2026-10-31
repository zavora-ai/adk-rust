//! The in-memory [`ProcessStore`] (spec 3.2 §7.3, §13 Q8).

use std::collections::BTreeMap;

use async_trait::async_trait;
use chrono::NaiveDate;
use tokio::sync::RwLock;

use super::{ProcessFilter, ProcessStore, SpendScope, UsageTotals};
use crate::bus::SpendDelta;
use crate::error::{KernelError, Result};
use crate::partition::PartitionId;
use crate::process::ProcessMeta;
use crate::sched::QueueClass;
use crate::wake::WakeSet;

/// A [`ProcessStore`] in process memory, for tests and a kernel with no durable store
/// (spec 3.2 §7.3). Its rows end with the process.
///
/// Rows are kept in `pid` order, so [`list`](ProcessStore::list) orders as the durable
/// backends do.
///
/// # Example
///
/// ```rust
/// use adk_kernel::{InMemoryProcessStore, ProcessFilter, ProcessKind, ProcessMeta, ProcessStore, QueueClass};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_kernel::Result<()> {
/// let store = InMemoryProcessStore::new();
/// store.put(ProcessMeta::new("run-2", QueueClass::Background, ProcessKind::Goal)).await?;
/// store.put(ProcessMeta::new("run-1", QueueClass::Interactive, ProcessKind::Interactive)).await?;
/// let pids: Vec<_> = store
///     .list(&ProcessFilter::default())
///     .await?
///     .into_iter()
///     .map(|meta| meta.pid)
///     .collect();
/// assert_eq!(pids, ["run-1", "run-2"]);
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Default)]
pub struct InMemoryProcessStore {
    rows: RwLock<BTreeMap<String, ProcessMeta>>,
    /// Usage per process, partition and UTC day.
    spend: RwLock<BTreeMap<(String, PartitionId, NaiveDate), UsageTotals>>,
}

impl InMemoryProcessStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

/// The class a durable backend reads back for `queue`, which it stores by name.
fn stored_queue(queue: QueueClass) -> QueueClass {
    match queue {
        QueueClass::Named(name) => QueueClass::from_name(&name),
        other => other,
    }
}

#[async_trait]
impl ProcessStore for InMemoryProcessStore {
    async fn put(&self, mut meta: ProcessMeta) -> Result<()> {
        meta.queue = stored_queue(meta.queue);
        self.rows.write().await.insert(meta.pid.clone(), meta);
        Ok(())
    }

    async fn get(&self, pid: &str) -> Result<Option<ProcessMeta>> {
        Ok(self.rows.read().await.get(pid).cloned())
    }

    async fn list(&self, filter: &ProcessFilter) -> Result<Vec<ProcessMeta>> {
        let filter =
            ProcessFilter { queue: filter.queue.clone().map(stored_queue), ..filter.clone() };
        Ok(self
            .rows
            .read()
            .await
            .values()
            .filter(|meta| filter.matches(meta))
            .take(filter.limit.unwrap_or(usize::MAX))
            .cloned()
            .collect())
    }

    async fn set_wake(&self, pid: &str, wake: Option<WakeSet>) -> Result<()> {
        let mut rows = self.rows.write().await;
        let row = rows.get_mut(pid).ok_or_else(|| KernelError::NotFound { pid: pid.into() })?;
        row.wake = wake;
        Ok(())
    }

    async fn set_queue(&self, pid: &str, queue: QueueClass) -> Result<()> {
        let mut rows = self.rows.write().await;
        let row = rows.get_mut(pid).ok_or_else(|| KernelError::NotFound { pid: pid.into() })?;
        row.queue = stored_queue(queue);
        Ok(())
    }

    async fn delete(&self, pid: &str) -> Result<bool> {
        Ok(self.rows.write().await.remove(pid).is_some())
    }

    async fn set_suspend_requested(&self, pid: &str, requested: bool) -> Result<()> {
        let mut rows = self.rows.write().await;
        let row = rows.get_mut(pid).ok_or_else(|| KernelError::NotFound { pid: pid.into() })?;
        row.suspend_requested = requested;
        Ok(())
    }

    async fn add_spend(
        &self,
        pid: &str,
        partition: &PartitionId,
        day: NaiveDate,
        delta: SpendDelta,
    ) -> Result<()> {
        let mut spend = self.spend.write().await;
        spend.entry((pid.to_string(), partition.clone(), day)).or_default().add(&delta);
        Ok(())
    }

    async fn spend(
        &self,
        scope: &SpendScope,
        from: NaiveDate,
        to_inclusive: NaiveDate,
    ) -> Result<u64> {
        Ok(self.usage(scope, from, to_inclusive).await?.micro_usd)
    }

    async fn usage(
        &self,
        scope: &SpendScope,
        from: NaiveDate,
        to_inclusive: NaiveDate,
    ) -> Result<UsageTotals> {
        let mut totals = UsageTotals::default();
        for ((pid, partition, day), day_totals) in self.spend.read().await.iter() {
            let in_scope = match scope {
                SpendScope::Process(process) => process == pid,
                SpendScope::Partition(scoped) => scoped == partition,
            };
            if in_scope && (from..=to_inclusive).contains(day) {
                totals.merge(day_totals);
            }
        }
        Ok(totals)
    }
}
