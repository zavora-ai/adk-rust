//! Conformance cases for [`DeviceStore`] backends (spec 3.3 §10 K2, §11 S-I7).
//!
//! | Case | Contract |
//! |------|----------|
//! | [`code_round_trip_and_take_once`] | A code reads back exactly as put, once: a second take finds nothing |
//! | [`take_returns_expired_codes`] | A take returns a code past its expiry; the caller decides |
//! | [`prune_removes_expired_codes`] | `prune_codes` removes the codes expired at or before the instant, and only those |
//! | [`clear_removes_every_code`] | `clear_codes` removes every waiting code |
//! | [`device_round_trip`] | A device reads back exactly as put: every field, every scope |
//! | [`put_device_replaces`] | A second put of one device replaces it, as a revocation does |
//! | [`list_devices_orders_by_pairing_then_id`] | `list_devices` orders by pairing time, then id |
//! | [`unknown_ids`] | An unknown device and an unknown code are `None` |
//! | [`records_survive_a_restart`] | Devices and codes read back through a new handle (durable backends only) |
//! | [`concurrent_takes_take_once`] | Eight concurrent takes of one code from two handles take it once |
//! | [`concurrent_writers`] | Concurrent puts from two handles over distinct devices and codes lose none |
//! | [`claim_attempts_are_counted_once_and_shared`] | Concurrent claim counts from two handles each get a distinct count, a reset starts again at one, and the count survives a restart |
//!
//! Every case writes under fresh ids and hashes, so a harness may point every case at one
//! shared database, except [`clear_removes_every_code`], which removes every code, and
//! [`claim_attempts_are_counted_once_and_shared`], which uses the one count of claims: give
//! each a database of its own, as the harnesses of this crate give every case.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};

use crate::pairing::{DeviceRecord, DeviceScope, DeviceStore, PairingCode};

/// Opens a [`DeviceStore`] backend for the conformance suite.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_kernel::conformance::DeviceStoreHarness;
/// use adk_kernel::pairing::{DeviceStore, InMemoryDeviceStore};
///
/// struct InMemory;
///
/// #[async_trait::async_trait]
/// impl DeviceStoreHarness for InMemory {
///     async fn open(&self) -> Arc<dyn DeviceStore> {
///         Arc::new(InMemoryDeviceStore::new())
///     }
///
///     fn is_durable(&self) -> bool {
///         false
///     }
/// }
/// ```
#[async_trait]
pub trait DeviceStoreHarness: Send + Sync {
    /// Opens a store. For a durable backend every call returns a new handle over the same
    /// stored data. Panics when the backend cannot be opened.
    async fn open(&self) -> Arc<dyn DeviceStore>;

    /// Whether data written through one handle is visible through the next.
    fn is_durable(&self) -> bool {
        true
    }
}

/// Two handles over one store: two opens of a durable backend, one store twice otherwise.
async fn two(harness: &dyn DeviceStoreHarness) -> [Arc<dyn DeviceStore>; 2] {
    if harness.is_durable() {
        [harness.open().await, harness.open().await]
    } else {
        let store = harness.open().await;
        [store.clone(), store]
    }
}

/// A fresh hex id, as the pairing service makes them.
fn fresh() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// An instant with microseconds, so a backend that rounds timestamps is caught.
fn instant(seconds: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_800_000_000 + seconds, 123_456_000).single().expect("valid instant")
}

fn code(owner: &str, expires: i64) -> PairingCode {
    let mut code = PairingCode::new(
        fresh(),
        owner,
        Duration::from_secs(90 * 86_400),
        instant(0),
        instant(expires),
    );
    code.groups = vec!["operator".into(), "on-call/日本".into()];
    code.scopes = vec![DeviceScope::Decide, DeviceScope::Stop];
    code.name = Some("Ana's phone \u{1F4F1}".into());
    code
}

fn device(created: i64) -> DeviceRecord {
    DeviceRecord::new(
        fresh(),
        "Ana's \"phone\" 日本",
        "user/ü",
        fresh(),
        instant(created),
        instant(created + 86_400),
    )
    .with_scopes([DeviceScope::Read, DeviceScope::Decide])
    .with_groups(["operator"])
}

/// A code reads back exactly as put, once.
pub async fn code_round_trip_and_take_once(harness: &dyn DeviceStoreHarness) {
    let store = harness.open().await;
    let code = code("ana", 600);
    store.put_code(code.clone()).await.expect("put");
    assert_eq!(store.take_code(&code.code_hash).await.expect("take"), Some(code.clone()));
    assert_eq!(store.take_code(&code.code_hash).await.expect("take again"), None);
}

