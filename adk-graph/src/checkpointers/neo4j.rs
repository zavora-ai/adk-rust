//! Neo4j [`Checkpointer`].

use async_trait::async_trait;
use neo4rs::{Graph, Neo4jErrorKind, Query, Row, Txn, query};

use super::{decode, encode, storage_error};
use crate::checkpoint::{Checkpointer, RetentionPolicy, conflict, duplicate_id, ensure_newer};
use crate::error::Result;
use crate::state::Checkpoint;

const BACKEND: &str = "neo4j";

/// Attempts at a save that fails with a transient error, such as a detected deadlock.
const TRANSIENT_ATTEMPTS: usize = 5;

const CONSTRAINT_VIOLATION: &str = "Neo.ClientError.Schema.ConstraintValidationFailed";

/// Schema codes that mean an `IF NOT EXISTS` statement raced another one and lost.
const SCHEMA_ALREADY_EXISTS: [&str; 4] = [
    "Neo.ClientError.Schema.EquivalentSchemaRuleAlreadyExists",
    "Neo.ClientError.Schema.ConstraintAlreadyExists",
    "Neo.ClientError.Schema.IndexAlreadyExists",
    "Neo.ClientError.Schema.ConstraintWithNameAlreadyExists",
];

const SCHEMA: [&str; 3] = [
    "CREATE CONSTRAINT adk_graph_checkpoint_id IF NOT EXISTS \
     FOR (c:AdkGraphCheckpoint) REQUIRE c.id IS UNIQUE",
    "CREATE CONSTRAINT adk_graph_thread_id IF NOT EXISTS \
     FOR (t:AdkGraphThread) REQUIRE t.id IS UNIQUE",
    "CREATE INDEX adk_graph_checkpoint_thread IF NOT EXISTS \
     FOR (c:AdkGraphCheckpoint) ON (c.thread_id, c.version)",
];

fn neo4j_code(error: &neo4rs::Error) -> Option<&str> {
    match error {
        neo4rs::Error::Neo4j(error) => Some(error.code()),
        _ => None,
    }
}

fn is_transient(error: &neo4rs::Error) -> bool {
    matches!(error, neo4rs::Error::Neo4j(error) if error.kind() == Neo4jErrorKind::Transient)
}

async fn txn_rows(txn: &mut Txn, query: Query) -> std::result::Result<Vec<Row>, neo4rs::Error> {
    let mut stream = txn.execute(query).await?;
    let mut rows = Vec::new();
    while let Some(row) = stream.next(&mut *txn).await? {
        rows.push(row);
    }
    Ok(rows)
}

/// What one save attempt concluded.
enum Attempt {
    Stored,
    Behind(u64),
}

/// Neo4j-backed [`Checkpointer`].
///
/// Each checkpoint is an `AdkGraphCheckpoint` node holding its ID, thread, version,
/// and the checkpoint as JSON in `data`. Each thread has an `AdkGraphThread` node
/// holding its highest version in `head`. A save write-locks the thread node before
/// it reads `head`, so concurrent saves of one thread run one after another and the
/// second sees the first's version. The thread node is created by an auto-commit
/// `MERGE` first, so a save only ever takes the write lock directly and two saves
/// cannot deadlock upgrading a shared lock. Call [`migrate`](Self::migrate) once per
/// database to create the uniqueness constraints.
///
/// # Example
///
/// ```rust,no_run
/// use adk_graph::Neo4jCheckpointer;
///
/// # async fn example() -> adk_graph::Result<()> {
/// let checkpointer = Neo4jCheckpointer::new("bolt://localhost:7687", "neo4j", "password").await?;
/// checkpointer.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Neo4jCheckpointer {
    graph: Graph,
}

impl std::fmt::Debug for Neo4jCheckpointer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Neo4jCheckpointer").finish_non_exhaustive()
    }
}

impl Neo4jCheckpointer {
    /// Connects to `uri` as `user`.
    ///
    /// # Errors
    ///
    /// Returns `GraphError::CheckpointError` when the server cannot be reached.
    pub async fn new(uri: &str, user: &str, password: &str) -> Result<Self> {
        let graph = Graph::new(uri, user, password)
            .await
            .map_err(|e| storage_error(BACKEND, format!("connection failed: {e}")))?;
        Ok(Self::from_graph(graph))
    }

    /// Uses an existing connection, such as one shared with other Neo4j stores.
    pub fn from_graph(graph: Graph) -> Self {
        Self { graph }
    }

