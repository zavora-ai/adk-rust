//! Conformance cases for [`ControlStore`] backends (spec 3.3 §10 K3, K4).
//!
//! | Case | Contract |
//! |------|----------|
//! | [`settings_round_trip`] | An area's settings read back exactly as put: every change, off and on, what the area started with and its earlier quiet times |
//! | [`put_settings_replaces`] | A second put of one area replaces it |
//! | [`list_settings_orders_by_partition`] | `list_area_settings` orders by partition id |
//! | [`unknown_keys`] | An unknown area and an unknown viewer are `None` |
//! | [`cursor_never_moves_back`] | An advance behind the stored cursor keeps the stored one; at or past it, stores the new one |
//! | [`cursors_are_per_user_and_viewer`] | Each user's viewer has its own cursor |
//! | [`records_survive_a_restart`] | Settings and cursors read back through a new handle (durable backends only) |
//! | [`concurrent_advances_keep_the_highest`] | Sixteen concurrent advances of one cursor from two handles leave the highest |
//! | [`update_changes_one_setting`] | `update_area_settings` creates an area's record, then changes one setting and keeps the others |
//! | [`concurrent_updates_all_hold`] | Sixteen concurrent updates of one area from two handles all hold |
//!
//! Every case writes under fresh partitions, users and viewers, so a harness may point
//! every case at one shared database.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};

use crate::control::{AreaSettings, AreaStart, AwayCursor, Change, ControlStore};
use crate::partition::{PartitionId, PartitionMode};
use crate::sched::QuietHours;

/// Opens a [`ControlStore`] backend for the conformance suite.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_kernel::conformance::ControlStoreHarness;
/// use adk_kernel::control::{ControlStore, InMemoryControlStore};
///
/// struct InMemory;
///
/// #[async_trait::async_trait]
/// impl ControlStoreHarness for InMemory {
///     async fn open(&self) -> Arc<dyn ControlStore> {
///         Arc::new(InMemoryControlStore::new())
///     }
///
///     fn is_durable(&self) -> bool {
///         false
///     }
/// }
/// ```
#[async_trait]
pub trait ControlStoreHarness: Send + Sync {
    /// Opens a store. For a durable backend every call returns a new handle over the same
    /// stored data. Panics when the backend cannot be opened.
    async fn open(&self) -> Arc<dyn ControlStore>;

    /// Whether data written through one handle is visible through the next.
    fn is_durable(&self) -> bool {
        true
    }
}

/// Two handles over one store: two opens of a durable backend, one store twice otherwise.
async fn two(harness: &dyn ControlStoreHarness) -> [Arc<dyn ControlStore>; 2] {
    if harness.is_durable() {
        [harness.open().await, harness.open().await]
    } else {
        let store = harness.open().await;
        [store.clone(), store]
    }
}

/// A fresh name with a character outside ASCII, so a backend that mangles text is caught.
fn fresh(prefix: &str) -> String {
    format!("{prefix}-{}-ü", uuid::Uuid::new_v4().simple())
}

/// An instant with microseconds, so a backend that rounds timestamps is caught.
fn instant(seconds: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_800_000_000 + seconds, 123_456_000).single().expect("valid instant")
}

fn settings(partition: &PartitionId) -> AreaSettings {
    let window = "22:00-07:00".parse::<QuietHours>().expect("window").with_utc_offset_minutes(180);
    let mut settings = AreaSettings::new(partition.clone())
        .with_quiet_time(Change::new(Some(window), "ana \"home\"", instant(1)))
        .with_daily_spending_limit(Change::new(None, "ana", instant(2)))
        .with_mode(Change::new(PartitionMode::Ask, "ana", instant(3)))
        .with_start(AreaStart::new(vec![window], Some(5_000_000), Some(PartitionMode::Auto)));
    settings.earlier_quiet_time.push(Change::new(None, "ana", instant(0)));
    settings
}

/// An area's settings read back exactly as put.
pub async fn settings_round_trip(harness: &dyn ControlStoreHarness) {
    let store = harness.open().await;
    let partition = PartitionId::new(fresh("area"));
    let settings = settings(&partition);
    store.put_area_settings(settings.clone()).await.expect("put");
    assert_eq!(store.area_settings(&partition).await.expect("get"), Some(settings));
}

/// A second put of one area replaces it.
pub async fn put_settings_replaces(harness: &dyn ControlStoreHarness) {
    let store = harness.open().await;
    let partition = PartitionId::new(fresh("area"));
    store.put_area_settings(settings(&partition)).await.expect("put");
    let replaced = AreaSettings::new(partition.clone()).with_daily_spending_limit(Change::new(
        Some(5_000_000),
        "ana",
        instant(9),
    ));
    store.put_area_settings(replaced.clone()).await.expect("put again");
    assert_eq!(store.area_settings(&partition).await.expect("get"), Some(replaced));
}

