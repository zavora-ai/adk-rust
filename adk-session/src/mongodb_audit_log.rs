//! MongoDB-backed [`AuditLogStore`](adk_core::audit_log::AuditLogStore).
//!
//! Works on a standalone `mongod` as well as on a replica set or sharded cluster: no
//! operation needs a multi-document transaction.
//!
//! | Collection | Key | Holds |
//! |------------|-----|-------|
//! | `adk_audit_logs` | `_id` = log id | `size`, the committed record count, and `pending`, an append in progress |
//! | `adk_audit_entries` | `log_id`, `seq` (unique) | Leaf hash and canonical record; the record is `null` once pruned |
//! | `adk_audit_nodes` | `log_id`, `level`, `idx` (unique) | Hashes of complete Merkle subtrees |
//! | `adk_audit_checkpoints` | `log_id`, `size` (unique) | Root hash, signed note, and creation time in microseconds |
//!
//! # Appends
//!
//! An append claims the log with one conditional update,
//! `{_id: log, size: <read>, pending: null}` → `{$set: {pending: <batch>}}`, which
//! parks the whole batch on the log document. Only one writer can claim a given size,
//! so a second writer at the same position fails the claim, rereads the log, and gets
//! `audit_log.seq_conflict`. The winner then writes the entries and nodes and commits
//! with `{_id: log, "pending.token": <token>}` → `{$set: {size: <end>, pending: null}}`.
//!
//! | Failure | Effect |
//! |---------|--------|
//! | A writer dies after its claim | The next append, from any process, writes the parked batch and commits it before deciding its own |
//! | Two processes finish one parked batch | Entry and node writes are idempotent upserts, and only one commit matches the token |
//! | A reader runs during an append | Reads are bounded by `size`, so entries of an uncommitted batch are never returned |
//!
//! A parked batch is one BSON document, so one append holds at most about 16 MB.

use std::ops::Range;

use adk_core::audit_log::{
    AuditAppend, AuditCheckpoint, AuditHash, AuditLogEntry, AuditLogStore, out_of_range_error,
    seq_conflict_error, validate_append, validate_checkpoint,
};
use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result};
use async_trait::async_trait;
use chrono::DateTime;
use mongodb::bson::{Bson, Document, doc};
use mongodb::error::{ErrorKind, WriteFailure};
use mongodb::options::{ClientOptions, FindOptions, IndexOptions, UpdateOptions};
use mongodb::{Client, Collection, Database, IndexModel};

const LOGS: &str = "adk_audit_logs";
const ENTRIES: &str = "adk_audit_entries";
const NODES: &str = "adk_audit_nodes";
const CHECKPOINTS: &str = "adk_audit_checkpoints";

/// MongoDB's duplicate key error code.
const DUPLICATE_KEY: i32 = 11000;

/// Attempts at an append or checkpoint that keeps losing races before it gives up.
const MAX_ATTEMPTS: u32 = 100;

fn store_error(message: String) -> AdkError {
    AdkError::new(
        ErrorComponent::Session,
        ErrorCategory::Unavailable,
        "session.audit_log.storage",
        message,
    )
}

fn mongo_error(operation: &str) -> impl FnOnce(mongodb::error::Error) -> AdkError + '_ {
    move |e| store_error(format!("audit log {operation} failed: {e}"))
}

fn corrupt(field: &str) -> AdkError {
    store_error(format!("audit log field '{field}' is corrupt"))
}

fn is_duplicate_key(error: &mongodb::error::Error) -> bool {
    match error.kind.as_ref() {
        ErrorKind::Write(WriteFailure::WriteError(write)) => write.code == DUPLICATE_KEY,
        ErrorKind::Command(command) => command.code == DUPLICATE_KEY,
        _ => false,
    }
}

fn upsert() -> UpdateOptions {
    UpdateOptions::builder().upsert(true).build()
}

fn to_i64(value: u64, what: &str) -> Result<i64> {
    i64::try_from(value).map_err(|_| store_error(format!("audit log {what} {value} exceeds i64")))
}

/// Reads a non-negative integer the server may return as `Int32`, `Int64`, or `Double`.
fn read_u64(document: &Document, field: &str) -> Result<u64> {
    let value = match document.get(field) {
        Some(Bson::Int32(value)) => i64::from(*value),
        Some(Bson::Int64(value)) => *value,
        Some(Bson::Double(value)) => *value as i64,
        _ => return Err(corrupt(field)),
    };
    u64::try_from(value).map_err(|_| corrupt(field))
}

fn read_hash(document: &Document, field: &str) -> Result<AuditHash> {
    document.get_str(field).map_err(|_| corrupt(field))?.parse().map_err(|_| corrupt(field))
}

/// The log's committed size and its parked batch, if any.
struct LogState {
    size: u64,
    pending: Option<Document>,
}

