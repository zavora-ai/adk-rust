//! The in-memory agent store passes the agent store conformance suite (spec 3.3 §5.7, §10
//! K7).

use std::sync::Arc;

use adk_kernel::agents::AgentStore;
use adk_kernel::conformance::AgentStoreHarness;
use adk_kernel::stores::InMemoryAgentStore;

struct InMemory;

#[async_trait::async_trait]
impl AgentStoreHarness for InMemory {
    async fn open(&self) -> Arc<dyn AgentStore> {
        Arc::new(InMemoryAgentStore::new())
    }

    fn is_durable(&self) -> bool {
        false
    }
}

adk_kernel::agent_store_conformance_tests! { harness = InMemory }
