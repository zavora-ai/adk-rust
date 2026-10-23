//! The in-memory ledger store passes the ledger store conformance suite (spec 3.2 §7.10,
//! §11, §13 Q8).

#![cfg(feature = "proc")]

use std::sync::Arc;

use adk_kernel::conformance::LedgerStoreHarness;
use adk_kernel::proc::{InMemoryLedgerStore, LedgerStore};

struct InMemory;

#[async_trait::async_trait]
impl LedgerStoreHarness for InMemory {
    async fn open(&self) -> Arc<dyn LedgerStore> {
        Arc::new(InMemoryLedgerStore::new())
    }

    fn is_durable(&self) -> bool {
        false
    }
}

adk_kernel::ledger_store_conformance_tests! { harness = InMemory }
