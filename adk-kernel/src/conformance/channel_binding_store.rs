//! Conformance cases for [`ChannelBindingStore`] backends (spec 3.3 §10 K1; feature
//! `channels`).
//!
//! | Case | Contract |
//! |------|----------|
//! | [`binding_round_trip`] | A binding reads back exactly as put, with the characters a backend must escape |
//! | [`put_binding_replaces`] | A second put of one binding replaces it, as a revocation does |
//! | [`list_bindings_orders_by_pairing_then_id`] | `list_bindings` orders by pairing time, then id |
//! | [`request_round_trip`] | A request reads back by channel and account, and is taken once by its code |
//! | [`put_request_replaces_the_accounts_request`] | An account's second request replaces its first; the first code takes nothing |
//! | [`take_returns_expired_requests`] | A take returns a request past its expiry; the caller decides |
//! | [`prune_removes_expired_requests`] | `prune_requests` removes the requests expired at or before the instant, and only those |
//! | [`list_requests_orders_by_time_then_account`] | `list_requests` orders by request time, then channel and account |
//! | [`unknown_ids`] | An unknown binding, request and code are `None` |
//! | [`records_survive_a_restart`] | Bindings and requests read back through a new handle (durable backends only) |
//! | [`concurrent_takes_take_once`] | Eight concurrent takes of one code from two handles take it once |
//!
//! Every case writes under fresh channels and accounts, so a harness may point every case
//! at one shared database.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};

use crate::channels::{ChannelBinding, ChannelBindingStore, ChannelPairingRequest};

/// Opens a [`ChannelBindingStore`] backend for the conformance suite.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_kernel::channels::{ChannelBindingStore, InMemoryChannelBindingStore};
/// use adk_kernel::conformance::ChannelBindingStoreHarness;
///
/// struct InMemory;
///
/// #[async_trait::async_trait]
/// impl ChannelBindingStoreHarness for InMemory {
///     async fn open(&self) -> Arc<dyn ChannelBindingStore> {
///         Arc::new(InMemoryChannelBindingStore::new())
///     }
///
///     fn is_durable(&self) -> bool {
///         false
///     }
/// }
/// ```
#[async_trait]
pub trait ChannelBindingStoreHarness: Send + Sync {
    /// Opens a store. For a durable backend every call returns a new handle over the same
    /// stored data. Panics when the backend cannot be opened.
    async fn open(&self) -> Arc<dyn ChannelBindingStore>;

    /// Whether data written through one handle is visible through the next.
    fn is_durable(&self) -> bool {
        true
    }
}

async fn two(harness: &dyn ChannelBindingStoreHarness) -> [Arc<dyn ChannelBindingStore>; 2] {
    if harness.is_durable() {
        [harness.open().await, harness.open().await]
    } else {
        let store = harness.open().await;
        [store.clone(), store]
    }
}

fn fresh() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// A fresh channel with the characters a backend must escape.
fn channel() -> String {
    format!("chat:ü/{}", fresh())
}

/// An instant with microseconds, so a backend that rounds timestamps is caught.
fn instant(seconds: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_800_000_000 + seconds, 654_321_000).single().expect("valid instant")
}

fn binding(channel: &str, paired: i64) -> ChannelBinding {
    let mut binding = ChannelBinding::new(
        channel,
        format!("acct %2F/{}", fresh()),
        "-100/日本",
        "user/ü",
        instant(paired),
    )
    .with_groups(["operator", "on-call"]);
    binding.display_name = Some("Ana \"A\" 日本".into());
    binding
}

fn request(channel: &str, account: &str, at: i64, expires: i64) -> ChannelPairingRequest {
    let mut request = ChannelPairingRequest::new(
        fresh(),
        channel,
        account,
        "thread/1",
        instant(at),
        instant(expires),
    );
    request.display_name = Some("Bo".into());
    request
}

/// A binding reads back exactly as put.
pub async fn binding_round_trip(harness: &dyn ChannelBindingStoreHarness) {
    let store = harness.open().await;
    let mut binding = binding(&channel(), 0);
    store.put_binding(binding.clone()).await.expect("put");
    assert_eq!(store.get_binding(&binding.id).await.expect("get"), Some(binding.clone()));
    binding.revoked_at = Some(instant(10));
    binding.display_name = None;
    store.put_binding(binding.clone()).await.expect("put");
    assert_eq!(store.get_binding(&binding.id).await.expect("get"), Some(binding));
}

