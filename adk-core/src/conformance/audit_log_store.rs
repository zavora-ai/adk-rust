//! Conformance cases for [`AuditLogStore`] backends.
//!
//! | Case | Contract |
//! |------|----------|
//! | [`append_assigns_contiguous_sequences`] | Batches occupy consecutive ranges and read back unchanged |
//! | [`append_refuses_a_wrong_start`] | A gap or an overlap is `audit_log.seq_conflict` and writes nothing |
//! | [`append_refuses_invalid_batches`] | Empty, non-contiguous, pruned-entry, and out-of-range-node batches are `audit_log.invalid_batch` |
//! | [`concurrent_appends_have_one_winner`] | Two handles appending at the same position: exactly one succeeds |
//! | [`logs_are_independent`] | Two `log_id`s keep separate sequences, hashes, and checkpoints |
//! | [`logs_sort_by_bytes`] | `logs()` orders ids by their UTF-8 bytes, whatever the backend's locale |
//! | [`hash_at_returns_leaves_and_nodes`] | Level 0 returns leaf hashes, higher levels the stored nodes, absent ones `None` |
//! | [`reads_past_the_end_fail`] | `read` past the log's size is `audit_log.out_of_range` |
//! | [`checkpoints_never_fork`] | Checkpoints list by size, never exceed the log, refuse a second root, and accept a re-signed note |
//! | [`prune_keeps_hashes`] | Pruning drops bodies below a position and keeps every hash |
//! | [`logs_survive_a_restart`] | A new handle sees every entry, node, and checkpoint |

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use super::unique;
use crate::audit_log::{
    AuditAppend, AuditCheckpoint, AuditHash, AuditLogEntry, AuditLogStore, AuditTreeNode,
};

/// Opens an [`AuditLogStore`] backend for the conformance suite.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::async_trait;
/// use adk_core::audit_log::{AuditLogStore, InMemoryAuditLogStore};
/// use adk_core::conformance::AuditLogStoreHarness;
///
/// #[derive(Default)]
/// struct InMemory(InMemoryAuditLogStore);
///
/// #[async_trait]
/// impl AuditLogStoreHarness for InMemory {
///     async fn open(&self) -> Arc<dyn AuditLogStore> {
///         Arc::new(self.0.clone())
///     }
/// }
/// ```
#[async_trait]
pub trait AuditLogStoreHarness: Send + Sync {
    /// Opens the store.
    ///
    /// For a durable backend every call returns a new handle over the same stored data,
    /// as a restarted process or a second process would see it. Panics when the backend
    /// cannot be opened.
    async fn open(&self) -> Arc<dyn AuditLogStore>;
}

fn hash(seed: u64) -> AuditHash {
    let mut bytes = [0u8; 32];
    bytes[..8].copy_from_slice(&seed.to_be_bytes());
    bytes[31] = 0xa5;
    AuditHash(bytes)
}

fn entries(range: std::ops::Range<u64>) -> Vec<AuditLogEntry> {
    range
        .map(|seq| AuditLogEntry {
            seq,
            leaf_hash: hash(seq),
            record: Some(format!("{{\"seq\":{seq},\"text\":\"é ✓\"}}")),
        })
        .collect()
}

fn batch(log_id: &str, range: std::ops::Range<u64>, nodes: Vec<AuditTreeNode>) -> AuditAppend {
    AuditAppend { log_id: log_id.to_string(), entries: entries(range), nodes }
}

fn node(level: u8, index: u64) -> AuditTreeNode {
    AuditTreeNode { level, index, hash: hash(1_000 + u64::from(level) * 100 + index) }
}

fn whole_millisecond(at: DateTime<Utc>) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(at.timestamp_millis()).expect("in range")
}

fn checkpoint(log_id: &str, size: u64, root: AuditHash, note: &str) -> AuditCheckpoint {
    AuditCheckpoint {
        log_id: log_id.to_string(),
        size,
        root_hash: root,
        note: note.to_string(),
        created_at: whole_millisecond(Utc::now()),
    }
}

fn assert_code(result: crate::Result<impl std::fmt::Debug>, code: &str) {
    match result {
        Ok(value) => panic!("expected {code}, got Ok({value:?})"),
        Err(error) => assert_eq!(error.code, code, "unexpected error: {error}"),
    }
}

/// Batches occupy consecutive ranges and read back unchanged.
pub async fn append_assigns_contiguous_sequences(harness: &impl AuditLogStoreHarness) {
    let store = harness.open().await;
    let log = unique("audit");
    assert_eq!(store.size(&log).await.unwrap(), 0);
    assert_eq!(store.append(batch(&log, 0..3, vec![node(1, 0)])).await.unwrap(), 0..3);
    assert_eq!(store.append(batch(&log, 3..4, vec![node(1, 1), node(2, 0)])).await.unwrap(), 3..4);
    assert_eq!(store.size(&log).await.unwrap(), 4);
    assert_eq!(store.read(&log, 0..4).await.unwrap(), entries(0..4));
    assert_eq!(store.read(&log, 1..3).await.unwrap(), entries(1..3));
    assert_eq!(store.read(&log, 2..2).await.unwrap(), Vec::new());
}

