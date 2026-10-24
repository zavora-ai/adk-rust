//! Conformance cases for [`CronWatermarkStore`] backends (spec 3.2 §7.4, §11, §13 Q8).
//!
//! | Case | Contract |
//! |------|----------|
//! | [`unknown_source`] | `read` of a source that never ran is `None`, and `delete` answers `false` |
//! | [`advance_then_read`] | `advance` stores a cursor to the microsecond and returns it; sources with `:`, `/`, spaces and non-ASCII characters keep their own cursors |
//! | [`never_moves_back`] | An earlier cursor leaves the stored one where it is, and `advance` returns the stored one |
//! | [`delete`] | `delete` removes the cursor and answers `true`, then `false`; other sources keep theirs |
//! | [`cursors_survive_a_restart`] | A cursor reads back through a new handle (durable backends only) |
//! | [`concurrent_advances_keep_the_latest`] | Advances from two handles in any order end at the latest cursor |
//!
//! Every case writes under fresh source names, so a harness may point every case at one
//! shared database.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};

use crate::watermark::CronWatermarkStore;

/// Opens a [`CronWatermarkStore`] backend for the conformance suite.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_kernel::conformance::CronWatermarkStoreHarness;
/// use adk_kernel::CronWatermarkStore;
/// use adk_kernel::stores::InMemoryCronWatermarkStore;
///
/// struct InMemory;
///
/// #[async_trait::async_trait]
/// impl CronWatermarkStoreHarness for InMemory {
///     async fn open(&self) -> Arc<dyn CronWatermarkStore> {
///         Arc::new(InMemoryCronWatermarkStore::new())
///     }
///
///     fn is_durable(&self) -> bool {
///         false
///     }
/// }
/// ```
#[async_trait]
pub trait CronWatermarkStoreHarness: Send + Sync {
    /// Opens a store. For a durable backend every call returns a new handle over the same
    /// stored data. Panics when the backend cannot be opened.
    async fn open(&self) -> Arc<dyn CronWatermarkStore>;

    /// Whether data written through one handle is visible through the next. `false` skips
    /// the restart case and runs the concurrency case on a single handle.
    fn is_durable(&self) -> bool {
        true
    }
}

/// A fresh source name with the characters a backend must keep: `:`, `/`, a space,
/// non-ASCII.
fn source(prefix: &str) -> String {
    format!("conformance:{prefix}/0 0 * * * */ü-日本/{}", uuid::Uuid::new_v4())
}

/// An instant with microseconds, so a backend that keeps less fails.
fn instant(seconds: i64, micros: u32) -> DateTime<Utc> {
    Utc.timestamp_opt(1_800_000_000 + seconds, micros * 1_000).single().expect("valid instant")
}

/// A second handle for a durable backend, else the first.
async fn other(
    harness: &impl CronWatermarkStoreHarness,
    first: &Arc<dyn CronWatermarkStore>,
) -> Arc<dyn CronWatermarkStore> {
    if harness.is_durable() { harness.open().await } else { Arc::clone(first) }
}

/// `read` of a source that never ran is `None`, and `delete` answers `false`.
pub async fn unknown_source(harness: &impl CronWatermarkStoreHarness) {
    let store = harness.open().await;
    let name = source("unknown");
    assert_eq!(store.read(&name).await.expect("read"), None);
    assert!(!store.delete(&name).await.expect("delete"));
}

/// `advance` stores a cursor to the microsecond and returns it; each source keeps its own.
pub async fn advance_then_read(harness: &impl CronWatermarkStoreHarness) {
    let store = harness.open().await;
    let (a, b) = (source("a"), source("b"));
    let at = instant(10, 123_456);
    assert_eq!(store.advance(&a, at).await.expect("advance"), at);
    assert_eq!(store.read(&a).await.expect("read"), Some(at));
    assert_eq!(store.read(&b).await.expect("read"), None, "another source has no cursor");
    let later = instant(70, 1);
    assert_eq!(store.advance(&b, later).await.expect("advance"), later);
    assert_eq!(store.read(&a).await.expect("read"), Some(at), "a keeps its own cursor");
}

/// An earlier cursor leaves the stored one where it is.
pub async fn never_moves_back(harness: &impl CronWatermarkStoreHarness) {
    let store = harness.open().await;
    let name = source("back");
    let (early, late) = (instant(0, 0), instant(3_600, 500));
    store.advance(&name, late).await.expect("advance");
    assert_eq!(store.advance(&name, early).await.expect("advance"), late);
    assert_eq!(store.read(&name).await.expect("read"), Some(late));
    assert_eq!(store.advance(&name, late).await.expect("advance"), late, "the same cursor again");
    let later = instant(7_200, 0);
    assert_eq!(store.advance(&name, later).await.expect("advance"), later);
}

/// `delete` removes the cursor and answers `true`, then `false`.
pub async fn delete(harness: &impl CronWatermarkStoreHarness) {
    let store = harness.open().await;
    let (gone, kept) = (source("gone"), source("kept"));
    store.advance(&gone, instant(5, 0)).await.expect("advance");
    store.advance(&kept, instant(6, 0)).await.expect("advance");
    assert!(store.delete(&gone).await.expect("delete"));
    assert!(!store.delete(&gone).await.expect("delete"));
    assert_eq!(store.read(&gone).await.expect("read"), None);
    assert_eq!(store.read(&kept).await.expect("read"), Some(instant(6, 0)));
}

/// A cursor reads back through a new handle.
pub async fn cursors_survive_a_restart(harness: &impl CronWatermarkStoreHarness) {
    if !harness.is_durable() {
        return;
    }
    let name = source("restart");
    let at = instant(42, 999_999);
    harness.open().await.advance(&name, at).await.expect("advance");
    assert_eq!(harness.open().await.read(&name).await.expect("read"), Some(at));
}

/// Advances from two handles in any order end at the latest cursor.
pub async fn concurrent_advances_keep_the_latest(harness: &impl CronWatermarkStoreHarness) {
    let first = harness.open().await;
    let second = other(harness, &first).await;
    let name = source("concurrent");
    let cursors: Vec<DateTime<Utc>> = (0..40).map(|step| instant(step * 60, 7)).collect();
    let latest = *cursors.last().expect("cursors");
    let mut tasks = Vec::new();
    for (index, cursor) in cursors.into_iter().rev().enumerate() {
        let store = if index % 2 == 0 { Arc::clone(&first) } else { Arc::clone(&second) };
        let name = name.clone();
        tasks.push(tokio::spawn(async move { store.advance(&name, cursor).await }));
    }
    for task in tasks {
        task.await.expect("task").expect("advance");
    }
    assert_eq!(first.read(&name).await.expect("read"), Some(latest));
    assert_eq!(second.read(&name).await.expect("read"), Some(latest));
}