/// A second put of one binding replaces it.
pub async fn put_binding_replaces(harness: &dyn ChannelBindingStoreHarness) {
    let store = harness.open().await;
    let mut binding = binding(&channel(), 0);
    store.put_binding(binding.clone()).await.expect("put");
    binding.revoked_at = Some(instant(60));
    store.put_binding(binding.clone()).await.expect("put again");
    let listed = store.list_bindings().await.expect("list");
    assert_eq!(
        listed.iter().filter(|listed| listed.id == binding.id).collect::<Vec<_>>(),
        [&binding]
    );
}

/// `list_bindings` orders by pairing time, then id.
pub async fn list_bindings_orders_by_pairing_then_id(harness: &dyn ChannelBindingStoreHarness) {
    let store = harness.open().await;
    let channel = channel();
    let early = binding(&channel, 100);
    let mut late = binding(&channel, 200);
    let mut tied = binding(&channel, 200);
    if tied.id < late.id {
        std::mem::swap(&mut tied, &mut late);
    }
    for binding in [&tied, &late, &early] {
        store.put_binding(binding.clone()).await.expect("put");
    }
    let listed: Vec<String> = store
        .list_bindings()
        .await
        .expect("list")
        .into_iter()
        .filter(|binding| binding.channel == channel)
        .map(|binding| binding.id)
        .collect();
    assert_eq!(listed, [early.id, late.id, tied.id]);
}

/// A request reads back by channel and account, and is taken once by its code.
pub async fn request_round_trip(harness: &dyn ChannelBindingStoreHarness) {
    let store = harness.open().await;
    let channel = channel();
    let request = request(&channel, "acct/ü 1", 0, 3_600);
    store.put_request(request.clone()).await.expect("put");
    assert_eq!(store.get_request(&channel, "acct/ü 1").await.expect("get"), Some(request.clone()));
    assert_eq!(store.take_request(&request.code_hash).await.expect("take"), Some(request.clone()));
    assert_eq!(store.take_request(&request.code_hash).await.expect("take again"), None);
    assert_eq!(store.get_request(&channel, "acct/ü 1").await.expect("get"), None);
}

/// An account's second request replaces its first.
pub async fn put_request_replaces_the_accounts_request(harness: &dyn ChannelBindingStoreHarness) {
    let store = harness.open().await;
    let channel = channel();
    let first = request(&channel, "acct", 0, 3_600);
    let second = request(&channel, "acct", 10, 3_610);
    store.put_request(first.clone()).await.expect("put");
    store.put_request(second.clone()).await.expect("put again");
    assert_eq!(store.get_request(&channel, "acct").await.expect("get"), Some(second.clone()));
    assert_eq!(store.take_request(&first.code_hash).await.expect("take first"), None);
    let ours = store
        .list_requests()
        .await
        .expect("list")
        .into_iter()
        .filter(|request| request.channel == channel)
        .count();
    assert_eq!(ours, 1);
    assert_eq!(store.take_request(&second.code_hash).await.expect("take"), Some(second));
}

/// A take returns a request past its expiry.
pub async fn take_returns_expired_requests(harness: &dyn ChannelBindingStoreHarness) {
    let store = harness.open().await;
    let request = request(&channel(), "acct", -7_200, -3_600);
    store.put_request(request.clone()).await.expect("put");
    assert_eq!(store.take_request(&request.code_hash).await.expect("take"), Some(request));
}

/// `prune_requests` removes the requests expired at or before the instant, and only those.
pub async fn prune_removes_expired_requests(harness: &dyn ChannelBindingStoreHarness) {
    let store = harness.open().await;
    let channel = channel();
    let expired = request(&channel, "a", -1_000_001_000, -1_000_000_000);
    let edge = request(&channel, "b", -1_000_001_000, -999_999_000);
    let live = request(&channel, "c", 0, 1_000_000_000);
    for request in [&expired, &edge, &live] {
        store.put_request(request.clone()).await.expect("put");
    }
    let removed = store.prune_requests(edge.expires_at).await.expect("prune");
    assert!(removed >= 2, "removed {removed}");
    assert_eq!(store.get_request(&channel, "a").await.expect("get"), None);
    assert_eq!(store.take_request(&edge.code_hash).await.expect("take"), None);
    assert_eq!(store.get_request(&channel, "c").await.expect("get"), Some(live));
}

