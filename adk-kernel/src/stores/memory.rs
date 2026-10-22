//! The in-memory [`ProcessStore`] (spec 3.2 §7.3, §13 Q8).

use std::collections::BTreeMap;

use async_trait::async_trait;
use chrono::NaiveDate;
use tokio::sync::RwLock;

use super::{ProcessFilter, ProcessStore, SpendScope};
use crate::bus::SpendDelta;
use crate::error::{KernelError, Result};
use crate::partition::PartitionId;
use crate::process::ProcessMeta;
use crate::sched::QueueClass;
use crate::wake::WakeSet;

/// A [`ProcessStore`] in process memory, for tests and a kernel with no durable store.
/// Its rows end with the process.
///
/// # Example
///
/// ```rust
/// use adk_kernel::InMemoryProcessStore;
///
/// let store = InMemoryProcessStore::new();
/// # let _ = store;
/// ```
#[derive(Debug, Default)]
pub struct InMemoryProcessStore {
    // Unimplemented: read once feat/kernel-stores lands (spec 3.2 §7.3).
    #[allow(dead_code)]
    rows: RwLock<BTreeMap<String, ProcessMeta>>,
}

impl InMemoryProcessStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl ProcessStore for InMemoryProcessStore {
    async fn put(&self, _meta: ProcessMeta) -> Result<()> {
        Err(KernelError::Unimplemented { item: "stores::InMemoryProcessStore::put" })
    }

    async fn get(&self, _pid: &str) -> Result<Option<ProcessMeta>> {
        Err(KernelError::Unimplemented { item: "stores::InMemoryProcessStore::get" })
    }

    async fn list(&self, _filter: &ProcessFilter) -> Result<Vec<ProcessMeta>> {
        Err(KernelError::Unimplemented { item: "stores::InMemoryProcessStore::list" })
    }

    async fn set_wake(&self, _pid: &str, _wake: Option<WakeSet>) -> Result<()> {
        Err(KernelError::Unimplemented { item: "stores::InMemoryProcessStore::set_wake" })
    }

    async fn set_queue(&self, _pid: &str, _queue: QueueClass) -> Result<()> {
        Err(KernelError::Unimplemented { item: "stores::InMemoryProcessStore::set_queue" })
    }

    async fn delete(&self, _pid: &str) -> Result<bool> {
        Err(KernelError::Unimplemented { item: "stores::InMemoryProcessStore::delete" })
    }

    async fn set_suspend_requested(&self, _pid: &str, _requested: bool) -> Result<()> {
        Err(KernelError::Unimplemented {
            item: "stores::InMemoryProcessStore::set_suspend_requested",
        })
    }

    async fn add_spend(
        &self,
        _pid: &str,
        _partition: &PartitionId,
        _day: NaiveDate,
        _delta: SpendDelta,
    ) -> Result<()> {
        Err(KernelError::Unimplemented { item: "stores::InMemoryProcessStore::add_spend" })
    }

    async fn spend(
        &self,
        _scope: &SpendScope,
        _from: NaiveDate,
        _to_inclusive: NaiveDate,
    ) -> Result<u64> {
        Err(KernelError::Unimplemented { item: "stores::InMemoryProcessStore::spend" })
    }
}
