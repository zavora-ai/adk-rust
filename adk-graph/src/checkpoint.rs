//! Checkpointing for persistent graph state

#[cfg(feature = "sqlite")]
use crate::error::GraphError;
use crate::error::Result;
use crate::state::Checkpoint;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// How many checkpoints to keep for a thread, and for how long.
///
/// A long-running thread accumulates one checkpoint per super-step. Without a
/// policy that grows without bound, which costs storage and slows a `list`.
///
/// The newest checkpoint is never removed, whatever the policy says: it is the
/// one a resume loads, so discarding it would end the thread.
///
/// # Example
///
/// ```
/// use adk_graph::checkpoint::RetentionPolicy;
/// use std::time::Duration;
///
/// // Keep the last 50 steps, and nothing older than a week.
/// let policy = RetentionPolicy::keep_last(50).with_max_age(Duration::from_secs(7 * 24 * 3600));
/// # let _ = policy;
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RetentionPolicy {
    /// Keep at most this many checkpoints per thread, newest first.
    pub max_per_thread: Option<usize>,
    /// Remove checkpoints older than this.
    pub max_age: Option<std::time::Duration>,
}

impl RetentionPolicy {
    /// Keeps the newest `count` checkpoints for a thread.
    ///
    /// A count of zero is raised to one, because the newest is always kept.
    pub fn keep_last(count: usize) -> Self {
        Self { max_per_thread: Some(count.max(1)), max_age: None }
    }

    /// Removes checkpoints older than `age`.
    pub fn max_age(age: std::time::Duration) -> Self {
        Self { max_per_thread: None, max_age: Some(age) }
    }

    /// Adds an age limit to a count limit.
    pub fn with_max_age(mut self, age: std::time::Duration) -> Self {
        self.max_age = Some(age);
        self
    }

    /// Adds a count limit to an age limit.
    pub fn with_max_per_thread(mut self, count: usize) -> Self {
        self.max_per_thread = Some(count.max(1));
        self
    }

    /// Whether this policy would remove anything.
    pub fn is_unlimited(&self) -> bool {
        self.max_per_thread.is_none() && self.max_age.is_none()
    }

    /// Selects the checkpoint ids this policy discards, newest always kept.
    ///
    /// Shared by every backend so they cannot disagree about what to keep.
    pub fn expired(&self, checkpoints: &[Checkpoint]) -> Vec<String> {
        if self.is_unlimited() || checkpoints.len() <= 1 {
            return Vec::new();
        }
        let mut ordered: Vec<&Checkpoint> = checkpoints.iter().collect();
        // Newest first, so the one a resume loads is at index 0.
        ordered.sort_by_key(|checkpoint| std::cmp::Reverse(checkpoint.created_at));

        let cutoff = self.max_age.and_then(|age| {
            chrono::Duration::from_std(age).ok().map(|age| chrono::Utc::now() - age)
        });

        ordered
            .iter()
            .enumerate()
            .filter(|(index, checkpoint)| {
                // Index 0 is the newest and is never discarded.
                *index > 0
                    && (self.max_per_thread.is_some_and(|max| *index >= max)
                        || cutoff.is_some_and(|cutoff| checkpoint.created_at < cutoff))
            })
            .map(|(_, checkpoint)| checkpoint.checkpoint_id.clone())
            .collect()
    }
}

/// Checkpointer trait for persistence
#[async_trait]
pub trait Checkpointer: Send + Sync {
    /// Save a checkpoint
    async fn save(&self, checkpoint: &Checkpoint) -> Result<String>;

    /// Load the latest checkpoint for a thread
    async fn load(&self, thread_id: &str) -> Result<Option<Checkpoint>>;

    /// Load a specific checkpoint by ID
    async fn load_by_id(&self, checkpoint_id: &str) -> Result<Option<Checkpoint>>;

    /// List all checkpoints for a thread (for time travel)
    async fn list(&self, thread_id: &str) -> Result<Vec<Checkpoint>>;

    /// Delete checkpoints for a thread
    async fn delete(&self, thread_id: &str) -> Result<()>;

    /// Removes the checkpoints a retention policy discards, keeping the newest.
    ///
    /// Returns how many were removed. The default keeps everything, so a backend
    /// written before this existed is unaffected and a thread grows as before.
    ///
    /// # Errors
    ///
    /// Returns an error when the backend cannot be read or written.
    async fn prune(&self, _thread_id: &str, _policy: &RetentionPolicy) -> Result<usize> {
        Ok(0)
    }
}

