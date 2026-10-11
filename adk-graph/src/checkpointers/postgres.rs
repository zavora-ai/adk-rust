//! PostgreSQL [`Checkpointer`].

use async_trait::async_trait;
use sqlx::Row;
use sqlx::postgres::PgPool;

use super::{decode, encode, storage_error};
use crate::checkpoint::{Checkpointer, RetentionPolicy, conflict, duplicate_id, ensure_newer};
use crate::error::Result;
use crate::state::Checkpoint;

const BACKEND: &str = "postgres";

/// Schema statements, run in order by [`PostgresCheckpointer::migrate`].
const SCHEMA: [&str; 2] = [
    "CREATE TABLE IF NOT EXISTS adk_graph_checkpoints (\
        id TEXT PRIMARY KEY, \
        thread_id TEXT NOT NULL, \
        version BIGINT NOT NULL, \
        created_at TIMESTAMPTZ NOT NULL, \
        data TEXT NOT NULL, \
        CONSTRAINT adk_graph_checkpoints_thread_version UNIQUE (thread_id, version))",
    "CREATE INDEX IF NOT EXISTS adk_graph_checkpoints_thread \
        ON adk_graph_checkpoints (thread_id, version DESC)",
];

/// PostgreSQL-backed [`Checkpointer`].
///
/// Each checkpoint is one row of `adk_graph_checkpoints`: the thread, version, and
/// creation time as columns and the whole checkpoint as JSON text. A save is one
/// `INSERT` guarded by "no equal or higher version exists", and the unique
/// constraint on `(thread_id, version)` refuses the second of two concurrent saves of
/// one version, so writers never interleave under any isolation level.
///
/// # Example
///
/// ```rust,no_run
/// use adk_graph::PostgresCheckpointer;
///
/// # async fn example() -> adk_graph::Result<()> {
/// let checkpointer = PostgresCheckpointer::new("postgres://localhost/app").await?;
/// checkpointer.migrate().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct PostgresCheckpointer {
    pool: PgPool,
}

impl PostgresCheckpointer {
    /// Connects to PostgreSQL. Call [`migrate`](Self::migrate) before first use.
    ///
    /// # Errors
    ///
    /// Returns `GraphError::CheckpointError` when the database cannot be reached.
    pub async fn new(database_url: &str) -> Result<Self> {
        let pool = PgPool::connect(database_url)
            .await
            .map_err(|e| storage_error(BACKEND, format!("connection failed: {e}")))?;
        Ok(Self::from_pool(pool))
    }

    /// Uses an existing connection pool, such as one shared with session storage.
    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Creates the checkpoint table and index if they do not exist, under an
    /// advisory lock so concurrent processes can call it at startup.
    ///
    /// # Errors
    ///
    /// Returns `GraphError::CheckpointError` when a statement fails.
    pub async fn migrate(&self) -> Result<()> {
        let fail = |e: sqlx::Error| storage_error(BACKEND, format!("migration failed: {e}"));
        let mut tx = self.pool.begin().await.map_err(fail)?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext('adk_graph_checkpoints'))")
            .execute(&mut *tx)
            .await
            .map_err(fail)?;
        for statement in SCHEMA {
            sqlx::query(statement).execute(&mut *tx).await.map_err(fail)?;
        }
        tx.commit().await.map_err(fail)
    }

    async fn head(&self, thread_id: &str) -> Result<u64> {
        let head: Option<i64> = sqlx::query_scalar(
            "SELECT MAX(version) FROM adk_graph_checkpoints WHERE thread_id = $1",
        )
        .bind(thread_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| storage_error(BACKEND, e))?;
        Ok(head.map_or(0, |head| head as u64))
    }

    async fn fetch(
        &self,
        query: sqlx::query::Query<'_, sqlx::Postgres, sqlx::postgres::PgArguments>,
    ) -> Result<Vec<Checkpoint>> {
        let rows = query.fetch_all(&self.pool).await.map_err(|e| storage_error(BACKEND, e))?;
        rows.iter()
            .map(|row| {
                let data: String = row.try_get("data").map_err(|e| storage_error(BACKEND, e))?;
                decode(BACKEND, &data)
            })
            .collect()
    }
}

