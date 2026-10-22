//! The in-memory process store passes the process store conformance suite (spec 3.2 §11,
//! §13 Q8).

use std::sync::Arc;

use adk_kernel::conformance::ProcessStoreHarness;
use adk_kernel::{InMemoryProcessStore, ProcessStore};

struct InMemory;

#[async_trait::async_trait]
impl ProcessStoreHarness for InMemory {
    async fn open(&self) -> Arc<dyn ProcessStore> {
        Arc::new(InMemoryProcessStore::new())
    }

    fn is_durable(&self) -> bool {
        false
    }
}

adk_kernel::process_store_conformance_tests! { harness = InMemory }
