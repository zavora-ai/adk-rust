//! Neo4j-backed [`AuditLogStore`](adk_core::audit_log::AuditLogStore).
//!
//! # Node layout
//!
//! | Label | Key (unique) | Holds |
//! |-------|--------------|-------|
//! | `AdkAuditLog` | `log_id` | The log's `size`, set by the first successful append, and the write lock appends take |
//! | `AdkAuditEntry` | `log_id`, `seq` | `leaf_hash` and canonical `record`; `record` is removed once pruned |
//! | `AdkAuditNode` | `log_id`, `level`, `idx` | `hash` of a complete Merkle subtree |
//! | `AdkAuditCheckpoint` | `log_id`, `size` | `root_hash`, signed `note`, and `created_at_us` |
//!
//! Sequence numbers, sizes, and node indexes are stored as 64-bit integers, hashes as
//! lowercase hex, and checkpoint times as integer microseconds, so everything reads back
//! exactly.
//!
//! # Atomicity
//!
//! | Operation | Mechanism |
//! |-----------|-----------|
//! | `append` | One transaction write-locks the `AdkAuditLog` node, reads `size`, checks the batch starts there, writes entries and nodes, and sets the new size |
//! | `put_checkpoint` | One transaction write-locks the `AdkAuditLog` node, then checks the size and any checkpoint of the same size before writing |
//! | `prune` | One statement removes `record` from entries below the position |
//!
//! The `AdkAuditLog` node is created by an auto-commit `MERGE` before the transaction, so
//! the transaction only ever takes the write lock directly and two writers cannot
//! deadlock on a shared-to-exclusive upgrade. A second writer waits for the first to
//! commit, then reads the new size and gets `audit_log.seq_conflict`. The uniqueness
//! constraint on `(log_id, seq)` refuses a duplicate entry even without the lock.
//!
//! Call [`migrate`](Neo4jAuditLogStore::migrate) once per database to create the
//! constraints.

use crate::neo4j_stores::{Inline, detached};
use std::ops::Range;

use adk_core::audit_log::{
    AuditAppend, AuditCheckpoint, AuditHash, AuditLogEntry, AuditLogStore, out_of_range_error,
    seq_conflict_error, validate_append, validate_checkpoint,
};
use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result};
use async_trait::async_trait;
use chrono::DateTime;
use neo4rs::{Graph, Neo4jErrorKind, Query, Row, Txn, query};

/// Attempts at a transaction that fails with a transient error, such as a detected
/// deadlock, before the error is returned.
const TRANSIENT_ATTEMPTS: usize = 5;

/// Schema codes that mean an `IF NOT EXISTS` statement raced another one and lost.
const SCHEMA_ALREADY_EXISTS: [&str; 4] = [
    "Neo.ClientError.Schema.EquivalentSchemaRuleAlreadyExists",
    "Neo.ClientError.Schema.ConstraintAlreadyExists",
    "Neo.ClientError.Schema.IndexAlreadyExists",
    "Neo.ClientError.Schema.ConstraintWithNameAlreadyExists",
];

const CONSTRAINT_VIOLATION: &str = "Neo.ClientError.Schema.ConstraintValidationFailed";

fn storage_error(message: String) -> AdkError {
    AdkError::new(
        ErrorComponent::Session,
        ErrorCategory::Unavailable,
        "session.audit_log.storage",
        format!("neo4j audit log: {message}"),
    )
}

fn corrupt(column: &str, error: impl std::fmt::Display) -> AdkError {
    storage_error(format!("column '{column}' is corrupt: {error}"))
}

fn to_i64(value: u64, what: &str) -> Result<i64> {
    i64::try_from(value).map_err(|_| storage_error(format!("{what} {value} exceeds i64")))
}

fn to_u64(value: i64, column: &str) -> Result<u64> {
    u64::try_from(value).map_err(|e| corrupt(column, e))
}

fn parse_hash(text: &str, column: &str) -> Result<AuditHash> {
    text.parse().map_err(|e| corrupt(column, e))
}

fn neo4j_code(error: &neo4rs::Error) -> Option<&str> {
    match error {
        neo4rs::Error::Neo4j(error) => Some(error.code()),
        _ => None,
    }
}

/// A failure inside a store operation: either the database's or the store's own verdict.
enum Failure {
    Neo4j(neo4rs::Error),
    Store(AdkError),
}