    /// Creates the constraints and index the checkpointer relies on.
    ///
    /// # Errors
    ///
    /// Returns `GraphError::CheckpointError` when a schema statement fails.
    pub async fn migrate(&self) -> Result<()> {
        for statement in SCHEMA {
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
                    Err(error) => {
                        return Err(storage_error(BACKEND, format!("migration failed: {error}")));
                    }
                }
            }
        }
        Ok(())
    }

    async fn try_save(
        &self,
        checkpoint: &Checkpoint,
        version: i64,
        data: &str,
    ) -> std::result::Result<Attempt, neo4rs::Error> {
        self.graph
            .run(
                query("MERGE (:AdkGraphThread {id: $thread})")
                    .param("thread", checkpoint.thread_id.clone()),
            )
            .await?;
        let mut txn = self.graph.start_txn().await?;
        let found = txn_rows(
            &mut txn,
            query(
                "MATCH (t:AdkGraphThread {id: $thread}) SET t.lock = coalesce(t.lock, 0) + 1 \
                 RETURN coalesce(t.head, 0) AS head",
            )
            .param("thread", checkpoint.thread_id.clone()),
        )
        .await?;
        let head: i64 = found.first().and_then(|row| row.get("head").ok()).unwrap_or(0);
        if head >= version {
            let _ = txn.rollback().await;
            return Ok(Attempt::Behind(head as u64));
        }
        txn.run(
            query(
                "MATCH (t:AdkGraphThread {id: $thread}) SET t.head = $version \
                 CREATE (:AdkGraphCheckpoint {id: $id, thread_id: $thread, version: $version, data: $data})",
            )
            .param("thread", checkpoint.thread_id.clone())
            .param("id", checkpoint.checkpoint_id.clone())
            .param("version", version)
            .param("data", data),
        )
        .await?;
        txn.commit().await?;
        Ok(Attempt::Stored)
    }

    async fn read(&self, statement: Query) -> Result<Vec<Checkpoint>> {
        let mut stream =
            self.graph.execute(statement).await.map_err(|e| storage_error(BACKEND, e))?;
        let mut checkpoints = Vec::new();
        while let Some(row) = stream.next().await.map_err(|e| storage_error(BACKEND, e))? {
            let data: String = row
                .get("data")
                .map_err(|e| storage_error(BACKEND, format!("column 'data' unreadable: {e}")))?;
            checkpoints.push(decode(BACKEND, &data)?);
        }
        Ok(checkpoints)
    }
}

fn version_param(version: u64) -> Result<i64> {
    i64::try_from(version)
        .map_err(|_| storage_error(BACKEND, format!("version {version} exceeds a 64-bit integer")))
}

#[async_trait]
impl Checkpointer for Neo4jCheckpointer {
    async fn save(&self, checkpoint: &Checkpoint) -> Result<String> {
        ensure_newer(checkpoint, None)?;
        let version = version_param(checkpoint.version)?;
        let data = encode(checkpoint)?;
        let mut tries = 1;
        loop {
            match self.try_save(checkpoint, version, &data).await {
                Ok(Attempt::Stored) => return Ok(checkpoint.checkpoint_id.clone()),
                Ok(Attempt::Behind(head)) => return Err(conflict(checkpoint, head)),
                Err(error) if is_transient(&error) && tries < TRANSIENT_ATTEMPTS => tries += 1,
                Err(error) if neo4j_code(&error) == Some(CONSTRAINT_VIOLATION) => {
                    return Err(duplicate_id(&checkpoint.checkpoint_id));
                }
                Err(error) => return Err(storage_error(BACKEND, error)),
            }
        }
    }

    async fn load(&self, thread_id: &str) -> Result<Option<Checkpoint>> {
        Ok(self
            .read(
                query(
                    "MATCH (c:AdkGraphCheckpoint {thread_id: $thread}) \
                     RETURN c.data AS data ORDER BY c.version DESC LIMIT 1",
                )
                .param("thread", thread_id),
            )
            .await?
            .pop())
    }

    async fn load_by_id(&self, checkpoint_id: &str) -> Result<Option<Checkpoint>> {
        Ok(self
            .read(
                query("MATCH (c:AdkGraphCheckpoint {id: $id}) RETURN c.data AS data")
                    .param("id", checkpoint_id),
            )
            .await?
            .pop())
    }

    async fn list(&self, thread_id: &str) -> Result<Vec<Checkpoint>> {
        self.read(
            query(
                "MATCH (c:AdkGraphCheckpoint {thread_id: $thread}) \
                 RETURN c.data AS data ORDER BY c.version ASC",
            )
            .param("thread", thread_id),
        )
        .await
    }

    async fn list_page(
        &self,
        thread_id: &str,
        before: Option<u64>,
        limit: usize,
    ) -> Result<Vec<Checkpoint>> {
        self.read(
            query(
                "MATCH (c:AdkGraphCheckpoint {thread_id: $thread}) WHERE c.version < $before \
                 RETURN c.data AS data ORDER BY c.version DESC LIMIT $limit",
            )
            .param("thread", thread_id)
            .param("before", before.map_or(Ok(i64::MAX), version_param)?)
            .param("limit", i64::try_from(limit).unwrap_or(i64::MAX)),
        )
        .await
    }

    async fn delete(&self, thread_id: &str) -> Result<()> {
        self.graph
            .run(
                query(
                    "MATCH (c:AdkGraphCheckpoint {thread_id: $thread}) DETACH DELETE c \
                     WITH count(*) AS removed \
                     MATCH (t:AdkGraphThread {id: $thread}) DETACH DELETE t",
                )
                .param("thread", thread_id),
            )
            .await
            .map_err(|e| storage_error(BACKEND, e))
    }

    async fn prune(&self, thread_id: &str, policy: &RetentionPolicy) -> Result<usize> {
        let expired = policy.expired(&self.list(thread_id).await?);
        if expired.is_empty() {
            return Ok(0);
        }
        let mut stream = self
            .graph
            .execute(
                query(
                    "MATCH (c:AdkGraphCheckpoint {thread_id: $thread}) WHERE c.id IN $ids \
                     DETACH DELETE c RETURN count(*) AS removed",
                )
                .param("thread", thread_id)
                .param("ids", expired),
            )
            .await
            .map_err(|e| storage_error(BACKEND, e))?;
        let removed: i64 = match stream.next().await.map_err(|e| storage_error(BACKEND, e))? {
            Some(row) => row.get("removed").map_err(|e| storage_error(BACKEND, e))?,
            None => 0,
        };
        Ok(removed as usize)
    }
}
