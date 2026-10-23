//! The in-memory partition store passes the partition store conformance suite (spec 3.2
//! §7.8, §11, §13 Q8).

use std::sync::Arc;

use adk_kernel::conformance::PartitionStoreHarness;
use adk_kernel::{InMemoryPartitionStore, PartitionStore};

struct InMemory;

#[async_trait::async_trait]
impl PartitionStoreHarness for InMemory {
    async fn open(&self) -> Arc<dyn PartitionStore> {
        Arc::new(InMemoryPartitionStore::new())
    }

    fn is_durable(&self) -> bool {
        false
    }
}

adk_kernel::partition_store_conformance_tests! { harness = InMemory }
