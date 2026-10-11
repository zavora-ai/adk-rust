//! Conformance cases for [`LeaseStore`] backends.
//!
//! | Case | Contract | Backend primitive |
//! |------|----------|-------------------|
//! | [`acquire_renew_release`] | Epochs start at 1 and increase on every grant; renew keeps the epoch; release frees the resource | Single-row upsert |
//! | [`concurrent_acquire_has_one_winner`] | 32 concurrent acquirers across two handles get exactly one lease | Conditional upsert, Lua, `findOneAndUpdate`, or a transaction |
//! | [`expiry_takeover_bumps_epoch`] | An expired lease is granted to another owner at the next epoch; an expired, untaken lease can still be renewed | Expiry compared inside the conditional write |
//! | [`stale_renew_and_release_are_refused`] | Renewing a lease that was taken over, released, or never granted fails with `lease.stale_epoch`; releasing it changes nothing | Epoch compared inside the conditional write |
//! | [`leases_survive_a_restart`] | A lease granted before a restart still blocks other owners and keeps counting epochs | Durable storage |

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use super::{base_time, unique_unicode};
use crate::lease::{LEASE_STALE_EPOCH_CODE, Lease, LeaseStore};
use crate::{Clock, ManualClock};

/// Opens a [`LeaseStore`] backend for the conformance suite.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::conformance::LeaseStoreHarness;
/// use adk_core::{Clock, InMemoryLeaseStore, LeaseStore, async_trait};
///
/// struct InMemory;
///
/// #[async_trait]
/// impl LeaseStoreHarness for InMemory {
///     async fn open(&self, clock: Arc<dyn Clock>) -> Arc<dyn LeaseStore> {
///         Arc::new(InMemoryLeaseStore::new().with_clock(clock))
///     }
///
///     fn is_durable(&self) -> bool {
///         false
///     }
/// }
/// ```
#[async_trait]
pub trait LeaseStoreHarness: Send + Sync {
    /// Opens a store that reads `clock`.
    ///
    /// For a durable backend every call returns a new handle over the same stored data,
    /// as a restarted process or a second process would see it. Panics when the backend
    /// cannot be opened.
    async fn open(&self, clock: Arc<dyn Clock>) -> Arc<dyn LeaseStore>;

    /// Whether data written through one handle is visible through the next.
    ///
    /// `false` skips the restart case and runs the concurrency case on a single handle.
    fn is_durable(&self) -> bool {
        true
    }
}

const TTL: Duration = Duration::from_secs(30);

async fn two_handles<H: LeaseStoreHarness + ?Sized>(
    harness: &H,
    clock: &ManualClock,
) -> (Arc<dyn LeaseStore>, Arc<dyn LeaseStore>) {
    let first = harness.open(Arc::new(clock.clone())).await;
    let second = if harness.is_durable() {
        harness.open(Arc::new(clock.clone())).await
    } else {
        first.clone()
    };
    (first, second)
}

#[track_caller]
fn assert_stale(result: crate::Result<Lease>, context: &str) {
    let error = result.expect_err(context);
    assert_eq!(error.code, LEASE_STALE_EPOCH_CODE, "{context}: {error}");
    assert_eq!(error.category, crate::ErrorCategory::Conflict, "{context}: {error}");
}

/// Grants start at epoch 1, a second owner is refused while the lease is live, renew
/// keeps the epoch and moves the expiry, and release frees the resource for the next
/// grant at the next epoch.
pub async fn acquire_renew_release<H: LeaseStoreHarness + ?Sized>(harness: &H) {
    let clock = ManualClock::new(base_time());
    let store = harness.open(Arc::new(clock.clone())).await;
    let resource = unique_unicode("lease");
    assert_eq!(store.current(&resource).await.expect("current"), None);

    let lease = store.acquire(&resource, "w1", TTL).await.expect("acquire").expect("granted");
    let expected = Lease {
        resource: resource.clone(),
        owner: "w1".into(),
        epoch: 1,
        expires_at: base_time() + chrono::Duration::seconds(30),
    };
    assert_eq!(lease, expected);
    assert_eq!(store.current(&resource).await.expect("current"), Some(expected.clone()));
    assert_eq!(store.acquire(&resource, "w2", TTL).await.expect("acquire"), None);

    clock.advance(Duration::from_secs(20));
    let renewed = store.renew(&lease, TTL).await.expect("renew");
    assert_eq!(
        renewed,
        Lease { expires_at: base_time() + chrono::Duration::seconds(50), ..expected.clone() }
    );
    clock.advance(Duration::from_secs(20));
    assert_eq!(
        store.acquire(&resource, "w2", TTL).await.expect("acquire"),
        None,
        "the renewal moved the expiry"
    );

    let again = store.acquire(&resource, "w1", TTL).await.expect("acquire").expect("own lease");
    assert_eq!(again.epoch, 2, "re-acquiring an owned lease fences the earlier grant");
    assert_stale(store.renew(&renewed, TTL).await, "the earlier grant is stale");

    store.release(&again).await.expect("release");
    assert_eq!(store.current(&resource).await.expect("current"), None);
    assert_stale(store.renew(&again, TTL).await, "a released lease is not renewed");
    let next = store.acquire(&resource, "w2", TTL).await.expect("acquire").expect("free");
    assert_eq!((next.owner.as_str(), next.epoch), ("w2", 3), "epochs continue after release");
}