impl From<neo4rs::Error> for Failure {
    fn from(error: neo4rs::Error) -> Self {
        Self::Neo4j(error)
    }
}

impl From<AdkError> for Failure {
    fn from(error: AdkError) -> Self {
        Self::Store(error)
    }
}

impl Failure {
    fn is_transient(&self) -> bool {
        matches!(self, Self::Neo4j(neo4rs::Error::Neo4j(error)) if error.kind() == Neo4jErrorKind::Transient)
    }

    fn into_adk(self) -> AdkError {
        match self {
            Self::Neo4j(error) => storage_error(error.to_string()),
            Self::Store(error) => error,
        }
    }
}

/// Retries `$attempt` while it fails with a transient error.
macro_rules! retry_transient {
    ($attempt:expr) => {{
        let mut tries = 1;
        loop {
            match $attempt.await {
                Err(failure) if failure.is_transient() && tries < TRANSIENT_ATTEMPTS => tries += 1,
                result => break result.map_err(Failure::into_adk),
            }
        }
    }};
}

/// Reads every row of `query` inside `txn`.
async fn rows(txn: &mut Txn, query: Query) -> std::result::Result<Vec<Row>, neo4rs::Error> {
    let mut stream = txn.execute(query).await?;
    let mut rows = Vec::new();
    while let Some(row) = stream.next(&mut *txn).await? {
        rows.push(row);
    }
    Ok(rows)
}

/// Reads every row of an auto-commit `query`.
async fn graph_rows(graph: &Graph, query: Query) -> Result<Vec<Row>> {
    let run = async {
        let mut stream = graph.execute(query).await?;
        let mut rows = Vec::new();
        while let Some(row) = stream.next().await? {
            rows.push(row);
        }
        Ok::<_, neo4rs::Error>(rows)
    };
    run.await.map_err(|e| storage_error(format!("read failed: {e}")))
}

fn column<T: serde::de::DeserializeOwned>(row: &Row, name: &str) -> Result<T> {
    row.get::<T>(name).map_err(|e| corrupt(name, e))
}

fn decode_checkpoint(row: &Row) -> Result<AuditCheckpoint> {
    let micros: i64 = column(row, "created_at_us")?;
    Ok(AuditCheckpoint {
        log_id: column(row, "log_id")?,
        size: to_u64(column(row, "size")?, "size")?,
        root_hash: parse_hash(&column::<String>(row, "root_hash")?, "root_hash")?,
        note: column(row, "note")?,
        created_at: DateTime::from_timestamp_micros(micros)
            .ok_or_else(|| corrupt("created_at_us", micros))?,
    })
}

/// Neo4j-backed [`AuditLogStore`].
///
/// See the [module documentation](self) for the node layout and how appends serialize.
///
/// # Example
///
/// ```rust,no_run
/// use adk_session::Neo4jAuditLogStore;
///
/// # async fn demo() -> adk_core::Result<()> {
/// let store = Neo4jAuditLogStore::new("bolt://localhost:7687", "neo4j", "password").await?;
/// store.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Neo4jAuditLogStore {
    graph: Graph,
}

impl std::fmt::Debug for Neo4jAuditLogStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Neo4jAuditLogStore").finish_non_exhaustive()
    }
}

impl Neo4jAuditLogStore {
    /// Connects to Neo4j.
    ///
    /// Call [`migrate`](Self::migrate) before first use.
    ///
    /// # Errors
    ///
    /// Returns `session.audit_log.storage` when the server cannot be reached.
    pub async fn new(uri: &str, user: &str, password: &str) -> Result<Self> {
        let graph = Graph::new(uri, user, password)
            .await
            .map_err(|e| storage_error(format!("connection failed: {e}")))?;
        Ok(Self::from_graph(graph))
    }

    /// Uses an existing connection, such as the one a
    /// [`Neo4jSessionService`](crate::Neo4jSessionService) holds.
    pub fn from_graph(graph: Graph) -> Self {
        Self { graph }
    }

