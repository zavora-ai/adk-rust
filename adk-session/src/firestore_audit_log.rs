//! Firestore-backed [`AuditLogStore`](adk_core::audit_log::AuditLogStore).
//!
//! # Collection layout
//!
//! | Collection (document ID) | Holds |
//! |--------------------------|-------|
//! | `adk_audit_logs` (SHA-256 of the log ID, hex) | The log ID and its `size`, the revision every append compares against |
//! | `adk_audit_logs/{log}/entries` (`seq`, zero-padded) | `seq`, `leaf_hash`, and canonical `record`; `record` is `null` once pruned |
//! | `adk_audit_logs/{log}/nodes` (`level-index`, zero-padded) | `level`, `idx`, and `hash` of a complete Merkle subtree |
//! | `adk_audit_logs/{log}/checkpoints` (`size`, zero-padded) | Log ID, `size`, `root_hash`, signed `note`, and `created_at_us` |
//!
//! Log IDs can hold `/` and are not length-limited, so the log document is addressed by
//! the ID's digest and stores the full ID; every read checks it, so two IDs that share
//! a digest never read each other's log. Sequence numbers, sizes, and node indexes are
//! 64-bit integers, hashes lowercase hex, and checkpoint times integer microseconds.
//!
//! # Atomicity
//!
//! | Operation | Mechanism |
//! |-----------|-----------|
//! | `append` | One read-write transaction reads the log document, checks the batch starts at its `size`, creates the entries (each only if absent), writes the nodes, and sets the new size |
//! | `put_checkpoint` | One read-write transaction reads the log document and the checkpoint of the same size, then writes |
//! | `prune` | Clears `record` on each entry below the position; clearing is idempotent |
//!
//! Firestore transactions are serializable: of two appends that read the same log
//! document, one commits and the other is retried, rereads the new size, and gets
//! `audit_log.seq_conflict`. Transactions read documents only by ID. Range reads and
//! listings run outside transactions on one field's range filter, which Firestore serves
//! from its automatic single-field indexes, so no composite index is needed.

use std::future::Future;
use std::ops::Range;
use std::pin::Pin;
use std::sync::Arc;

use adk_core::audit_log::{
    AuditAppend, AuditCheckpoint, AuditHash, AuditLogEntry, AuditLogStore, out_of_range_error,
    validate_append, validate_checkpoint,
};
use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result};
use async_trait::async_trait;
use chrono::DateTime;
use firestore::errors::{BackoffError, FirestoreError};
use firestore::{FirestoreDb, FirestoreTransactionOptions, FirestoreWritePrecondition};
use serde::{Deserialize, Serialize};

const LOGS: &str = "adk_audit_logs";
const ENTRIES: &str = "entries";
const NODES: &str = "nodes";
const CHECKPOINTS: &str = "checkpoints";

/// How long one operation may spend retrying conflicting transactions.
const TRANSACTION_DEADLINE: chrono::TimeDelta = chrono::TimeDelta::seconds(120);

type TxResult<T> = std::result::Result<T, BackoffError<AdkError>>;
type TxFuture<'b, T> = Pin<Box<dyn Future<Output = TxResult<T>> + Send + 'b>>;

fn storage_error(message: String) -> AdkError {
    AdkError::new(
        ErrorComponent::Session,
        ErrorCategory::Unavailable,
        "session.audit_log.storage",
        format!("firestore audit log: {message}"),
    )
}

fn corrupt(field: &str, error: impl std::fmt::Display) -> AdkError {
    storage_error(format!("field '{field}' is corrupt: {error}"))
}

fn to_i64(value: u64, what: &str) -> Result<i64> {
    i64::try_from(value).map_err(|_| storage_error(format!("{what} {value} exceeds i64")))
}

fn to_u64(value: i64, field: &str) -> Result<u64> {
    u64::try_from(value).map_err(|e| corrupt(field, e))
}