/// 32 concurrent acquirers, split across two handles when the backend is durable, get
/// exactly one lease.
pub async fn concurrent_acquire_has_one_winner<H: LeaseStoreHarness + ?Sized>(harness: &H) {
    let clock = ManualClock::new(base_time());
    let (first, second) = two_handles(harness, &clock).await;
    let resource = unique_unicode("lease");
    let tasks: Vec<_> = (0..32)
        .map(|i| {
            let store = if i % 2 == 0 { first.clone() } else { second.clone() };
            let resource = resource.clone();
            tokio::spawn(async move { store.acquire(&resource, &format!("worker-{i}"), TTL).await })
        })
        .collect();
    let mut winners = Vec::new();
    for task in tasks {
        if let Some(lease) = task.await.expect("acquire task completes").expect("acquire") {
            winners.push(lease);
        }
    }
    assert_eq!(winners.len(), 1, "exactly one acquirer wins: {winners:?}");
    assert_eq!(winners[0].epoch, 1);
    assert_eq!(first.current(&resource).await.expect("current"), Some(winners[0].clone()));
}

/// Once a lease expires another owner takes it at the next epoch, and the old holder's
/// renew is refused; a lease that expired without being taken can still be renewed.
pub async fn expiry_takeover_bumps_epoch<H: LeaseStoreHarness + ?Sized>(harness: &H) {
    let clock = ManualClock::new(base_time());
    let (first, second) = two_handles(harness, &clock).await;
    let resource = unique_unicode("lease");

    let old = first.acquire(&resource, "w1", TTL).await.expect("acquire").expect("granted");
    clock.advance(Duration::from_secs(31));
    assert_eq!(first.current(&resource).await.expect("current"), None, "the lease expired");
    let late = first.renew(&old, TTL).await.expect("an expired, untaken lease renews");
    assert_eq!(late.epoch, 1);

    clock.advance(Duration::from_secs(31));
    let taken = second.acquire(&resource, "w2", TTL).await.expect("acquire").expect("expired");
    assert_eq!((taken.owner.as_str(), taken.epoch), ("w2", 2));
    assert_stale(first.renew(&old, TTL).await, "the old holder lost the lease");
    first.release(&old).await.expect("a stale release is a no-op");
    assert_eq!(second.current(&resource).await.expect("current"), Some(taken));
}

/// Renewing a lease that another owner holds, that was released, that names the wrong
/// owner, or that was never granted fails with `lease.stale_epoch`, and releasing it
/// leaves the current lease untouched.
pub async fn stale_renew_and_release_are_refused<H: LeaseStoreHarness + ?Sized>(harness: &H) {
    let clock = ManualClock::new(base_time());
    let store = harness.open(Arc::new(clock.clone())).await;
    let resource = unique_unicode("lease");

    let never =
        Lease { resource: resource.clone(), owner: "w1".into(), epoch: 7, expires_at: base_time() };
    assert_stale(store.renew(&never, TTL).await, "a lease never granted");
    store.release(&never).await.expect("releasing an unknown lease is a no-op");

    let lease = store.acquire(&resource, "w1", TTL).await.expect("acquire").expect("granted");
    let impostor = Lease { owner: "w2".into(), ..lease.clone() };
    assert_stale(store.renew(&impostor, TTL).await, "the wrong owner");
    store.release(&impostor).await.expect("no-op");
    let future = Lease { epoch: lease.epoch + 1, ..lease.clone() };
    assert_stale(store.renew(&future, TTL).await, "an epoch ahead of the store");
    store.release(&future).await.expect("no-op");
    assert_eq!(store.current(&resource).await.expect("current"), Some(lease.clone()));

    store.release(&lease).await.expect("release");
    store.release(&lease).await.expect("a second release is a no-op");
    assert_stale(store.renew(&lease, TTL).await, "after release");
}

/// A lease granted before a restart still blocks other owners after it, renews through
/// the new handle, and the next grant continues its epoch.
pub async fn leases_survive_a_restart<H: LeaseStoreHarness + ?Sized>(harness: &H) {
    if !harness.is_durable() {
        return;
    }
    let clock = ManualClock::new(base_time());
    let resource = unique_unicode("lease");
    let lease = {
        let before = harness.open(Arc::new(clock.clone())).await;
        before.acquire(&resource, "w1", TTL).await.expect("acquire").expect("granted")
    };

    let after = harness.open(Arc::new(clock.clone())).await;
    assert_eq!(after.current(&resource).await.expect("current"), Some(lease.clone()));
    assert_eq!(after.acquire(&resource, "w2", TTL).await.expect("acquire"), None);
    after.renew(&lease, TTL).await.expect("renew after restart");
    after.release(&lease).await.expect("release after restart");
    let next = after.acquire(&resource, "w2", TTL).await.expect("acquire").expect("free");
    assert_eq!(next.epoch, 2);
}