/// MongoDB-backed [`AuditLogStore`].
///
/// See the [module documentation](crate::mongodb_audit_log) for how appends serialize
/// without transactions.
///
/// # Example
///
/// ```rust,no_run
/// use adk_session::MongoAuditLogStore;
///
/// # async fn example() -> adk_core::Result<()> {
/// let store = MongoAuditLogStore::new("mongodb://localhost:27017", "adk").await?;
/// store.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct MongoAuditLogStore {
    db: Database,
}

impl MongoAuditLogStore {
    /// Connects to `connection_string` and uses `database_name`.
    ///
    /// # Errors
    ///
    /// Returns `session.audit_log.storage` when the connection string is invalid.
    pub async fn new(connection_string: &str, database_name: &str) -> Result<Self> {
        let options = ClientOptions::parse(connection_string)
            .await
            .map_err(|e| store_error(format!("mongodb connection failed: {e}")))?;
        let client = Client::with_options(options)
            .map_err(|e| store_error(format!("mongodb connection failed: {e}")))?;
        Ok(Self::from_database(client.database(database_name)))
    }

    /// Uses an existing database handle, such as one shared with other MongoDB stores.
    pub fn from_database(db: Database) -> Self {
        Self { db }
    }

    /// Creates the unique indexes the store relies on.
    ///
    /// # Errors
    ///
    /// Returns `session.audit_log.storage` when an index cannot be created.
    pub async fn migrate(&self) -> Result<()> {
        let indexes = [
            (ENTRIES, doc! { "log_id": 1, "seq": 1 }, "adk_audit_entries_log_seq"),
            (NODES, doc! { "log_id": 1, "level": 1, "idx": 1 }, "adk_audit_nodes_log_level_idx"),
            (CHECKPOINTS, doc! { "log_id": 1, "size": 1 }, "adk_audit_checkpoints_log_size"),
        ];
        for (collection, keys, name) in indexes {
            self.db
                .collection::<Document>(collection)
                .create_index(
                    IndexModel::builder()
                        .keys(keys)
                        .options(
                            IndexOptions::builder().name(name.to_string()).unique(true).build(),
                        )
                        .build(),
                )
                .await
                .map_err(mongo_error("migrate"))?;
        }
        Ok(())
    }

    fn logs_collection(&self) -> Collection<Document> {
        self.db.collection(LOGS)
    }

    fn entries(&self) -> Collection<Document> {
        self.db.collection(ENTRIES)
    }

    fn nodes(&self) -> Collection<Document> {
        self.db.collection(NODES)
    }

    fn checkpoint_collection(&self) -> Collection<Document> {
        self.db.collection(CHECKPOINTS)
    }

    async fn state(&self, log_id: &str) -> Result<LogState> {
        let Some(document) = self
            .logs_collection()
            .find_one(doc! { "_id": log_id })
            .await
            .map_err(mongo_error("read"))?
        else {
            return Ok(LogState { size: 0, pending: None });
        };
        let pending = match document.get("pending") {
            None | Some(Bson::Null) => None,
            Some(Bson::Document(pending)) => Some(pending.clone()),
            Some(_) => return Err(corrupt("pending")),
        };
        Ok(LogState { size: read_u64(&document, "size")?, pending })
    }

    /// Writes a parked batch's entries and nodes, then commits it. Safe to run from any
    /// number of processes at once.
    async fn finish(&self, log_id: &str, pending: &Document) -> Result<()> {
        let token = pending.get_str("token").map_err(|_| corrupt("pending.token"))?;
        let end = read_u64(pending, "end")?;
        for entry in pending.get_array("entries").map_err(|_| corrupt("pending.entries"))? {
            let entry = entry.as_document().ok_or_else(|| corrupt("pending.entries"))?;
            let seq = entry.get("seq").cloned().ok_or_else(|| corrupt("pending.entries"))?;
            let mut row = entry.clone();
            row.insert("log_id", log_id);
            self.entries()
                .update_one(doc! { "log_id": log_id, "seq": seq }, doc! { "$setOnInsert": row })
                .with_options(upsert())
                .await
                .map_err(mongo_error("append"))?;
        }
        for node in pending.get_array("nodes").map_err(|_| corrupt("pending.nodes"))? {
            let node = node.as_document().ok_or_else(|| corrupt("pending.nodes"))?;
            let level = node.get("level").cloned().ok_or_else(|| corrupt("pending.nodes"))?;
            let idx = node.get("idx").cloned().ok_or_else(|| corrupt("pending.nodes"))?;
            let mut row = node.clone();
            row.insert("log_id", log_id);
            self.nodes()
                .update_one(
                    doc! { "log_id": log_id, "level": level, "idx": idx },
                    doc! { "$setOnInsert": row },
                )
                .with_options(upsert())
                .await
                .map_err(mongo_error("append"))?;
        }
        self.logs_collection()
            .update_one(
                doc! { "_id": log_id, "pending.token": token },
                doc! { "$set": { "size": to_i64(end, "size")?, "pending": Bson::Null } },
            )
            .await
            .map_err(mongo_error("append"))?;
        Ok(())
    }
}

