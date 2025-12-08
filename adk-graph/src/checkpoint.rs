//! Checkpointing for persistent graph state

use crate::error::{GraphError, Result};
use crate::state::Checkpoint;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

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
                created_at TEXT NOT NULL
            )
            "#,
        )
        .execute(&pool)
        .await
        .map_err(|e| GraphError::CheckpointError(e.to_string()))?;

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
            INSERT INTO graph_checkpoints (id, thread_id, state, step, pending_nodes, metadata, created_at)
            VALUES (?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(&checkpoint.checkpoint_id)
        .bind(&checkpoint.thread_id)
        .bind(&state_json)
        .bind(checkpoint.step as i64)
        .bind(&pending_json)
        .bind(&metadata_json)
        .bind(&created_at)
        .execute(&self.pool)
        .await
        .map_err(|e| GraphError::CheckpointError(e.to_string()))?;

        Ok(checkpoint.checkpoint_id.clone())
    }

    async fn load(&self, thread_id: &str) -> Result<Option<Checkpoint>> {
        let row: Option<(String, String, String, i64, String, String, String)> = sqlx::query_as(
            r#"
            SELECT id, thread_id, state, step, pending_nodes, metadata, created_at
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
            Some((id, thread_id, state, step, pending_nodes, metadata, created_at)) => {
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
                };
                Ok(Some(checkpoint))
            }
            None => Ok(None),
        }
    }

    async fn load_by_id(&self, checkpoint_id: &str) -> Result<Option<Checkpoint>> {
        let row: Option<(String, String, String, i64, String, String, String)> = sqlx::query_as(
            r#"
            SELECT id, thread_id, state, step, pending_nodes, metadata, created_at
            FROM graph_checkpoints
            WHERE id = ?
            "#,
        )
        .bind(checkpoint_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| GraphError::CheckpointError(e.to_string()))?;

        match row {
            Some((id, thread_id, state, step, pending_nodes, metadata, created_at)) => {
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
                };
                Ok(Some(checkpoint))
            }
            None => Ok(None),
        }
    }

    async fn list(&self, thread_id: &str) -> Result<Vec<Checkpoint>> {
        let rows: Vec<(String, String, String, i64, String, String, String)> = sqlx::query_as(
            r#"
            SELECT id, thread_id, state, step, pending_nodes, metadata, created_at
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
        for (id, thread_id, state, step, pending_nodes, metadata, created_at) in rows {
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
