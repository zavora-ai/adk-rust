//! The in-memory stores pass the same conformance suites as every durable backend.

use std::sync::Arc;

use adk_core::conformance::{
    ActionLedgerHarness, ApprovalStoreHarness, SpendLedgerHarness, SpendLedgerSetup,
};
use adk_core::{
    ActionLedger, ApprovalStore, InMemoryActionLedger, InMemoryApprovalStore, InMemorySpendLedger,
    SpendLedger, async_trait,
};

struct InMemorySpend;

#[async_trait]
impl SpendLedgerHarness for InMemorySpend {
    async fn open(&self, setup: SpendLedgerSetup) -> Arc<dyn SpendLedger> {
        Arc::new(
            InMemorySpendLedger::new(setup.limits)
                .with_reservation_ttl(setup.reservation_ttl)
                .with_clock(setup.clock),
        )
    }

    fn is_durable(&self) -> bool {
        false
    }
}

#[derive(Default)]
struct InMemoryActions(Arc<InMemoryActionLedger>);

#[async_trait]
impl ActionLedgerHarness for InMemoryActions {
    async fn open(&self) -> Arc<dyn ActionLedger> {
        self.0.clone()
    }
}

#[derive(Default)]
struct InMemoryApprovals(Arc<InMemoryApprovalStore>);

#[async_trait]
impl ApprovalStoreHarness for InMemoryApprovals {
    async fn open(&self) -> Arc<dyn ApprovalStore> {
        self.0.clone()
    }
}

mod spend {
    adk_core::spend_ledger_conformance_tests!(harness = super::InMemorySpend);
}

mod actions {
    adk_core::action_ledger_conformance_tests!(harness = super::InMemoryActions::default());
}

mod approvals {
    adk_core::approval_store_conformance_tests!(harness = super::InMemoryApprovals::default());
}