/// `list_requests` orders by request time, then channel and account.
pub async fn list_requests_orders_by_time_then_account(harness: &dyn ChannelBindingStoreHarness) {
    let store = harness.open().await;
    let channel = channel();
    let requests = [
        request(&channel, "b", 100, 4_000),
        request(&channel, "a", 100, 4_000),
        request(&channel, "c", 50, 4_000),
    ];
    for request in &requests {
        store.put_request(request.clone()).await.expect("put");
    }
    let listed: Vec<String> = store
        .list_requests()
        .await
        .expect("list")
        .into_iter()
        .filter(|request| request.channel == channel)
        .map(|request| request.account)
        .collect();
    assert_eq!(listed, ["c", "a", "b"]);
}

/// An unknown binding, request and code are `None`.
pub async fn unknown_ids(harness: &dyn ChannelBindingStoreHarness) {
    let store = harness.open().await;
    assert_eq!(store.get_binding(&fresh()).await.expect("get"), None);
    assert_eq!(store.get_request(&channel(), "nobody").await.expect("get"), None);
    assert_eq!(store.take_request(&fresh()).await.expect("take"), None);
}

/// Bindings and requests read back through a new handle.
pub async fn records_survive_a_restart(harness: &dyn ChannelBindingStoreHarness) {
    if !harness.is_durable() {
        return;
    }
    let channel = channel();
    let binding = binding(&channel, 0);
    let request = request(&channel, "acct", 0, 3_600);
    {
        let store = harness.open().await;
        store.put_binding(binding.clone()).await.expect("put");
        store.put_request(request.clone()).await.expect("put");
    }
    let store = harness.open().await;
    assert_eq!(store.get_binding(&binding.id).await.expect("get"), Some(binding));
    assert_eq!(store.get_request(&channel, "acct").await.expect("get"), Some(request));
}

/// Eight concurrent takes of one code from two handles take it once.
pub async fn concurrent_takes_take_once(harness: &dyn ChannelBindingStoreHarness) {
    let stores = two(harness).await;
    let request = request(&channel(), "acct", 0, 3_600);
    stores[0].put_request(request.clone()).await.expect("put");
    let mut takes = Vec::new();
    for index in 0..8 {
        let store = stores[index % 2].clone();
        let hash = request.code_hash.clone();
        takes.push(tokio::spawn(async move { store.take_request(&hash).await.expect("take") }));
    }
    let mut taken = 0;
    for take in takes {
        if take.await.expect("take completes").is_some() {
            taken += 1;
        }
    }
    assert_eq!(taken, 1);
}

/// Generates one test per [`channel_binding_store`](self) case for a `ChannelBindingStoreHarness` (spec 3.3 §10 K1; feature `channels`).
///
/// Used as [`process_store_conformance_tests!`](crate::process_store_conformance_tests) is;
/// see the [module documentation](crate::conformance).
#[cfg(feature = "channels")]
#[macro_export]
macro_rules! channel_binding_store_conformance_tests {
    ($(#[$attr:meta])* harness = $harness:expr $(,)?) => {
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn channel_binding_store_binding_round_trip() {
            $crate::conformance::channel_binding_store::binding_round_trip(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn channel_binding_store_put_binding_replaces() {
            $crate::conformance::channel_binding_store::put_binding_replaces(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn channel_binding_store_list_bindings_orders_by_pairing_then_id() {
            $crate::conformance::channel_binding_store::list_bindings_orders_by_pairing_then_id(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn channel_binding_store_request_round_trip() {
            $crate::conformance::channel_binding_store::request_round_trip(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn channel_binding_store_put_request_replaces_the_accounts_request() {
            $crate::conformance::channel_binding_store::put_request_replaces_the_accounts_request(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn channel_binding_store_take_returns_expired_requests() {
            $crate::conformance::channel_binding_store::take_returns_expired_requests(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn channel_binding_store_prune_removes_expired_requests() {
            $crate::conformance::channel_binding_store::prune_removes_expired_requests(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn channel_binding_store_list_requests_orders_by_time_then_account() {
            $crate::conformance::channel_binding_store::list_requests_orders_by_time_then_account(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn channel_binding_store_unknown_ids() {
            $crate::conformance::channel_binding_store::unknown_ids(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn channel_binding_store_records_survive_a_restart() {
            $crate::conformance::channel_binding_store::records_survive_a_restart(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn channel_binding_store_concurrent_takes_take_once() {
            $crate::conformance::channel_binding_store::concurrent_takes_take_once(&$harness).await;
        }
    };
}