fn version_param(version: u64) -> Result<i64> {
    i64::try_from(version)
        .map_err(|_| storage_error(BACKEND, format!("version {version} exceeds BIGINT")))
}

#[async_trait]
impl Checkpointer for PostgresCheckpointer {
    async fn save(&self, checkpoint: &Checkpoint) -> Result<String> {
        ensure_newer(checkpoint, None)?;
        let version = version_param(checkpoint.version)?;
        let inserted = sqlx::query(
            "INSERT INTO adk_graph_checkpoints (id, thread_id, version, created_at, data) \
             SELECT $1, $2, $3, $4, $5 \
             WHERE NOT EXISTS (SELECT 1 FROM adk_graph_checkpoints \
                               WHERE thread_id = $2 AND version >= $3)",
        )
        .bind(&checkpoint.checkpoint_id)
        .bind(&checkpoint.thread_id)
        .bind(version)
        .bind(checkpoint.created_at)
        .bind(encode(checkpoint)?)
        .execute(&self.pool)
        .await;
        match inserted {
            Ok(result) if result.rows_affected() == 1 => Ok(checkpoint.checkpoint_id.clone()),
            Ok(_) => Err(conflict(checkpoint, self.head(&checkpoint.thread_id).await?)),
            Err(sqlx::Error::Database(error)) if error.is_unique_violation() => {
                if error.constraint() == Some("adk_graph_checkpoints_thread_version") {
                    Err(conflict(checkpoint, self.head(&checkpoint.thread_id).await?))
                } else {
                    Err(duplicate_id(&checkpoint.checkpoint_id))
                }
            }
            Err(error) => Err(storage_error(BACKEND, error)),
        }
    }

    async fn load(&self, thread_id: &str) -> Result<Option<Checkpoint>> {
        Ok(self
            .fetch(
                sqlx::query(
                    "SELECT data FROM adk_graph_checkpoints WHERE thread_id = $1 \
                     ORDER BY version DESC LIMIT 1",
                )
                .bind(thread_id.to_string()),
            )
            .await?
            .pop())
    }

    async fn load_by_id(&self, checkpoint_id: &str) -> Result<Option<Checkpoint>> {
        Ok(self
            .fetch(
                sqlx::query("SELECT data FROM adk_graph_checkpoints WHERE id = $1")
                    .bind(checkpoint_id.to_string()),
            )
            .await?
            .pop())
    }

    async fn list(&self, thread_id: &str) -> Result<Vec<Checkpoint>> {
        self.fetch(
            sqlx::query(
                "SELECT data FROM adk_graph_checkpoints WHERE thread_id = $1 ORDER BY version ASC",
            )
            .bind(thread_id.to_string()),
        )
        .await
    }

    async fn list_page(
        &self,
        thread_id: &str,
        before: Option<u64>,
        limit: usize,
    ) -> Result<Vec<Checkpoint>> {
        let before = before.map_or(Ok(i64::MAX), version_param)?;
        self.fetch(
            sqlx::query(
                "SELECT data FROM adk_graph_checkpoints WHERE thread_id = $1 AND version < $2 \
                 ORDER BY version DESC LIMIT $3",
            )
            .bind(thread_id.to_string())
            .bind(before)
            .bind(i64::try_from(limit).unwrap_or(i64::MAX)),
        )
        .await
    }

    async fn delete(&self, thread_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM adk_graph_checkpoints WHERE thread_id = $1")
            .bind(thread_id)
            .execute(&self.pool)
            .await
            .map_err(|e| storage_error(BACKEND, e))?;
        Ok(())
    }

    async fn prune(&self, thread_id: &str, policy: &RetentionPolicy) -> Result<usize> {
        let expired = policy.expired(&self.list(thread_id).await?);
        if expired.is_empty() {
            return Ok(0);
        }
        let result =
            sqlx::query("DELETE FROM adk_graph_checkpoints WHERE thread_id = $1 AND id = ANY($2)")
                .bind(thread_id)
                .bind(&expired)
                .execute(&self.pool)
                .await
                .map_err(|e| storage_error(BACKEND, e))?;
        Ok(result.rows_affected() as usize)
    }
}