/// A take returns a code past its expiry.
pub async fn take_returns_expired_codes(harness: &dyn DeviceStoreHarness) {
    let store = harness.open().await;
    let code = code("ana", -600);
    store.put_code(code.clone()).await.expect("put");
    assert_eq!(store.take_code(&code.code_hash).await.expect("take"), Some(code));
}

/// `prune_codes` removes the codes expired at or before the instant, and only those.
pub async fn prune_removes_expired_codes(harness: &dyn DeviceStoreHarness) {
    let store = harness.open().await;
    // Far in the past, so codes other cases left in a shared database are pruned too and
    // the count stays exact only for this case's codes below the cut.
    let expired = code("ana", -1_000_000_000);
    let edge = code("ana", -999_999_000);
    let live = code("ana", 1_000_000_000);
    for code in [&expired, &edge, &live] {
        store.put_code(code.clone()).await.expect("put");
    }
    let removed = store.prune_codes(edge.expires_at).await.expect("prune");
    assert!(removed >= 2, "removed {removed}");
    assert_eq!(store.take_code(&expired.code_hash).await.expect("take"), None);
    assert_eq!(store.take_code(&edge.code_hash).await.expect("take"), None);
    assert_eq!(store.take_code(&live.code_hash).await.expect("take"), Some(live));
}

/// `clear_codes` removes every waiting code.
pub async fn clear_removes_every_code(harness: &dyn DeviceStoreHarness) {
    let store = harness.open().await;
    let codes = [code("ana", 600), code("bo", 600)];
    for code in &codes {
        store.put_code(code.clone()).await.expect("put");
    }
    assert!(store.clear_codes().await.expect("clear") >= 2);
    for code in &codes {
        assert_eq!(store.take_code(&code.code_hash).await.expect("take"), None);
    }
}

/// A device reads back exactly as put.
pub async fn device_round_trip(harness: &dyn DeviceStoreHarness) {
    let store = harness.open().await;
    for scopes in [DeviceScope::ALL.to_vec(), vec![DeviceScope::Read], vec![DeviceScope::Stop]] {
        let mut device = device(0).with_scopes(scopes);
        device.revoked_at = Some(instant(30));
        store.put_device(device.clone()).await.expect("put");
        assert_eq!(store.get_device(&device.id).await.expect("get"), Some(device));
    }
}

/// A second put of one device replaces it.
pub async fn put_device_replaces(harness: &dyn DeviceStoreHarness) {
    let store = harness.open().await;
    let mut device = device(0);
    store.put_device(device.clone()).await.expect("put");
    device.revoked_at = Some(instant(60));
    store.put_device(device.clone()).await.expect("put again");
    assert_eq!(store.get_device(&device.id).await.expect("get"), Some(device.clone()));
    let listed = store.list_devices().await.expect("list");
    assert_eq!(listed.iter().filter(|listed| listed.id == device.id).count(), 1);
}

/// `list_devices` orders by pairing time, then id.
pub async fn list_devices_orders_by_pairing_then_id(harness: &dyn DeviceStoreHarness) {
    let store = harness.open().await;
    let mut late = device(200);
    let early = device(100);
    let mut tied = device(200);
    // Two devices paired at one instant order by id.
    if tied.id < late.id {
        std::mem::swap(&mut tied.id, &mut late.id);
    }
    for device in [&late, &tied, &early] {
        store.put_device(device.clone()).await.expect("put");
    }
    let ours = [&early.id, &late.id, &tied.id];
    let listed: Vec<String> = store
        .list_devices()
        .await
        .expect("list")
        .into_iter()
        .filter(|device| ours.contains(&&device.id))
        .map(|device| device.id)
        .collect();
    assert_eq!(listed, [early.id.clone(), late.id.clone(), tied.id.clone()]);
}

/// An unknown device and an unknown code are `None`.
pub async fn unknown_ids(harness: &dyn DeviceStoreHarness) {
    let store = harness.open().await;
    assert_eq!(store.get_device(&fresh()).await.expect("get"), None);
    assert_eq!(store.take_code(&fresh()).await.expect("take"), None);
}

/// Devices and codes read back through a new handle.
pub async fn records_survive_a_restart(harness: &dyn DeviceStoreHarness) {
    if !harness.is_durable() {
        return;
    }
    let device = device(0);
    let code = code("ana", 600);
    {
        let store = harness.open().await;
        store.put_device(device.clone()).await.expect("put");
        store.put_code(code.clone()).await.expect("put");
    }
    let store = harness.open().await;
    assert_eq!(store.get_device(&device.id).await.expect("get"), Some(device));
    assert_eq!(store.take_code(&code.code_hash).await.expect("take"), Some(code));
}