/// `list_area_settings` orders by partition id.
pub async fn list_settings_orders_by_partition(harness: &dyn ControlStoreHarness) {
    let store = harness.open().await;
    let stem = fresh("order");
    let ids = [format!("{stem}-b"), format!("{stem}-a"), format!("{stem}-c")];
    for id in &ids {
        store.put_area_settings(AreaSettings::new(id.as_str())).await.expect("put");
    }
    let listed: Vec<String> = store
        .list_area_settings()
        .await
        .expect("list")
        .into_iter()
        .map(|settings| settings.partition.as_str().to_string())
        .filter(|id| id.starts_with(&stem))
        .collect();
    let mut sorted = ids.to_vec();
    sorted.sort();
    assert_eq!(listed, sorted);
    let all: Vec<String> = store
        .list_area_settings()
        .await
        .expect("list")
        .into_iter()
        .map(|settings| settings.partition.as_str().to_string())
        .collect();
    let mut ordered = all.clone();
    ordered.sort();
    assert_eq!(all, ordered, "every area is ordered by id");
}

/// An unknown area and an unknown viewer are `None`.
pub async fn unknown_keys(harness: &dyn ControlStoreHarness) {
    let store = harness.open().await;
    assert_eq!(store.area_settings(&PartitionId::new(fresh("none"))).await.expect("get"), None);
    assert_eq!(store.cursor(&fresh("user"), "home").await.expect("get"), None);
}

/// An advance behind the stored cursor keeps the stored one.
pub async fn cursor_never_moves_back(harness: &dyn ControlStoreHarness) {
    let store = harness.open().await;
    let user = fresh("user");
    let first = AwayCursor::new(user.clone(), "device:0a1b", 40, instant(10));
    assert_eq!(store.advance_cursor(first.clone()).await.expect("advance"), first);
    let back = AwayCursor::new(user.clone(), "device:0a1b", 12, instant(20));
    assert_eq!(store.advance_cursor(back).await.expect("advance"), first, "never back");
    let again = AwayCursor::new(user.clone(), "device:0a1b", 40, instant(30));
    assert_eq!(
        store.advance_cursor(again.clone()).await.expect("advance"),
        again,
        "same row, seen later"
    );
    let on = AwayCursor::new(user.clone(), "device:0a1b", 41, instant(40));
    assert_eq!(store.advance_cursor(on.clone()).await.expect("advance"), on);
    assert_eq!(store.cursor(&user, "device:0a1b").await.expect("get"), Some(on));
}

/// Each user's viewer has its own cursor.
pub async fn cursors_are_per_user_and_viewer(harness: &dyn ControlStoreHarness) {
    let store = harness.open().await;
    let (ana, bo) = (fresh("ana"), fresh("bo"));
    let cursors = [
        AwayCursor::new(ana.clone(), "home", 5, instant(1)),
        AwayCursor::new(ana.clone(), "terminal", 9, instant(2)),
        AwayCursor::new(bo.clone(), "home", 7, instant(3)),
    ];
    for cursor in &cursors {
        store.advance_cursor(cursor.clone()).await.expect("advance");
    }
    for cursor in cursors {
        assert_eq!(store.cursor(&cursor.user, &cursor.viewer).await.expect("get"), Some(cursor));
    }
}

/// Settings and cursors read back through a new handle.
pub async fn records_survive_a_restart(harness: &dyn ControlStoreHarness) {
    if !harness.is_durable() {
        return;
    }
    let partition = PartitionId::new(fresh("area"));
    let cursor = AwayCursor::new(fresh("user"), "home", 77, instant(5));
    {
        let store = harness.open().await;
        store.put_area_settings(settings(&partition)).await.expect("put");
        store.advance_cursor(cursor.clone()).await.expect("advance");
    }
    let store = harness.open().await;
    assert_eq!(store.area_settings(&partition).await.expect("get"), Some(settings(&partition)));
    assert_eq!(store.cursor(&cursor.user, "home").await.expect("get"), Some(cursor));
}

