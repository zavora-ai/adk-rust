//! Conformance cases for [`GovernanceStateStore`] backends.
//!
//! | Case | Contract | Backend primitive |
//! |------|----------|-------------------|
//! | [`set_get_list_round_trip`] | A state reads back exactly as set, at epoch 1 then 2, stamped with the store's clock; a stale epoch fails with `governance.conflict` | Compare-and-swap on the epoch |
//! | [`concurrent_set_has_one_winner`] | 32 concurrent sets with one expected epoch across two handles succeed exactly once | Create-if-absent, then compare-and-swap |
//! | [`scopes_are_independent`] | The organisation, tenant, and agent scopes keep separate states and epochs, even when their names coincide | Distinct keys per scope |
//! | [`states_survive_a_restart`] | States and epochs read back after a restart | Durable storage |
//!
//! Every case except one uses fresh tenant and agent scopes. [`scopes_are_independent`]
//! also writes the single organisation scope, starting from whatever epoch it holds.

use std::sync::Arc;

use async_trait::async_trait;

use super::{base_time, unique_unicode};
use crate::governance_state::{
    GOVERNANCE_CONFLICT_CODE, GovernanceScope, GovernanceState, GovernanceStateStore,
};
use crate::principal::{AgentId, SubjectRef};
use crate::{Clock, ManualClock};

/// Opens a [`GovernanceStateStore`] backend for the conformance suite.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::conformance::GovernanceStateStoreHarness;
/// use adk_core::{Clock, GovernanceStateStore, InMemoryGovernanceStateStore, async_trait};
///
/// struct InMemory;
///
/// #[async_trait]
/// impl GovernanceStateStoreHarness for InMemory {
///     async fn open(&self, clock: Arc<dyn Clock>) -> Arc<dyn GovernanceStateStore> {
///         Arc::new(InMemoryGovernanceStateStore::new().with_clock(clock))
///     }
///
///     fn is_durable(&self) -> bool {
///         false
///     }
/// }
/// ```
#[async_trait]
pub trait GovernanceStateStoreHarness: Send + Sync {
    /// Opens a store that stamps `issued_at` from `clock`.
    ///
    /// For a durable backend every call returns a new handle over the same stored data,
    /// as a restarted process or a second process would see it. Panics when the backend
    /// cannot be opened.
    async fn open(&self, clock: Arc<dyn Clock>) -> Arc<dyn GovernanceStateStore>;

    /// Whether data written through one handle is visible through the next.
    ///
    /// `false` skips the restart case and runs the concurrency case on a single handle.
    fn is_durable(&self) -> bool {
        true
    }
}

fn tenant() -> GovernanceScope {
    GovernanceScope::Tenant(unique_unicode("tenant"))
}

#[track_caller]
fn assert_conflict(result: crate::Result<GovernanceState>, current_epoch: u64, context: &str) {
    let error = result.expect_err(context);
    assert_eq!(error.code, GOVERNANCE_CONFLICT_CODE, "{context}: {error}");
    assert_eq!(error.category, crate::ErrorCategory::Conflict, "{context}: {error}");
    assert_eq!(
        error.details.metadata.get("current_epoch"),
        Some(&current_epoch.into()),
        "{context}: the error names the current epoch"
    );
}

/// A state reads back exactly as set, epochs count from 1, `issued_at` comes from the
/// store's clock, and a set with a stale expected epoch fails with
/// `governance.conflict` and changes nothing.
pub async fn set_get_list_round_trip<H: GovernanceStateStoreHarness + ?Sized>(harness: &H) {
    let clock = ManualClock::new(base_time());
    let store = harness.open(Arc::new(clock.clone())).await;
    let scope = tenant();
    assert_eq!(store.get(&scope).await.expect("get"), None);

    let oncall = SubjectRef::human("oncall@example.com");
    let frozen = store
        .set(scope.clone(), 0, true, Some("incident: ü/42".into()), Some(oncall.clone()))
        .await
        .expect("freeze");
    let expected = GovernanceState {
        scope: scope.clone(),
        frozen: true,
        reason: Some("incident: ü/42".into()),
        issued_by: Some(oncall),
        issued_at: base_time(),
        epoch: 1,
    };
    assert_eq!(frozen, expected);
    assert_eq!(store.get(&scope).await.expect("get"), Some(expected.clone()));
    assert_conflict(store.set(scope.clone(), 0, false, None, None).await, 1, "epoch 0 is stale");
    assert_conflict(store.set(scope.clone(), 5, false, None, None).await, 1, "epoch 5 is ahead");

    clock.advance(std::time::Duration::from_secs(60));
    let thawed = store.set(scope.clone(), 1, false, None, None).await.expect("unfreeze");
    let expected = GovernanceState {
        scope: scope.clone(),
        frozen: false,
        reason: None,
        issued_by: None,
        issued_at: clock.now(),
        epoch: 2,
    };
    assert_eq!(thawed, expected);
    let listed = store.list().await.expect("list");
    assert_eq!(listed.into_iter().filter(|s| s.scope == scope).collect::<Vec<_>>(), [expected]);
}