fn parse_hash(text: &str, field: &str) -> Result<AuditHash> {
    text.parse().map_err(|e| corrupt(field, e))
}

/// Retries a transaction on a failure Firestore reports as retryable, such as a
/// contention abort, and gives up on any other.
fn retry_or_fail(error: FirestoreError) -> BackoffError<AdkError> {
    let retryable = matches!(&error, FirestoreError::DatabaseError(db) if db.retry_possible);
    let error = storage_error(error.to_string());
    if retryable { BackoffError::transient(error) } else { BackoffError::permanent(error) }
}

/// Recovers the [`AdkError`] a transaction body failed with, or reports the transaction
/// failure itself.
fn transaction_error(error: FirestoreError) -> AdkError {
    match error {
        FirestoreError::ErrorInTransaction(inner) => match inner.source.downcast::<AdkError>() {
            Ok(adk_error) => *adk_error,
            Err(source) => storage_error(source.to_string()),
        },
        other => storage_error(other.to_string()),
    }
}

/// SHA-256 of the log ID in hex: path-safe and fixed length.
fn log_doc_id(log_id: &str) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write;

    Sha256::digest(log_id.as_bytes()).iter().fold(String::with_capacity(64), |mut id, byte| {
        let _ = write!(id, "{byte:02x}");
        id
    })
}