    /// Creates the uniqueness constraints the store relies on. Concurrent processes may
    /// call it at startup.
    ///
    /// # Errors
    ///
    /// Returns `session.audit_log.storage` when a schema statement fails.
    pub async fn migrate(&self) -> Result<()> {
        let statements = [
            "CREATE CONSTRAINT adk_audit_log_unique IF NOT EXISTS \
             FOR (l:AdkAuditLog) REQUIRE l.log_id IS UNIQUE",
            "CREATE CONSTRAINT adk_audit_entry_unique IF NOT EXISTS \
             FOR (e:AdkAuditEntry) REQUIRE (e.log_id, e.seq) IS UNIQUE",
            "CREATE CONSTRAINT adk_audit_node_unique IF NOT EXISTS \
             FOR (n:AdkAuditNode) REQUIRE (n.log_id, n.level, n.idx) IS UNIQUE",
            "CREATE CONSTRAINT adk_audit_checkpoint_unique IF NOT EXISTS \
             FOR (c:AdkAuditCheckpoint) REQUIRE (c.log_id, c.size) IS UNIQUE",
        ];
        for statement in statements {
            let mut tries = 1;
            loop {
                match self.graph.run(query(statement)).await {
                    Ok(()) => break,
                    Err(error)
                        if neo4j_code(&error)
                            .is_some_and(|code| SCHEMA_ALREADY_EXISTS.contains(&code)) =>
                    {
                        break;
                    }
                    Err(error) if tries < TRANSIENT_ATTEMPTS => {
                        tracing::debug!(error = %error, "retrying neo4j schema statement");
                        tries += 1;
                    }
                    Err(error) => return Err(storage_error(format!("migration failed: {error}"))),
                }
            }
        }
        Ok(())
    }

    /// Starts a transaction holding the write lock on `log_id`'s log node and returns it
    /// with the log's size.
    async fn lock_log(&self, log_id: &str) -> std::result::Result<(Txn, u64), Failure> {
        // Created outside the transaction, so the transaction takes the write lock directly
        // instead of upgrading a MERGE's lock, which two writers could deadlock on.
        self.graph
            .run(query("MERGE (:AdkAuditLog {log_id: $log_id})").param("log_id", log_id))
            .await?;
        let mut txn = self.graph.start_txn().await?;
        txn.run(
            query(
                "MATCH (l:AdkAuditLog {log_id: $log_id}) \
                 SET l.lock_seq = coalesce(l.lock_seq, 0) + 1",
            )
            .param("log_id", log_id),
        )
        .await?;
        let found = rows(
            &mut txn,
            query("MATCH (l:AdkAuditLog {log_id: $log_id}) RETURN coalesce(l.size, 0) AS size")
                .param("log_id", log_id),
        )
        .await?;
        let size = match found.first() {
            Some(row) => to_u64(column(row, "size")?, "size")?,
            None => 0,
        };
        Ok((txn, size))
    }

    async fn try_append(&self, batch: &AuditAppend) -> std::result::Result<Range<u64>, Failure> {
        let (mut txn, size) = self.lock_log(&batch.log_id).await?;
        let range = match validate_append(batch, size) {
            Ok(range) => range,
            Err(error) => {
                let _ = txn.rollback().await;
                return Err(error.into());
            }
        };
        for entry in &batch.entries {
            let created = txn
                .run(
                    query(
                        "CREATE (:AdkAuditEntry {log_id: $log_id, seq: $seq, \
                         leaf_hash: $leaf_hash, record: $record})",
                    )
                    .param("log_id", batch.log_id.as_str())
                    .param("seq", to_i64(entry.seq, "seq")?)
                    .param("leaf_hash", entry.leaf_hash.to_string())
                    .param("record", entry.record.clone()),
                )
                .await;
            if let Err(error) = created {
                let _ = txn.rollback().await;
                if neo4j_code(&error) == Some(CONSTRAINT_VIOLATION) {
                    return Err(seq_conflict_error(&batch.log_id, size, range.start).into());
                }
                return Err(error.into());
            }
        }
        for node in &batch.nodes {
            txn.run(
                query(
                    "MERGE (n:AdkAuditNode {log_id: $log_id, level: $level, idx: $idx}) \
                     SET n.hash = $hash",
                )
                .param("log_id", batch.log_id.as_str())
                .param("level", i64::from(node.level))
                .param("idx", to_i64(node.index, "node index")?)
                .param("hash", node.hash.to_string()),
            )
            .await?;
        }
        txn.run(
            query("MATCH (l:AdkAuditLog {log_id: $log_id}) SET l.size = $size")
                .param("log_id", batch.log_id.as_str())
                .param("size", to_i64(range.end, "size")?),
        )
        .await?;
        txn.commit().await?;
        Ok(range)
    }