/// In-memory checkpointer for development and testing
#[derive(Default)]
pub struct MemoryCheckpointer {
    checkpoints: Arc<RwLock<HashMap<String, Vec<Checkpoint>>>>,
}

impl MemoryCheckpointer {
    /// Create a new in-memory checkpointer
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl Checkpointer for MemoryCheckpointer {
    async fn save(&self, checkpoint: &Checkpoint) -> Result<String> {
        let mut store = self.checkpoints.write().await;
        let thread_checkpoints = store.entry(checkpoint.thread_id.clone()).or_insert_with(Vec::new);

        let checkpoint_id = checkpoint.checkpoint_id.clone();
        thread_checkpoints.push(checkpoint.clone());

        Ok(checkpoint_id)
    }

    async fn load(&self, thread_id: &str) -> Result<Option<Checkpoint>> {
        let store = self.checkpoints.read().await;
        Ok(store.get(thread_id).and_then(|checkpoints| checkpoints.last()).cloned())
    }

    async fn load_by_id(&self, checkpoint_id: &str) -> Result<Option<Checkpoint>> {
        let store = self.checkpoints.read().await;
        for checkpoints in store.values() {
            for checkpoint in checkpoints {
                if checkpoint.checkpoint_id == checkpoint_id {
                    return Ok(Some(checkpoint.clone()));
                }
            }
        }
        Ok(None)
    }

    async fn list(&self, thread_id: &str) -> Result<Vec<Checkpoint>> {
        let store = self.checkpoints.read().await;
        Ok(store.get(thread_id).cloned().unwrap_or_default())
    }

    async fn delete(&self, thread_id: &str) -> Result<()> {
        let mut store = self.checkpoints.write().await;
        store.remove(thread_id);
        Ok(())
    }

    async fn prune(&self, thread_id: &str, policy: &RetentionPolicy) -> Result<usize> {
        let mut store = self.checkpoints.write().await;
        let Some(thread) = store.get_mut(thread_id) else { return Ok(0) };
        let expired = policy.expired(thread);
        if expired.is_empty() {
            return Ok(0);
        }
        let before = thread.len();
        thread.retain(|checkpoint| !expired.contains(&checkpoint.checkpoint_id));
        Ok(before - thread.len())
    }
}

/// SQLite checkpointer for production use
#[cfg(feature = "sqlite")]
pub struct SqliteCheckpointer {
    pool: sqlx::SqlitePool,
}

#[cfg(feature = "sqlite")]
impl SqliteCheckpointer {
    /// Create a new SQLite checkpointer
    pub async fn new(database_url: &str) -> Result<Self> {
        let pool = sqlx::SqlitePool::connect(database_url)
            .await
            .map_err(|e| GraphError::CheckpointError(e.to_string()))?;

        Self::from_pool(pool).await
    }

    /// Create a SQLite checkpointer from an existing pool.
    ///
    /// Use this to share one connection pool with the rest of an application
    /// instead of opening a second one. The checkpointer writes through the pool
    /// it is given, so the caller's own queries see its rows.
    ///
    /// The schema is applied to the pool's database on every call, so adopting an
    /// already-initialized database is safe.
    ///
    /// `SqlitePool` comes from `sqlx`, so a caller has to depend on a
    /// semver-compatible `sqlx` to construct one:
    ///
    /// ```toml
    /// sqlx = { version = "0.8", features = ["runtime-tokio", "sqlite"] }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns `GraphError::CheckpointError` when the table or the index cannot be
    /// created on the pool's database.
    pub async fn from_pool(pool: sqlx::SqlitePool) -> Result<Self> {
        // Create table
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS graph_checkpoints (
                id TEXT PRIMARY KEY,
                thread_id TEXT NOT NULL,
                state TEXT NOT NULL,
                step INTEGER NOT NULL,
                pending_nodes TEXT NOT NULL,
                metadata TEXT,
                created_at TEXT NOT NULL,
                cleared_interrupt TEXT,
                attempts TEXT,
                child_ledger TEXT
            )
            "#,
        )
        .execute(&pool)
        .await
        .map_err(|e| GraphError::CheckpointError(e.to_string()))?;

        // A database created before `cleared_interrupt` existed keeps its old
        // shape under CREATE TABLE IF NOT EXISTS, so add the column separately
        // and ignore the duplicate-column error on a database that already has it.
        for column in ["cleared_interrupt", "attempts", "child_ledger"] {
            let _ = sqlx::query(&format!("ALTER TABLE graph_checkpoints ADD COLUMN {column} TEXT"))
                .execute(&pool)
                .await;
        }