fn padded(value: u64) -> String {
    format!("{value:020}")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LogDoc {
    log_id: String,
    size: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EntryDoc {
    seq: i64,
    leaf_hash: String,
    #[serde(default)]
    record: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct NodeDoc {
    level: i64,
    idx: i64,
    hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CheckpointDoc {
    log_id: String,
    size: i64,
    root_hash: String,
    note: String,
    created_at_us: i64,
}

impl CheckpointDoc {
    fn decode(self) -> Result<AuditCheckpoint> {
        Ok(AuditCheckpoint {
            log_id: self.log_id,
            size: to_u64(self.size, "size")?,
            root_hash: parse_hash(&self.root_hash, "root_hash")?,
            note: self.note,
            created_at: DateTime::from_timestamp_micros(self.created_at_us)
                .ok_or_else(|| corrupt("created_at_us", self.created_at_us))?,
        })
    }
}

/// Reads the log document, `None` when the log was never written.
async fn read_log(
    db: &FirestoreDb,
    log_id: &str,
) -> std::result::Result<Option<LogDoc>, FirestoreError> {
    let stored: Option<LogDoc> =
        db.fluent().select().by_id_in(LOGS).obj::<LogDoc>().one(log_doc_id(log_id)).await?;
    Ok(stored)
}

/// The log's size, refusing a document that belongs to another log ID.
fn log_size(stored: Option<&LogDoc>, log_id: &str) -> Result<u64> {
    match stored {
        None => Ok(0),
        Some(doc) if doc.log_id == log_id => to_u64(doc.size, "size"),
        Some(doc) => Err(storage_error(format!(
            "log ids '{log_id}' and '{}' share a document digest",
            doc.log_id
        ))),
    }
}

/// Firestore-backed [`AuditLogStore`].
///
/// See the [module documentation](self) for the data model and how appends serialize.
///
/// # Example
///
/// ```rust,no_run
/// use adk_session::FirestoreAuditLogStore;
///
/// # async fn demo() -> adk_core::Result<()> {
/// let store = FirestoreAuditLogStore::new("my-gcp-project").await?;
/// store.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct FirestoreAuditLogStore {
    db: FirestoreDb,
}

impl std::fmt::Debug for FirestoreAuditLogStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FirestoreAuditLogStore").finish_non_exhaustive()
    }
}

impl FirestoreAuditLogStore {
    /// Connects to `project_id` with Application Default Credentials. Honors
    /// `FIRESTORE_EMULATOR_HOST`.
    ///
    /// # Errors
    ///
    /// Returns `session.audit_log.storage` when the client cannot be created.
    pub async fn new(project_id: &str) -> Result<Self> {
        let db = FirestoreDb::new(project_id)
            .await
            .map_err(|e| storage_error(format!("connection failed: {e}")))?;
        Ok(Self::from_db(db))
    }

    /// Uses an existing client, such as the one a `FirestoreSessionService` holds.
    pub fn from_db(db: FirestoreDb) -> Self {
        Self { db }
    }

    /// Prepares the store's storage.
    ///
    /// Firestore creates collections on first write and serves every query the store
    /// makes from automatic single-field indexes, so there is nothing to create; the
    /// method exists so every backend is set up the same way.
    ///
    /// # Errors
    ///
    /// Never returns an error.
    pub async fn migrate(&self) -> Result<()> {
        Ok(())
    }

    async fn read_entries(&self, log_id: &str, range: Range<u64>) -> Result<Vec<EntryDoc>> {
        let parent = self
            .db
            .parent_path(LOGS, log_doc_id(log_id))
            .map_err(|e| storage_error(e.to_string()))?;
        let start = to_i64(range.start, "seq")?;
        let end = to_i64(range.end, "seq")?;
        let mut entries: Vec<EntryDoc> = self
            .db
            .fluent()
            .select()
            .from(ENTRIES)
            .parent(&parent)
            .filter(|q| {
                q.for_all([
                    q.field("seq").greater_than_or_equal(start),
                    q.field("seq").less_than(end),
                ])
            })
            .obj::<EntryDoc>()
            .query()
            .await
            .map_err(|e| storage_error(format!("read failed: {e}")))?;
        entries.sort_by_key(|entry| entry.seq);
        Ok(entries)
    }

    async fn read_checkpoints(&self, log_id: &str) -> Result<Vec<AuditCheckpoint>> {
        let parent = self
            .db
            .parent_path(LOGS, log_doc_id(log_id))
            .map_err(|e| storage_error(e.to_string()))?;
        let docs: Vec<CheckpointDoc> = self
            .db
            .fluent()
            .select()
            .from(CHECKPOINTS)
            .parent(&parent)
            .obj::<CheckpointDoc>()
            .query()
            .await
            .map_err(|e| storage_error(format!("checkpoint read failed: {e}")))?;
        let mut checkpoints = docs
            .into_iter()
            .filter(|doc| doc.log_id == log_id)
            .map(CheckpointDoc::decode)
            .collect::<Result<Vec<_>>>()?;
        checkpoints.sort_by_key(|checkpoint| checkpoint.size);
        Ok(checkpoints)
    }
}

#[async_trait]
impl AuditLogStore for FirestoreAuditLogStore {
    async fn append(&self, batch: AuditAppend) -> Result<Range<u64>> {
        let batch = Arc::new(batch);
        self.db
            .run_transaction_with_options(
                move |db, transaction| -> TxFuture<'_, Result<Range<u64>>> {
                    let batch = batch.clone();
                    Box::pin(async move {
                        let log_id = batch.log_id.as_str();
                        let stored = read_log(&db, log_id).await.map_err(retry_or_fail)?;
                        let size =
                            log_size(stored.as_ref(), log_id).map_err(BackoffError::permanent)?;
                        let range = match validate_append(&batch, size) {
                            Ok(range) => range,
                            Err(refusal) => return Ok(Err(refusal)),
                        };
                        let doc_id = log_doc_id(log_id);
                        let parent = db.parent_path(LOGS, &doc_id).map_err(retry_or_fail)?;
                        for entry in &batch.entries {
                            let doc = EntryDoc {
                                seq: to_i64(entry.seq, "seq").map_err(BackoffError::permanent)?,
                                leaf_hash: entry.leaf_hash.to_string(),
                                record: entry.record.clone(),
                            };
                            db.fluent()
                                .update()
                                .in_col(ENTRIES)
                                .precondition(FirestoreWritePrecondition::Exists(false))
                                .document_id(padded(entry.seq))
                                .parent(&parent)
                                .object(&doc)
                                .add_to_transaction(transaction)
                                .map_err(retry_or_fail)?;
                        }
                        for node in &batch.nodes {
                            let doc = NodeDoc {
                                level: i64::from(node.level),
                                idx: to_i64(node.index, "node index")
                                    .map_err(BackoffError::permanent)?,
                                hash: node.hash.to_string(),
                            };
                            db.fluent()
                                .update()
                                .in_col(NODES)
                                .document_id(format!("{:03}-{}", node.level, padded(node.index)))
                                .parent(&parent)
                                .object(&doc)
                                .add_to_transaction(transaction)
                                .map_err(retry_or_fail)?;
                        }
                        let log = LogDoc {
                            log_id: log_id.to_string(),
                            size: to_i64(range.end, "size").map_err(BackoffError::permanent)?,
                        };
                        db.fluent()
                            .update()
                            .in_col(LOGS)
                            .document_id(&doc_id)
                            .object(&log)
                            .add_to_transaction(transaction)
                            .map_err(retry_or_fail)?;
                        Ok(Ok(range))
                    })
                },
                FirestoreTransactionOptions::new().with_max_elapsed_time(TRANSACTION_DEADLINE),
            )
            .await
            .map_err(transaction_error)?
    }

    async fn size(&self, log_id: &str) -> Result<u64> {
        let stored = read_log(&self.db, log_id)
            .await
            .map_err(|e| storage_error(format!("read failed: {e}")))?;
        log_size(stored.as_ref(), log_id)
    }

    async fn read(&self, log_id: &str, range: Range<u64>) -> Result<Vec<AuditLogEntry>> {
        let size = self.size(log_id).await?;
        if range.end > size {
            return Err(out_of_range_error(log_id, size, range.end));
        }
        if range.start >= range.end {
            return Ok(Vec::new());
        }
        let expected = range.end - range.start;
        let entries = self.read_entries(log_id, range).await?;
        if entries.len() as u64 != expected {
            return Err(storage_error(format!(
                "log '{log_id}' is missing entries: expected {expected}, found {}",
                entries.len()
            )));
        }
        entries
            .into_iter()
            .map(|doc| {
                Ok(AuditLogEntry {
                    seq: to_u64(doc.seq, "seq")?,
                    leaf_hash: parse_hash(&doc.leaf_hash, "leaf_hash")?,
                    record: doc.record,
                })
            })
            .collect()
    }

    async fn hash_at(&self, log_id: &str, level: u8, index: u64) -> Result<Option<AuditHash>> {
        let doc_id = log_doc_id(log_id);
        let parent =
            self.db.parent_path(LOGS, &doc_id).map_err(|e| storage_error(e.to_string()))?;
        let fail = |e: FirestoreError| storage_error(format!("read failed: {e}"));
        let hash = if level == 0 {
            let entry: Option<EntryDoc> = self
                .db
                .fluent()
                .select()
                .by_id_in(ENTRIES)
                .parent(&parent)
                .obj::<EntryDoc>()
                .one(padded(index))
                .await
                .map_err(fail)?;
            entry.map(|doc| doc.leaf_hash)
        } else {
            let node: Option<NodeDoc> = self
                .db
                .fluent()
                .select()
                .by_id_in(NODES)
                .parent(&parent)
                .obj::<NodeDoc>()
                .one(format!("{level:03}-{}", padded(index)))
                .await
                .map_err(fail)?;
            node.map(|doc| doc.hash)
        };
        if hash.is_some() {
            // Guard against a digest shared with another log ID.
            log_size(read_log(&self.db, log_id).await.map_err(fail)?.as_ref(), log_id)?;
        }
        hash.map(|text| parse_hash(&text, "hash")).transpose()
    }

    async fn put_checkpoint(&self, checkpoint: AuditCheckpoint) -> Result<()> {
        let checkpoint = Arc::new(checkpoint);
        self.db
            .run_transaction_with_options(
                move |db, transaction| -> TxFuture<'_, Result<()>> {
                    let checkpoint = checkpoint.clone();
                    Box::pin(async move {
                        let log_id = checkpoint.log_id.as_str();
                        let stored = read_log(&db, log_id).await.map_err(retry_or_fail)?;
                        let size =
                            log_size(stored.as_ref(), log_id).map_err(BackoffError::permanent)?;
                        let parent =
                            db.parent_path(LOGS, log_doc_id(log_id)).map_err(retry_or_fail)?;
                        let existing: Option<CheckpointDoc> = db
                            .fluent()
                            .select()
                            .by_id_in(CHECKPOINTS)
                            .parent(&parent)
                            .obj::<CheckpointDoc>()
                            .one(padded(checkpoint.size))
                            .await
                            .map_err(retry_or_fail)?;
                        let existing_root = existing
                            .map(|doc| parse_hash(&doc.root_hash, "root_hash"))
                            .transpose()
                            .map_err(BackoffError::permanent)?;
                        if let Err(refusal) = validate_checkpoint(&checkpoint, size, existing_root)
                        {
                            return Ok(Err(refusal));
                        }
                        let doc = CheckpointDoc {
                            log_id: log_id.to_string(),
                            size: to_i64(checkpoint.size, "size")
                                .map_err(BackoffError::permanent)?,
                            root_hash: checkpoint.root_hash.to_string(),
                            note: checkpoint.note.clone(),
                            created_at_us: checkpoint.created_at.timestamp_micros(),
                        };
                        db.fluent()
                            .update()
                            .in_col(CHECKPOINTS)
                            .document_id(padded(checkpoint.size))
                            .parent(&parent)
                            .object(&doc)
                            .add_to_transaction(transaction)
                            .map_err(retry_or_fail)?;
                        Ok(Ok(()))
                    })
                },
                FirestoreTransactionOptions::new().with_max_elapsed_time(TRANSACTION_DEADLINE),
            )
            .await
            .map_err(transaction_error)?
    }

    async fn latest_checkpoint(&self, log_id: &str) -> Result<Option<AuditCheckpoint>> {
        Ok(self.read_checkpoints(log_id).await?.pop())
    }

    async fn checkpoints(&self, log_id: &str) -> Result<Vec<AuditCheckpoint>> {
        self.read_checkpoints(log_id).await
    }

    async fn prune(&self, log_id: &str, before_seq: u64) -> Result<u64> {
        let size = self.size(log_id).await?;
        if before_seq > size {
            return Err(out_of_range_error(log_id, size, before_seq));
        }
        let parent = self
            .db
            .parent_path(LOGS, log_doc_id(log_id))
            .map_err(|e| storage_error(e.to_string()))?;
        let mut pruned = 0;
        for entry in self.read_entries(log_id, 0..before_seq).await? {
            if entry.record.is_none() {
                continue;
            }
            let seq = to_u64(entry.seq, "seq")?;
            let cleared = EntryDoc { record: None, ..entry };
            self.db
                .fluent()
                .update()
                .in_col(ENTRIES)
                .precondition(FirestoreWritePrecondition::Exists(true))
                .document_id(padded(seq))
                .parent(&parent)
                .object(&cleared)
                .execute::<EntryDoc>()
                .await
                .map_err(|e| storage_error(format!("prune failed: {e}")))?;
            pruned += 1;
        }
        Ok(pruned)
    }

    async fn logs(&self) -> Result<Vec<String>> {
        let docs: Vec<LogDoc> = self
            .db
            .fluent()
            .select()
            .from(LOGS)
            .obj::<LogDoc>()
            .query()
            .await
            .map_err(|e| storage_error(format!("read failed: {e}")))?;
        let mut ids: Vec<String> = docs.into_iter().map(|doc| doc.log_id).collect();
        ids.sort();
        Ok(ids)
    }
}