/// Concurrent sets of one scope with the same expected epoch, across two handles,
/// succeed exactly once; the rest fail with `governance.conflict`.
pub async fn concurrent_set_has_one_winner<H: GovernanceStateStoreHarness + ?Sized>(harness: &H) {
    let clock = ManualClock::new(base_time());
    let first = harness.open(Arc::new(clock.clone())).await;
    let second = if harness.is_durable() {
        harness.open(Arc::new(clock.clone())).await
    } else {
        first.clone()
    };
    let scope = tenant();

    let tasks: Vec<_> = (0..32)
        .map(|i| {
            let store = if i % 2 == 0 { first.clone() } else { second.clone() };
            let scope = scope.clone();
            tokio::spawn(async move {
                store.set(scope, 0, true, Some(format!("operator {i}")), None).await
            })
        })
        .collect();
    let mut winners = Vec::new();
    for task in tasks {
        match task.await.expect("set task completes") {
            Ok(state) => winners.push(state),
            Err(error) => assert_eq!(error.code, GOVERNANCE_CONFLICT_CODE, "{error}"),
        }
    }
    assert_eq!(winners.len(), 1, "exactly one operator's change lands");
    assert_eq!(first.get(&scope).await.expect("get"), Some(winners[0].clone()));
}

/// The organisation, tenant, and agent scopes keep separate states and epochs, even when
/// a tenant and an agent share a name.
pub async fn scopes_are_independent<H: GovernanceStateStoreHarness + ?Sized>(harness: &H) {
    let clock = ManualClock::new(base_time());
    let store = harness.open(Arc::new(clock.clone())).await;
    let name = unique_unicode("shared-name");
    let tenant = GovernanceScope::Tenant(name.clone());
    let agent = GovernanceScope::Agent(AgentId::new(name).expect("valid agent id"));

    store.set(tenant.clone(), 0, true, Some("tenant".into()), None).await.expect("tenant");
    assert_eq!(store.get(&agent).await.expect("get"), None, "the agent scope is separate");
    let agent_state = store.set(agent.clone(), 0, false, None, None).await.expect("agent");
    assert_eq!(agent_state.epoch, 1);
    let tenant_state = store.get(&tenant).await.expect("get").expect("tenant state");
    assert_eq!((tenant_state.frozen, tenant_state.epoch), (true, 1));

    let org_epoch = store.get(&GovernanceScope::Org).await.expect("get").map_or(0, |s| s.epoch);
    let org = store
        .set(GovernanceScope::Org, org_epoch, false, Some("conformance".into()), None)
        .await
        .expect("org");
    assert_eq!(org.epoch, org_epoch + 1);
    assert_eq!(
        store.get(&tenant).await.expect("get"),
        Some(tenant_state),
        "org leaves tenants alone"
    );
}

/// States and epochs written before a restart read back after it, and the CAS continues
/// from the stored epoch.
pub async fn states_survive_a_restart<H: GovernanceStateStoreHarness + ?Sized>(harness: &H) {
    if !harness.is_durable() {
        return;
    }
    let clock = ManualClock::new(base_time());
    let scope = tenant();
    let stored = {
        let before = harness.open(Arc::new(clock.clone())).await;
        before
            .set(
                scope.clone(),
                0,
                true,
                Some("before restart".into()),
                Some(SubjectRef::service("ops")),
            )
            .await
            .expect("set")
    };
    let after = harness.open(Arc::new(clock.clone())).await;
    assert_eq!(after.get(&scope).await.expect("get"), Some(stored));
    assert_conflict(after.set(scope.clone(), 0, false, None, None).await, 1, "after restart");
    assert_eq!(after.set(scope, 1, false, None, None).await.expect("set").epoch, 2);
}
