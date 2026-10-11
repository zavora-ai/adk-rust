//! The in-memory stores pass the same conformance suites as every durable backend.

use std::sync::Arc;

use adk_core::conformance::{
    ActionLedgerHarness, ApprovalStoreHarness, GovernanceStateStoreHarness, LeaseStoreHarness,
    RunStoreHarness, ScheduleStoreHarness, SpendLedgerHarness, SpendLedgerSetup,
};
use adk_core::{
    ActionLedger, ApprovalStore, Clock, DurableRunStore, GovernanceStateStore,
    InMemoryActionLedger, InMemoryApprovalStore, InMemoryGovernanceStateStore, InMemoryLeaseStore,
    InMemoryRunStore, InMemoryScheduleStore, InMemorySpendLedger, LeaseStore, ScheduleStore,
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

mod approval_service {
    adk_core::approval_service_conformance_tests!(harness = super::InMemoryApprovals::default());
}

struct InMemoryLeases;

#[async_trait]
impl LeaseStoreHarness for InMemoryLeases {
    async fn open(&self, clock: Arc<dyn Clock>) -> Arc<dyn LeaseStore> {
        Arc::new(InMemoryLeaseStore::new().with_clock(clock))
    }

    fn is_durable(&self) -> bool {
        false
    }
}

struct InMemoryRuns;

#[async_trait]
impl RunStoreHarness for InMemoryRuns {
    async fn open(&self, clock: Arc<dyn Clock>) -> Arc<dyn DurableRunStore> {
        Arc::new(InMemoryRunStore::new().with_clock(clock))
    }

    fn is_durable(&self) -> bool {
        false
    }
}

struct InMemorySchedules;

#[async_trait]
impl ScheduleStoreHarness for InMemorySchedules {
    async fn open(&self) -> Arc<dyn ScheduleStore> {
        Arc::new(InMemoryScheduleStore::new())
    }

    fn is_durable(&self) -> bool {
        false
    }
}

struct InMemoryGovernance;

#[async_trait]
impl GovernanceStateStoreHarness for InMemoryGovernance {
    async fn open(&self, clock: Arc<dyn Clock>) -> Arc<dyn GovernanceStateStore> {
        Arc::new(InMemoryGovernanceStateStore::new().with_clock(clock))
    }

    fn is_durable(&self) -> bool {
        false
    }
}

mod leases {
    adk_core::lease_store_conformance_tests!(harness = super::InMemoryLeases);
}

mod runs {
    adk_core::run_store_conformance_tests!(harness = super::InMemoryRuns);
}

mod schedules {
    adk_core::schedule_store_conformance_tests!(harness = super::InMemorySchedules);
}

mod governance {
    adk_core::governance_state_store_conformance_tests!(harness = super::InMemoryGovernance);
}

#[derive(Default)]
struct InMemoryAuditLog(adk_core::audit_log::InMemoryAuditLogStore);

#[async_trait]
impl adk_core::conformance::AuditLogStoreHarness for InMemoryAuditLog {
    async fn open(&self) -> Arc<dyn adk_core::audit_log::AuditLogStore> {
        Arc::new(self.0.clone())
    }
}

mod audit_log {
    adk_core::audit_log_store_conformance_tests!(harness = super::InMemoryAuditLog::default());
}