    async fn try_put_checkpoint(
        &self,
        checkpoint: &AuditCheckpoint,
    ) -> std::result::Result<(), Failure> {
        let (mut txn, size) = self.lock_log(&checkpoint.log_id).await?;
        let found = rows(
            &mut txn,
            query(
                "MATCH (c:AdkAuditCheckpoint {log_id: $log_id, size: $size}) \
                 RETURN c.root_hash AS root_hash",
            )
            .param("log_id", checkpoint.log_id.as_str())
            .param("size", to_i64(checkpoint.size, "size")?),
        )
        .await?;
        let existing = match found.first() {
            Some(row) => Some(parse_hash(&column::<String>(row, "root_hash")?, "root_hash")?),
            None => None,
        };
        if let Err(error) = validate_checkpoint(checkpoint, size, existing) {
            let _ = txn.rollback().await;
            return Err(error.into());
        }
        txn.run(
            query(
                "MERGE (c:AdkAuditCheckpoint {log_id: $log_id, size: $size}) \
                 SET c.root_hash = $root_hash, c.note = $note, c.created_at_us = $created_at_us",
            )
            .param("log_id", checkpoint.log_id.as_str())
            .param("size", to_i64(checkpoint.size, "size")?)
            .param("root_hash", checkpoint.root_hash.to_string())
            .param("note", checkpoint.note.as_str())
            .param("created_at_us", checkpoint.created_at.timestamp_micros()),
        )
        .await?;
        txn.commit().await?;
        Ok(())
    }
}

const CHECKPOINT_COLUMNS: &str = "c.log_id AS log_id, c.size AS size, c.root_hash AS root_hash, \
     c.note AS note, c.created_at_us AS created_at_us";

#[async_trait]
impl AuditLogStore for Inline<Neo4jAuditLogStore> {
    async fn append(&self, batch: AuditAppend) -> Result<Range<u64>> {
        retry_transient!(self.try_append(&batch))
    }

    async fn size(&self, log_id: &str) -> Result<u64> {
        let found = graph_rows(
            &self.graph,
            query("MATCH (l:AdkAuditLog {log_id: $log_id}) RETURN coalesce(l.size, 0) AS size")
                .param("log_id", log_id),
        )
        .await?;
        match found.first() {
            Some(row) => to_u64(column(row, "size")?, "size"),
            None => Ok(0),
        }
    }

    async fn read(&self, log_id: &str, range: Range<u64>) -> Result<Vec<AuditLogEntry>> {
        let size = self.size(log_id).await?;
        if range.end > size {
            return Err(out_of_range_error(log_id, size, range.end));
        }
        if range.start >= range.end {
            return Ok(Vec::new());
        }
        let found = graph_rows(
            &self.graph,
            query(
                "MATCH (e:AdkAuditEntry {log_id: $log_id}) WHERE e.seq >= $start AND e.seq < $end \
                 RETURN e.seq AS seq, e.leaf_hash AS leaf_hash, e.record AS record ORDER BY e.seq",
            )
            .param("log_id", log_id)
            .param("start", to_i64(range.start, "seq")?)
            .param("end", to_i64(range.end, "seq")?),
        )
        .await?;
        found
            .iter()
            .map(|row| {
                Ok(AuditLogEntry {
                    seq: to_u64(column(row, "seq")?, "seq")?,
                    leaf_hash: parse_hash(&column::<String>(row, "leaf_hash")?, "leaf_hash")?,
                    record: column(row, "record")?,
                })
            })
            .collect()
    }

    async fn hash_at(&self, log_id: &str, level: u8, index: u64) -> Result<Option<AuditHash>> {
        let Ok(index) = i64::try_from(index) else {
            return Ok(None);
        };
        let statement = if level == 0 {
            query("MATCH (e:AdkAuditEntry {log_id: $log_id, seq: $idx}) RETURN e.leaf_hash AS hash")
        } else {
            query(
                "MATCH (n:AdkAuditNode {log_id: $log_id, level: $level, idx: $idx}) \
                 RETURN n.hash AS hash",
            )
            .param("level", i64::from(level))
        };
        let found =
            graph_rows(&self.graph, statement.param("log_id", log_id).param("idx", index)).await?;
        found.first().map(|row| parse_hash(&column::<String>(row, "hash")?, "hash")).transpose()
    }

    async fn put_checkpoint(&self, checkpoint: AuditCheckpoint) -> Result<()> {
        retry_transient!(self.try_put_checkpoint(&checkpoint))
    }

