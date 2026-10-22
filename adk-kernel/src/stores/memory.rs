//! The in-memory [`ProcessStore`] (spec 3.2 §7.3, §13 Q8).

use std::collections::BTreeMap;

use async_trait::async_trait;
use tokio::sync::RwLock;

use super::{ProcessFilter, ProcessStore};
use crate::error::{KernelError, Result};
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
}