#[async_trait]
impl AuditLogStore for MongoAuditLogStore {
    async fn append(&self, batch: AuditAppend) -> Result<Range<u64>> {
        // Checks the batch's shape against its own start before touching the log.
        let first = batch.entries.first().map_or(0, |entry| entry.seq);
        validate_append(&batch, first)?;
        let log_id = batch.log_id.as_str();
        self.logs_collection()
            .update_one(
                doc! { "_id": log_id },
                doc! { "$setOnInsert": { "size": 0_i64, "pending": Bson::Null } },
            )
            .with_options(upsert())
            .await
            .map_err(mongo_error("append"))?;

        let entries = batch
            .entries
            .iter()
            .map(|entry| {
                Ok(doc! {
                    "seq": to_i64(entry.seq, "seq")?,
                    "leaf_hash": entry.leaf_hash.to_string(),
                    "record": entry.record.clone().map_or(Bson::Null, Bson::String),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let nodes = batch
            .nodes
            .iter()
            .map(|node| {
                Ok(doc! {
                    "level": i32::from(node.level),
                    "idx": to_i64(node.index, "node index")?,
                    "hash": node.hash.to_string(),
                })
            })
            .collect::<Result<Vec<_>>>()?;

        for _ in 0..MAX_ATTEMPTS {
            let state = self.state(log_id).await?;
            if let Some(pending) = state.pending {
                self.finish(log_id, &pending).await?;
                continue;
            }
            let range = validate_append(&batch, state.size)?;
            let pending = doc! {
                "token": uuid::Uuid::new_v4().to_string(),
                "end": to_i64(range.end, "size")?,
                "entries": entries.clone(),
                "nodes": nodes.clone(),
            };
            let claimed = self
                .logs_collection()
                .update_one(
                    doc! { "_id": log_id, "size": to_i64(state.size, "size")?, "pending": Bson::Null },
                    doc! { "$set": { "pending": pending.clone() } },
                )
                .await
                .map_err(mongo_error("append"))?;
            if claimed.modified_count == 1 {
                self.finish(log_id, &pending).await?;
                return Ok(range);
            }
        }
        let size = self.state(log_id).await?.size;
        Err(seq_conflict_error(log_id, size, first))
    }

    async fn size(&self, log_id: &str) -> Result<u64> {
        Ok(self.state(log_id).await?.size)
    }

    async fn read(&self, log_id: &str, range: Range<u64>) -> Result<Vec<AuditLogEntry>> {
        let size = self.size(log_id).await?;
        if range.end > size {
            return Err(out_of_range_error(log_id, size, range.end));
        }
        if range.start >= range.end {
            return Ok(Vec::new());
        }
        let mut cursor = self
            .entries()
            .find(doc! {
                "log_id": log_id,
                "seq": { "$gte": to_i64(range.start, "seq")?, "$lt": to_i64(range.end, "seq")? },
            })
            .with_options(FindOptions::builder().sort(doc! { "seq": 1 }).build())
            .await
            .map_err(mongo_error("read"))?;
        let mut entries = Vec::new();
        while cursor.advance().await.map_err(mongo_error("read"))? {
            let document = cursor.deserialize_current().map_err(mongo_error("read"))?;
            let record = match document.get("record") {
                None | Some(Bson::Null) => None,
                Some(Bson::String(record)) => Some(record.clone()),
                Some(_) => return Err(corrupt("record")),
            };
            entries.push(AuditLogEntry {
                seq: read_u64(&document, "seq")?,
                leaf_hash: read_hash(&document, "leaf_hash")?,
                record,
            });
        }
        if entries.len() as u64 != range.end - range.start {
            return Err(store_error(format!(
                "audit log '{log_id}' is missing entries in {}..{}",
                range.start, range.end
            )));
        }
        Ok(entries)
    }

    async fn hash_at(&self, log_id: &str, level: u8, index: u64) -> Result<Option<AuditHash>> {
        // Entries and nodes of an uncommitted batch exist before the size covers them.
        let covered = index
            .checked_add(1)
            .and_then(|next| next.checked_shl(u32::from(level)))
            .filter(|_| level < 64);
        let Some(end) = covered else {
            return Ok(None);
        };
        if end > self.size(log_id).await? {
            return Ok(None);
        }
        let Ok(index) = i64::try_from(index) else {
            return Ok(None);
        };
        let (collection, filter, field) = if level == 0 {
            (self.entries(), doc! { "log_id": log_id, "seq": index }, "leaf_hash")
        } else {
            (
                self.nodes(),
                doc! { "log_id": log_id, "level": i32::from(level), "idx": index },
                "hash",
            )
        };
        let document = collection.find_one(filter).await.map_err(mongo_error("read"))?;
        document.map(|document| read_hash(&document, field)).transpose()
    }

    async fn put_checkpoint(&self, checkpoint: AuditCheckpoint) -> Result<()> {
        let log_id = checkpoint.log_id.as_str();
        let size = to_i64(checkpoint.size, "size")?;
        let row = doc! {
            "log_id": log_id,
            "size": size,
            "root_hash": checkpoint.root_hash.to_string(),
            "note": &checkpoint.note,
            "created_at_us": checkpoint.created_at.timestamp_micros(),
        };
        for _ in 0..MAX_ATTEMPTS {
            let log_size = self.size(log_id).await?;
            let existing = self
                .checkpoint_collection()
                .find_one(doc! { "log_id": log_id, "size": size })
                .await
                .map_err(mongo_error("checkpoint read"))?
                .map(|document| read_hash(&document, "root_hash"))
                .transpose()?;
            if validate_checkpoint(&checkpoint, log_size, existing)? {
                // A checkpoint's root never changes, so filtering on it only re-signs.
                self.checkpoint_collection()
                    .update_one(
                        doc! {
                            "log_id": log_id,
                            "size": size,
                            "root_hash": checkpoint.root_hash.to_string(),
                        },
                        doc! { "$set": {
                            "note": &checkpoint.note,
                            "created_at_us": checkpoint.created_at.timestamp_micros(),
                        } },
                    )
                    .await
                    .map_err(mongo_error("checkpoint write"))?;
                return Ok(());
            }
            match self.checkpoint_collection().insert_one(row.clone()).await {
                Ok(_) => return Ok(()),
                // Another writer stored this size first; compare against its root.
                Err(error) if is_duplicate_key(&error) => continue,
                Err(error) => return Err(mongo_error("checkpoint write")(error)),
            }
        }
        Err(store_error(format!(
            "audit checkpoint for '{log_id}' at size {} kept racing another writer",
            checkpoint.size
        )))
    }

    async fn latest_checkpoint(&self, log_id: &str) -> Result<Option<AuditCheckpoint>> {
        Ok(self.checkpoints(log_id).await?.pop())
    }

    async fn checkpoints(&self, log_id: &str) -> Result<Vec<AuditCheckpoint>> {
        let mut cursor = self
            .checkpoint_collection()
            .find(doc! { "log_id": log_id })
            .with_options(FindOptions::builder().sort(doc! { "size": 1 }).build())
            .await
            .map_err(mongo_error("checkpoint read"))?;
        let mut checkpoints = Vec::new();
        while cursor.advance().await.map_err(mongo_error("checkpoint read"))? {
            let document = cursor.deserialize_current().map_err(mongo_error("checkpoint read"))?;
            let micros = match document.get("created_at_us") {
                Some(Bson::Int64(micros)) => *micros,
                _ => return Err(corrupt("created_at_us")),
            };
            checkpoints.push(AuditCheckpoint {
                log_id: log_id.to_string(),
                size: read_u64(&document, "size")?,
                root_hash: read_hash(&document, "root_hash")?,
                note: document.get_str("note").map_err(|_| corrupt("note"))?.to_string(),
                created_at: DateTime::from_timestamp_micros(micros)
                    .ok_or_else(|| corrupt("created_at_us"))?,
            });
        }
        Ok(checkpoints)
    }

    async fn prune(&self, log_id: &str, before_seq: u64) -> Result<u64> {
        let size = self.size(log_id).await?;
        if before_seq > size {
            return Err(out_of_range_error(log_id, size, before_seq));
        }
        let pruned = self
            .entries()
            .update_many(
                doc! {
                    "log_id": log_id,
                    "seq": { "$lt": to_i64(before_seq, "seq")? },
                    "record": { "$ne": Bson::Null },
                },
                doc! { "$set": { "record": Bson::Null } },
            )
            .await
            .map_err(mongo_error("prune"))?;
        Ok(pruned.modified_count)
    }

    async fn logs(&self) -> Result<Vec<String>> {
        // A refused first append creates the log document with nothing in it.
        let mut cursor = self
            .logs_collection()
            .find(doc! { "$or": [ { "size": { "$gt": 0_i64 } }, { "pending": { "$ne": Bson::Null } } ] })
            .await
            .map_err(mongo_error("read"))?;
        let mut ids = Vec::new();
        while cursor.advance().await.map_err(mongo_error("read"))? {
            let document = cursor.deserialize_current().map_err(mongo_error("read"))?;
            ids.push(document.get_str("_id").map_err(|_| corrupt("_id"))?.to_string());
        }
        ids.sort();
        Ok(ids)
    }
}