/// Sixteen concurrent advances of one cursor from two handles leave the highest.
pub async fn concurrent_advances_keep_the_highest(harness: &dyn ControlStoreHarness) {
    let stores = two(harness).await;
    let user = fresh("user");
    let mut advances = Vec::new();
    for (index, seq) in
        [3, 14, 1, 15, 9, 2, 6, 5, 13, 8, 12, 4, 10, 16, 7, 11].into_iter().enumerate()
    {
        let store = stores[index % 2].clone();
        let cursor = AwayCursor::new(user.clone(), "home", seq, instant(seq.cast_signed()));
        advances.push(tokio::spawn(
            async move { store.advance_cursor(cursor).await.expect("advance") },
        ));
    }
    for advance in advances {
        advance.await.expect("advance completes");
    }
    let stored = stores[0].cursor(&user, "home").await.expect("get").expect("a cursor");
    assert_eq!(stored.seq, 16);
}

/// `update_area_settings` creates an area's record, then changes one setting and keeps the
/// others.
pub async fn update_changes_one_setting(harness: &dyn ControlStoreHarness) {
    let store = harness.open().await;
    let partition = PartitionId::new(fresh("area"));
    let plan = Change::new(PartitionMode::Plan, "ana", instant(4));
    let stored = store
        .update_area_settings(&partition, &|settings: &mut AreaSettings| {
            settings.mode = Some(plan.clone());
        })
        .await
        .expect("update");
    let created = AreaSettings::new(partition.clone()).with_mode(plan.clone());
    assert_eq!(stored, created);
    assert_eq!(store.area_settings(&partition).await.expect("get"), Some(created));

    store.put_area_settings(settings(&partition)).await.expect("put");
    let limit = Change::new(Some(7_000_000), "ana", instant(6));
    let stored = store
        .update_area_settings(&partition, &|settings: &mut AreaSettings| {
            settings.daily_spending_limit = Some(limit.clone());
        })
        .await
        .expect("update");
    let changed = settings(&partition).with_daily_spending_limit(limit);
    assert_eq!(stored, changed);
    assert_eq!(store.area_settings(&partition).await.expect("get"), Some(changed));
}

/// Sixteen concurrent updates of one area from two handles all hold: none is lost to
/// another written over it.
pub async fn concurrent_updates_all_hold(harness: &dyn ControlStoreHarness) {
    let stores = two(harness).await;
    let partition = PartitionId::new(fresh("area"));
    let mut updates = Vec::new();
    for index in 0..16_i64 {
        let store = stores[usize::try_from(index).expect("small") % 2].clone();
        let partition = partition.clone();
        updates.push(tokio::spawn(async move {
            let change = Change::new(None, format!("person-{index}"), instant(index));
            store
                .update_area_settings(&partition, &|settings: &mut AreaSettings| {
                    settings.earlier_quiet_time.push(change.clone());
                })
                .await
                .expect("update")
        }));
    }
    for update in updates {
        update.await.expect("update completes");
    }
    let stored = stores[0].area_settings(&partition).await.expect("get").expect("a record");
    let mut by: Vec<String> =
        stored.earlier_quiet_time.iter().map(|change| change.by.clone()).collect();
    by.sort();
    let mut expected: Vec<String> = (0..16).map(|index| format!("person-{index}")).collect();
    expected.sort();
    assert_eq!(by, expected);
}

/// Generates one test per [`control_store`](self) case for a `ControlStoreHarness` (spec
/// 3.3 §10 K3, K4).
///
/// Used as [`process_store_conformance_tests!`](crate::process_store_conformance_tests) is;
/// see the [module documentation](crate::conformance).
#[macro_export]
macro_rules! control_store_conformance_tests {
    ($(#[$attr:meta])* harness = $harness:expr $(,)?) => {
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn control_store_settings_round_trip() {
            $crate::conformance::control_store::settings_round_trip(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn control_store_put_settings_replaces() {
            $crate::conformance::control_store::put_settings_replaces(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn control_store_list_settings_orders_by_partition() {
            $crate::conformance::control_store::list_settings_orders_by_partition(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn control_store_unknown_keys() {
            $crate::conformance::control_store::unknown_keys(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn control_store_cursor_never_moves_back() {
            $crate::conformance::control_store::cursor_never_moves_back(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn control_store_cursors_are_per_user_and_viewer() {
            $crate::conformance::control_store::cursors_are_per_user_and_viewer(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn control_store_records_survive_a_restart() {
            $crate::conformance::control_store::records_survive_a_restart(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn control_store_concurrent_advances_keep_the_highest() {
            $crate::conformance::control_store::concurrent_advances_keep_the_highest(&$harness)
                .await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn control_store_update_changes_one_setting() {
            $crate::conformance::control_store::update_changes_one_setting(&$harness).await;
        }
        $(#[$attr])*
        #[::tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn control_store_concurrent_updates_all_hold() {
            $crate::conformance::control_store::concurrent_updates_all_hold(&$harness).await;
        }
    };
}