    async fn latest_checkpoint(&self, log_id: &str) -> Result<Option<AuditCheckpoint>> {
        let found = graph_rows(
            &self.graph,
            query(&format!(
                "MATCH (c:AdkAuditCheckpoint {{log_id: $log_id}}) RETURN {CHECKPOINT_COLUMNS} \
                 ORDER BY c.size DESC LIMIT 1"
            ))
            .param("log_id", log_id),
        )
        .await?;
        found.first().map(decode_checkpoint).transpose()
    }

    async fn checkpoints(&self, log_id: &str) -> Result<Vec<AuditCheckpoint>> {
        let found = graph_rows(
            &self.graph,
            query(&format!(
                "MATCH (c:AdkAuditCheckpoint {{log_id: $log_id}}) RETURN {CHECKPOINT_COLUMNS} \
                 ORDER BY c.size"
            ))
            .param("log_id", log_id),
        )
        .await?;
        found.iter().map(decode_checkpoint).collect()
    }

    async fn prune(&self, log_id: &str, before_seq: u64) -> Result<u64> {
        let size = self.size(log_id).await?;
        if before_seq > size {
            return Err(out_of_range_error(log_id, size, before_seq));
        }
        let found = graph_rows(
            &self.graph,
            query(
                "MATCH (e:AdkAuditEntry {log_id: $log_id}) \
                 WHERE e.seq < $before AND e.record IS NOT NULL \
                 REMOVE e.record RETURN count(e) AS pruned",
            )
            .param("log_id", log_id)
            .param("before", to_i64(before_seq, "seq")?),
        )
        .await?;
        match found.first() {
            Some(row) => to_u64(column(row, "pruned")?, "pruned"),
            None => Ok(0),
        }
    }

    async fn logs(&self) -> Result<Vec<String>> {
        // A log node exists from an append's first lock; only a committed append sets `size`.
        let found = graph_rows(
            &self.graph,
            query(
                "MATCH (l:AdkAuditLog) WHERE l.size IS NOT NULL \
                 RETURN l.log_id AS log_id",
            ),
        )
        .await?;
        let mut ids = found.iter().map(|row| column(row, "log_id")).collect::<Result<Vec<_>>>()?;
        // Sorted here so the order never depends on the server's collation.
        ids.sort();
        Ok(ids)
    }
}

#[async_trait]
impl AuditLogStore for Neo4jAuditLogStore {
    async fn append(&self, batch: AuditAppend) -> Result<Range<u64>> {
        let inline = Inline(self.clone());
        detached(async move { inline.append(batch).await }).await
    }

    async fn size(&self, log_id: &str) -> Result<u64> {
        let inline = Inline(self.clone());
        let log_id = log_id.to_owned();
        detached(async move { inline.size(&log_id).await }).await
    }

    async fn read(&self, log_id: &str, range: Range<u64>) -> Result<Vec<AuditLogEntry>> {
        let inline = Inline(self.clone());
        let log_id = log_id.to_owned();
        detached(async move { inline.read(&log_id, range).await }).await
    }

    async fn hash_at(&self, log_id: &str, level: u8, index: u64) -> Result<Option<AuditHash>> {
        let inline = Inline(self.clone());
        let log_id = log_id.to_owned();
        detached(async move { inline.hash_at(&log_id, level, index).await }).await
    }

    async fn put_checkpoint(&self, checkpoint: AuditCheckpoint) -> Result<()> {
        let inline = Inline(self.clone());
        detached(async move { inline.put_checkpoint(checkpoint).await }).await
    }

    async fn latest_checkpoint(&self, log_id: &str) -> Result<Option<AuditCheckpoint>> {
        let inline = Inline(self.clone());
        let log_id = log_id.to_owned();
        detached(async move { inline.latest_checkpoint(&log_id).await }).await
    }

    async fn checkpoints(&self, log_id: &str) -> Result<Vec<AuditCheckpoint>> {
        let inline = Inline(self.clone());
        let log_id = log_id.to_owned();
        detached(async move { inline.checkpoints(&log_id).await }).await
    }

    async fn prune(&self, log_id: &str, before_seq: u64) -> Result<u64> {
        let inline = Inline(self.clone());
        let log_id = log_id.to_owned();
        detached(async move { inline.prune(&log_id, before_seq).await }).await
    }

    async fn logs(&self) -> Result<Vec<String>> {
        let inline = Inline(self.clone());
        detached(async move { inline.logs().await }).await
    }
}