/// Eight concurrent takes of one code from two handles take it once.
pub async fn concurrent_takes_take_once(harness: &dyn DeviceStoreHarness) {
    let stores = two(harness).await;
    let code = code("ana", 600);
    stores[0].put_code(code.clone()).await.expect("put");
    let mut takes = Vec::new();
    for index in 0..8 {
        let store = stores[index % 2].clone();
        let hash = code.code_hash.clone();
        takes.push(tokio::spawn(async move { store.take_code(&hash).await.expect("take") }));
    }
    let mut taken = 0;
    for take in takes {
        if take.await.expect("take completes").is_some() {
            taken += 1;
        }
    }
    assert_eq!(taken, 1);
}

/// Concurrent puts from two handles over distinct devices and codes lose none.
pub async fn concurrent_writers(harness: &dyn DeviceStoreHarness) {
    let stores = two(harness).await;
    let mut writes = Vec::new();
    let mut devices = Vec::new();
    let mut codes = Vec::new();
    for index in 0..16 {
        let store = stores[index % 2].clone();
        let device = device(i64::try_from(index).expect("small"));
        let code = code("ana", 600);
        devices.push(device.clone());
        codes.push(code.clone());
        writes.push(tokio::spawn(async move {
            store.put_device(device).await.expect("put device");
            store.put_code(code).await.expect("put code");
        }));
    }
    for write in writes {
        write.await.expect("write completes");
    }
    let store = &stores[1];
    for device in devices {
        assert_eq!(store.get_device(&device.id).await.expect("get").as_ref(), Some(&device));
    }
    for code in codes {
        assert_eq!(store.take_code(&code.code_hash).await.expect("take"), Some(code));
    }
}

/// Concurrent claim counts from two handles each get a distinct count, a reset starts the
/// count again at one, and a durable count survives a restart (review finding 14: replicas
/// share the count of wrong codes).
pub async fn claim_attempts_are_counted_once_and_shared(harness: &dyn DeviceStoreHarness) {
    let stores = two(harness).await;
    stores[0].reset_claim_attempts().await.expect("reset");
    let mut counts = Vec::new();
    for index in 0..16 {
        let store = stores[index % 2].clone();
        counts.push(tokio::spawn(async move { store.count_claim_attempt().await.expect("count") }));
    }
    let mut seen = Vec::new();
    for count in counts {
        seen.push(count.await.expect("count completes"));
    }
    seen.sort_unstable();
    assert_eq!(seen, (1..=16).collect::<Vec<u64>>(), "every count is distinct");
    stores[1].reset_claim_attempts().await.expect("reset");
    assert_eq!(stores[0].count_claim_attempt().await.expect("count"), 1);
    if harness.is_durable() {
        let reopened = harness.open().await;
        assert_eq!(reopened.count_claim_attempt().await.expect("count"), 2);
    }
}

/// Generates one test per [`device_store`](self) case for a `DeviceStoreHarness` (spec 3.3 §10 K2).
///
/// Used as [`process_store_conformance_tests!`](crate::process_store_conformance_tests) is;
/// see the [module documentation](crate::conformance).
#[macro_export]
macro_rules! device_store_conformance_tests {
    ($(#[$attr:meta])* harness = $harness:expr $(,)?) => {
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn device_store_code_round_trip_and_take_once() {
            $crate::conformance::device_store::code_round_trip_and_take_once(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn device_store_take_returns_expired_codes() {
            $crate::conformance::device_store::take_returns_expired_codes(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn device_store_prune_removes_expired_codes() {
            $crate::conformance::device_store::prune_removes_expired_codes(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn device_store_clear_removes_every_code() {
            $crate::conformance::device_store::clear_removes_every_code(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn device_store_device_round_trip() {
            $crate::conformance::device_store::device_round_trip(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn device_store_put_device_replaces() {
            $crate::conformance::device_store::put_device_replaces(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn device_store_list_devices_orders_by_pairing_then_id() {
            $crate::conformance::device_store::list_devices_orders_by_pairing_then_id(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn device_store_unknown_ids() {
            $crate::conformance::device_store::unknown_ids(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn device_store_records_survive_a_restart() {
            $crate::conformance::device_store::records_survive_a_restart(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn device_store_concurrent_takes_take_once() {
            $crate::conformance::device_store::concurrent_takes_take_once(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn device_store_concurrent_writers() {
            $crate::conformance::device_store::concurrent_writers(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn device_store_claim_attempts_are_counted_once_and_shared() {
            $crate::conformance::device_store::claim_attempts_are_counted_once_and_shared(&$harness).await;
        }
    };
}