/// A gap or an overlap is `audit_log.seq_conflict` and writes nothing.
pub async fn append_refuses_a_wrong_start(harness: &impl AuditLogStoreHarness) {
    let store = harness.open().await;
    let log = unique("audit");
    store.append(batch(&log, 0..2, vec![node(1, 0)])).await.unwrap();
    assert_code(store.append(batch(&log, 3..4, vec![])).await, "audit_log.seq_conflict");
    assert_code(store.append(batch(&log, 1..3, vec![])).await, "audit_log.seq_conflict");
    assert_eq!(store.size(&log).await.unwrap(), 2);
    assert_eq!(store.read(&log, 0..2).await.unwrap(), entries(0..2));
}

/// Empty, non-contiguous, pruned-entry, and out-of-range-node batches are refused.
pub async fn append_refuses_invalid_batches(harness: &impl AuditLogStoreHarness) {
    let store = harness.open().await;
    let log = unique("audit");
    assert_code(store.append(batch(&log, 0..0, vec![])).await, "audit_log.invalid_batch");

    let mut gapped = batch(&log, 0..3, vec![]);
    gapped.entries.remove(1);
    assert_code(store.append(gapped).await, "audit_log.invalid_batch");

    let mut pruned = batch(&log, 0..1, vec![]);
    pruned.entries[0].record = None;
    assert_code(store.append(pruned).await, "audit_log.invalid_batch");

    // Leaves 0..1 cannot complete the level-1 node over leaves 0..2.
    assert_code(store.append(batch(&log, 0..1, vec![node(1, 0)])).await, "audit_log.invalid_batch");
    // Level 0 lives with the entries.
    assert_code(store.append(batch(&log, 0..1, vec![node(0, 0)])).await, "audit_log.invalid_batch");
    assert_eq!(store.size(&log).await.unwrap(), 0);

    store.append(batch(&log, 0..2, vec![node(1, 0)])).await.unwrap();
    // A node wholly before the batch was completed by an earlier append.
    assert_code(store.append(batch(&log, 2..4, vec![node(1, 0)])).await, "audit_log.invalid_batch");
    assert_eq!(store.size(&log).await.unwrap(), 2);
}

/// Two handles appending at the same position: exactly one succeeds.
pub async fn concurrent_appends_have_one_winner(harness: &impl AuditLogStoreHarness) {
    let first = harness.open().await;
    let second = harness.open().await;
    let log = unique("audit");
    first.append(batch(&log, 0..1, vec![])).await.unwrap();
    for round in 1..6u64 {
        let mut a = batch(&log, round..round + 1, vec![]);
        a.entries[0].leaf_hash = hash(10_000 + round);
        let b = batch(&log, round..round + 1, vec![]);
        let (left, right) = tokio::join!(first.append(a), second.append(b));
        let winners = usize::from(left.is_ok()) + usize::from(right.is_ok());
        assert_eq!(winners, 1, "round {round}: {left:?} / {right:?}");
        for result in [left, right] {
            if let Err(error) = result {
                assert_eq!(error.code, "audit_log.seq_conflict", "round {round}: {error}");
            }
        }
    }
    assert_eq!(first.size(&log).await.unwrap(), 6);
}

/// Two `log_id`s keep separate sequences, hashes, and checkpoints.
pub async fn logs_are_independent(harness: &impl AuditLogStoreHarness) {
    let store = harness.open().await;
    let a = unique("audit-a");
    let b = unique("audit-b");
    store.append(batch(&a, 0..2, vec![node(1, 0)])).await.unwrap();
    store.append(batch(&b, 0..1, vec![])).await.unwrap();
    store.put_checkpoint(checkpoint(&a, 2, hash(7), "a")).await.unwrap();
    assert_eq!(store.size(&a).await.unwrap(), 2);
    assert_eq!(store.size(&b).await.unwrap(), 1);
    assert_eq!(store.hash_at(&b, 1, 0).await.unwrap(), None);
    assert_eq!(store.latest_checkpoint(&b).await.unwrap(), None);
    let logs = store.logs().await.unwrap();
    assert!(logs.contains(&a) && logs.contains(&b), "{logs:?}");
    let mut sorted = logs.clone();
    sorted.sort();
    assert_eq!(logs, sorted);
}

/// `logs()` orders ids by their UTF-8 bytes, as `Vec<String>::sort` does, so a backend whose
/// default collation is locale-aware (ICU, glibc) must order by bytes explicitly.
pub async fn logs_sort_by_bytes(harness: &impl AuditLogStoreHarness) {
    let store = harness.open().await;
    let base = unique("audit-order");
    let ids: Vec<String> =
        ["a-1", "B-2", "_c", ":é/x", "b"].iter().map(|suffix| format!("{base}{suffix}")).collect();
    for id in &ids {
        store.append(batch(id, 0..1, vec![])).await.unwrap();
    }
    let ours: Vec<String> =
        store.logs().await.unwrap().into_iter().filter(|id| id.starts_with(&base)).collect();
    let mut expected = ids.clone();
    expected.sort();
    assert_eq!(ours, expected);
}