        sqlx::query(
            r#"
            CREATE INDEX IF NOT EXISTS idx_graph_checkpoints_thread
            ON graph_checkpoints(thread_id, created_at DESC)
            "#,
        )
        .execute(&pool)
        .await
        .map_err(|e| GraphError::CheckpointError(e.to_string()))?;

        Ok(Self { pool })
    }

    /// Create an in-memory SQLite checkpointer (for testing)
    pub async fn in_memory() -> Result<Self> {
        Self::new(":memory:").await
    }
}

#[cfg(feature = "sqlite")]
#[async_trait]
impl Checkpointer for SqliteCheckpointer {
    async fn save(&self, checkpoint: &Checkpoint) -> Result<String> {
        let state_json = serde_json::to_string(&checkpoint.state)?;
        let pending_json = serde_json::to_string(&checkpoint.pending_nodes)?;
        let metadata_json = serde_json::to_string(&checkpoint.metadata)?;
        let created_at = checkpoint.created_at.to_rfc3339();

        sqlx::query(
            r#"
            INSERT INTO graph_checkpoints (id, thread_id, state, step, pending_nodes, metadata, created_at, cleared_interrupt, attempts, child_ledger)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(&checkpoint.checkpoint_id)
        .bind(&checkpoint.thread_id)
        .bind(&state_json)
        .bind(checkpoint.step as i64)
        .bind(&pending_json)
        .bind(&metadata_json)
        .bind(&created_at)
        .bind(checkpoint.cleared_interrupt.as_deref())
        .bind(serde_json::to_string(&checkpoint.attempts).unwrap_or_else(|_| "{}".to_string()))
        .bind(serde_json::to_string(&checkpoint.child_ledger).unwrap_or_else(|_| "{}".to_string()))
        .execute(&self.pool)
        .await
        .map_err(|e| GraphError::CheckpointError(e.to_string()))?;

        Ok(checkpoint.checkpoint_id.clone())
    }

    async fn load(&self, thread_id: &str) -> Result<Option<Checkpoint>> {
        let row: Option<(String, String, String, i64, String, String, String, Option<String>, Option<String>, Option<String>)> = sqlx::query_as(
            r#"
            SELECT id, thread_id, state, step, pending_nodes, metadata, created_at, cleared_interrupt, attempts, child_ledger
            FROM graph_checkpoints
            WHERE thread_id = ?
            ORDER BY created_at DESC
            LIMIT 1
            "#,
        )
        .bind(thread_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| GraphError::CheckpointError(e.to_string()))?;

        match row {
            Some((
                id,
                thread_id,
                state,
                step,
                pending_nodes,
                metadata,
                created_at,
                cleared_interrupt,
                attempts,
                child_ledger,
            )) => {
                let checkpoint = Checkpoint {
                    checkpoint_id: id,
                    thread_id,
                    state: serde_json::from_str(&state)?,
                    step: step as usize,
                    pending_nodes: serde_json::from_str(&pending_nodes)?,
                    metadata: serde_json::from_str(&metadata)?,
                    created_at: chrono::DateTime::parse_from_rfc3339(&created_at)
                        .map_err(|e| GraphError::CheckpointError(e.to_string()))?
                        .with_timezone(&chrono::Utc),
                    cleared_interrupt,
                    attempts: attempts
                        .and_then(|raw| serde_json::from_str(&raw).ok())
                        .unwrap_or_default(),
                    child_ledger: child_ledger
                        .and_then(|raw| serde_json::from_str(&raw).ok())
                        .unwrap_or_default(),
                };
                Ok(Some(checkpoint))
            }
            None => Ok(None),
        }
    }

    async fn load_by_id(&self, checkpoint_id: &str) -> Result<Option<Checkpoint>> {
        let row: Option<(String, String, String, i64, String, String, String, Option<String>, Option<String>, Option<String>)> = sqlx::query_as(
            r#"
            SELECT id, thread_id, state, step, pending_nodes, metadata, created_at, cleared_interrupt, attempts, child_ledger
            FROM graph_checkpoints
            WHERE id = ?
            "#,
        )
        .bind(checkpoint_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| GraphError::CheckpointError(e.to_string()))?;

        match row {
            Some((
                id,
                thread_id,
                state,
                step,
                pending_nodes,
                metadata,
                created_at,
                cleared_interrupt,
                attempts,
                child_ledger,
            )) => {
                let checkpoint = Checkpoint {
                    checkpoint_id: id,
                    thread_id,
                    state: serde_json::from_str(&state)?,
                    step: step as usize,
                    pending_nodes: serde_json::from_str(&pending_nodes)?,
                    metadata: serde_json::from_str(&metadata)?,
                    created_at: chrono::DateTime::parse_from_rfc3339(&created_at)
                        .map_err(|e| GraphError::CheckpointError(e.to_string()))?
                        .with_timezone(&chrono::Utc),
                    cleared_interrupt,
                    attempts: attempts
                        .and_then(|raw| serde_json::from_str(&raw).ok())
                        .unwrap_or_default(),
                    child_ledger: child_ledger
                        .and_then(|raw| serde_json::from_str(&raw).ok())
                        .unwrap_or_default(),
                };
                Ok(Some(checkpoint))
            }
            None => Ok(None),
        }
    }

    async fn list(&self, thread_id: &str) -> Result<Vec<Checkpoint>> {
        let rows: Vec<(String, String, String, i64, String, String, String, Option<String>, Option<String>, Option<String>)> = sqlx::query_as(
            r#"
            SELECT id, thread_id, state, step, pending_nodes, metadata, created_at, cleared_interrupt, attempts, child_ledger
            FROM graph_checkpoints
            WHERE thread_id = ?
            ORDER BY created_at ASC
            "#,
        )
        .bind(thread_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| GraphError::CheckpointError(e.to_string()))?;

        let mut checkpoints = Vec::with_capacity(rows.len());
        for (
            id,
            thread_id,
            state,
            step,
            pending_nodes,
            metadata,
            created_at,
            cleared_interrupt,
            attempts,
            child_ledger,
        ) in rows
        {
            checkpoints.push(Checkpoint {
                checkpoint_id: id,
                thread_id,
                state: serde_json::from_str(&state)?,
                step: step as usize,
                pending_nodes: serde_json::from_str(&pending_nodes)?,
                metadata: serde_json::from_str(&metadata)?,
                created_at: chrono::DateTime::parse_from_rfc3339(&created_at)
                    .map_err(|e| GraphError::CheckpointError(e.to_string()))?
                    .with_timezone(&chrono::Utc),
                cleared_interrupt,
                attempts: attempts
                    .and_then(|raw| serde_json::from_str(&raw).ok())
                    .unwrap_or_default(),
                child_ledger: child_ledger
                    .and_then(|raw| serde_json::from_str(&raw).ok())
                    .unwrap_or_default(),
            });
        }
        Ok(checkpoints)
    }

    async fn delete(&self, thread_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM graph_checkpoints WHERE thread_id = ?")
            .bind(thread_id)
            .execute(&self.pool)
            .await
            .map_err(|e| GraphError::CheckpointError(e.to_string()))?;
        Ok(())
    }

    async fn prune(&self, thread_id: &str, policy: &RetentionPolicy) -> Result<usize> {
        // The policy decides, not the SQL, so both backends keep the same set.
        let expired = policy.expired(&self.list(thread_id).await?);
        if expired.is_empty() {
            return Ok(0);
        }
        let mut removed = 0usize;
        for checkpoint_id in &expired {
            let result = sqlx::query("DELETE FROM graph_checkpoints WHERE id = ?")
                .bind(checkpoint_id)
                .execute(&self.pool)
                .await
                .map_err(|e| GraphError::CheckpointError(e.to_string()))?;
            removed += result.rows_affected() as usize;
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::State;

    #[tokio::test]
    async fn test_memory_checkpointer() {
        let cp = MemoryCheckpointer::new();

        // Create and save checkpoint
        let checkpoint = Checkpoint::new("thread_1", State::new(), 0, vec!["node_a".to_string()]);
        let id = cp.save(&checkpoint).await.unwrap();
        assert!(!id.is_empty());

        // Load latest
        let loaded = cp.load("thread_1").await.unwrap();
        assert!(loaded.is_some());
        assert_eq!(loaded.unwrap().step, 0);

        // Save another checkpoint
        let checkpoint2 = Checkpoint::new("thread_1", State::new(), 1, vec!["node_b".to_string()]);
        cp.save(&checkpoint2).await.unwrap();

        // Load latest should return step 1
        let loaded = cp.load("thread_1").await.unwrap();
        assert_eq!(loaded.unwrap().step, 1);

        // List should return both
        let all = cp.list("thread_1").await.unwrap();
        assert_eq!(all.len(), 2);

        // Delete
        cp.delete("thread_1").await.unwrap();
        let loaded = cp.load("thread_1").await.unwrap();
        assert!(loaded.is_none());
    }
}