/// Level 0 returns leaf hashes, higher levels the stored nodes, absent ones `None`.
pub async fn hash_at_returns_leaves_and_nodes(harness: &impl AuditLogStoreHarness) {
    let store = harness.open().await;
    let log = unique("audit");
    assert_eq!(store.hash_at(&log, 0, 0).await.unwrap(), None);
    store.append(batch(&log, 0..4, vec![node(1, 0), node(1, 1), node(2, 0)])).await.unwrap();
    store.append(batch(&log, 4..5, vec![])).await.unwrap();
    for seq in 0..5 {
        assert_eq!(store.hash_at(&log, 0, seq).await.unwrap(), Some(hash(seq)));
    }
    assert_eq!(store.hash_at(&log, 0, 5).await.unwrap(), None);
    assert_eq!(store.hash_at(&log, 1, 1).await.unwrap(), Some(node(1, 1).hash));
    assert_eq!(store.hash_at(&log, 2, 0).await.unwrap(), Some(node(2, 0).hash));
    assert_eq!(store.hash_at(&log, 1, 2).await.unwrap(), None);
    assert_eq!(store.hash_at(&log, 3, 0).await.unwrap(), None);
}

/// `read` past the log's size is `audit_log.out_of_range`.
pub async fn reads_past_the_end_fail(harness: &impl AuditLogStoreHarness) {
    let store = harness.open().await;
    let log = unique("audit");
    assert_code(store.read(&log, 0..1).await, "audit_log.out_of_range");
    store.append(batch(&log, 0..2, vec![node(1, 0)])).await.unwrap();
    assert_code(store.read(&log, 1..3).await, "audit_log.out_of_range");
    assert_code(store.prune(&log, 3).await, "audit_log.out_of_range");
}

/// Checkpoints list by size, never exceed the log, refuse a second root, and accept a
/// re-signed note for the same root.
pub async fn checkpoints_never_fork(harness: &impl AuditLogStoreHarness) {
    let store = harness.open().await;
    let log = unique("audit");
    store.append(batch(&log, 0..4, vec![node(1, 0), node(1, 1), node(2, 0)])).await.unwrap();
    assert_code(
        store.put_checkpoint(checkpoint(&log, 5, hash(50), "beyond")).await,
        "audit_log.out_of_range",
    );
    let four = checkpoint(&log, 4, hash(40), "four");
    let two = checkpoint(&log, 2, hash(20), "two");
    store.put_checkpoint(four.clone()).await.unwrap();
    store.put_checkpoint(two.clone()).await.unwrap();
    assert_eq!(store.latest_checkpoint(&log).await.unwrap(), Some(four.clone()));
    assert_eq!(store.checkpoints(&log).await.unwrap(), vec![two.clone(), four.clone()]);

    assert_code(
        store.put_checkpoint(checkpoint(&log, 4, hash(41), "fork")).await,
        "audit_log.checkpoint_conflict",
    );
    let cosigned = checkpoint(&log, 4, hash(40), "four, cosigned");
    store.put_checkpoint(cosigned.clone()).await.unwrap();
    assert_eq!(store.checkpoints(&log).await.unwrap(), vec![two, cosigned]);
}

/// Pruning drops bodies below a position and keeps every hash.
pub async fn prune_keeps_hashes(harness: &impl AuditLogStoreHarness) {
    let store = harness.open().await;
    let log = unique("audit");
    store.append(batch(&log, 0..4, vec![node(1, 0), node(1, 1), node(2, 0)])).await.unwrap();
    assert_eq!(store.prune(&log, 3).await.unwrap(), 3);
    assert_eq!(store.prune(&log, 3).await.unwrap(), 0);
    let read = store.read(&log, 0..4).await.unwrap();
    let mut expected = entries(0..4);
    for entry in &mut expected[..3] {
        entry.record = None;
    }
    assert_eq!(read, expected);
    assert_eq!(store.hash_at(&log, 0, 1).await.unwrap(), Some(hash(1)));
    assert_eq!(store.hash_at(&log, 2, 0).await.unwrap(), Some(node(2, 0).hash));
    store.append(batch(&log, 4..5, vec![])).await.unwrap();
}

/// A new handle sees every entry, node, and checkpoint.
pub async fn logs_survive_a_restart(harness: &impl AuditLogStoreHarness) {
    let log = unique("audit");
    let written = checkpoint(&log, 3, hash(30), "three");
    {
        let store = harness.open().await;
        store.append(batch(&log, 0..3, vec![node(1, 0)])).await.unwrap();
        store.put_checkpoint(written.clone()).await.unwrap();
    }
    let store = harness.open().await;
    assert_eq!(store.size(&log).await.unwrap(), 3);
    assert_eq!(store.read(&log, 0..3).await.unwrap(), entries(0..3));
    assert_eq!(store.hash_at(&log, 1, 0).await.unwrap(), Some(node(1, 0).hash));
    assert_eq!(store.latest_checkpoint(&log).await.unwrap(), Some(written));
    store.append(batch(&log, 3..4, vec![node(1, 1), node(2, 0)])).await.unwrap();
}
