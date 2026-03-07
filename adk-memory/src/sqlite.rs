//! SQLite-backed memory service.
//!
//! Provides [`SqliteMemoryService`], a [`MemoryService`](crate::MemoryService) implementation
//! that stores memory entries in SQLite with keyword-based full-text search via FTS5.
//!
//! This is a lightweight alternative to [`PostgresMemoryService`](crate::PostgresMemoryService)
//! for single-node deployments that don't need vector similarity search.
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_memory::SqliteMemoryService;
//!
//! let service = SqliteMemoryService::new("sqlite:memory.db").await?;
//! service.migrate().await?;
//! ```

use crate::service::*;
use adk_core::{Part, Result};
use async_trait::async_trait;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{Row, SqlitePool};
use std::str::FromStr;
use tracing::instrument;

/// SQLite-backed memory service with FTS5 full-text search.
///
/// Stores memory entries in a SQLite database with an FTS5 virtual table
/// for efficient keyword search. No embedding provider is needed.
///
/// # Example
///
/// ```rust,ignore
/// use adk_memory::SqliteMemoryService;
///
/// let service = SqliteMemoryService::new("sqlite:memory.db").await?;
/// service.migrate().await?;
/// ```
pub struct SqliteMemoryService {
    pool: SqlitePool,
}

impl SqliteMemoryService {
    /// Connect to SQLite for memory storage.
    ///
    /// Accepts any SQLite connection string (e.g. `sqlite:memory.db`,
    /// `sqlite::memory:` for in-memory). File-based databases are
    /// created automatically if they don't exist.
    pub async fn new(database_url: &str) -> Result<Self> {
        let options = SqliteConnectOptions::from_str(database_url)
            .map_err(|e| adk_core::AdkError::Memory(format!("invalid sqlite url: {e}")))?
            .create_if_missing(true);
        let pool = SqlitePool::connect_with(options)
            .await
            .map_err(|e| adk_core::AdkError::Memory(format!("sqlite connection failed: {e}")))?;
        Ok(Self { pool })
    }

    /// Create a memory service from an existing connection pool.
    pub fn from_pool(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Create the `memory_entries` table and FTS5 virtual table.
    ///
    /// Safe to call multiple times — uses `IF NOT EXISTS`.
    pub async fn migrate(&self) -> Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS memory_entries (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                app_name TEXT NOT NULL,
                user_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                content TEXT NOT NULL,
                content_text TEXT NOT NULL,
                author TEXT NOT NULL,
                timestamp TEXT NOT NULL
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| adk_core::AdkError::Memory(format!("migration failed: {e}")))?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_memory_app_user \
             ON memory_entries(app_name, user_id)",
        )
        .execute(&self.pool)
        .await
        .map_err(|e| adk_core::AdkError::Memory(format!("index creation failed: {e}")))?;

        // FTS5 virtual table for full-text search
        sqlx::query(
            r#"
            CREATE VIRTUAL TABLE IF NOT EXISTS memory_entries_fts
            USING fts5(content_text, content='memory_entries', content_rowid='id')
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| adk_core::AdkError::Memory(format!("FTS5 table creation failed: {e}")))?;

        // Triggers to keep FTS index in sync
        sqlx::query(
            r#"
            CREATE TRIGGER IF NOT EXISTS memory_entries_ai AFTER INSERT ON memory_entries BEGIN
                INSERT INTO memory_entries_fts(rowid, content_text) VALUES (new.id, new.content_text);
            END
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| adk_core::AdkError::Memory(format!("trigger creation failed: {e}")))?;

        sqlx::query(
            r#"
            CREATE TRIGGER IF NOT EXISTS memory_entries_ad AFTER DELETE ON memory_entries BEGIN
                INSERT INTO memory_entries_fts(memory_entries_fts, rowid, content_text) VALUES('delete', old.id, old.content_text);
            END
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| adk_core::AdkError::Memory(format!("trigger creation failed: {e}")))?;

        Ok(())
    }

    /// Extract plain text from content parts.
    fn extract_text(content: &adk_core::Content) -> String {
        content
            .parts
            .iter()
            .filter_map(|part| match part {
                Part::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[async_trait]
impl MemoryService for SqliteMemoryService {
    #[instrument(skip_all, fields(app_name = %app_name, user_id = %user_id, session_id = %session_id, entry_count = entries.len()))]
    async fn add_session(
        &self,
        app_name: &str,
        user_id: &str,
        session_id: &str,
        entries: Vec<MemoryEntry>,
    ) -> Result<()> {
        if entries.is_empty() {
            return Ok(());
        }

        for entry in &entries {
            let content_json = serde_json::to_string(&entry.content)
                .map_err(|e| adk_core::AdkError::Memory(format!("serialization failed: {e}")))?;
            let content_text = Self::extract_text(&entry.content);
            let timestamp_str = entry.timestamp.to_rfc3339();

            sqlx::query(
                "INSERT INTO memory_entries \
                 (app_name, user_id, session_id, content, content_text, author, timestamp) \
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(app_name)
            .bind(user_id)
            .bind(session_id)
            .bind(&content_json)
            .bind(&content_text)
            .bind(&entry.author)
            .bind(&timestamp_str)
            .execute(&self.pool)
            .await
            .map_err(|e| adk_core::AdkError::Memory(format!("insert failed: {e}")))?;
        }

        Ok(())
    }

    #[instrument(skip_all, fields(app_name = %req.app_name, user_id = %req.user_id))]
    async fn search(&self, req: SearchRequest) -> Result<SearchResponse> {
        let limit = req.limit.unwrap_or(10) as i64;

        let rows = sqlx::query(
            r#"
            SELECT m.content, m.author, m.timestamp, f.rank
            FROM memory_entries_fts f
            JOIN memory_entries m ON m.id = f.rowid
            WHERE memory_entries_fts MATCH ?
              AND m.app_name = ? AND m.user_id = ?
            ORDER BY f.rank
            LIMIT ?
            "#,
        )
        .bind(&req.query)
        .bind(&req.app_name)
        .bind(&req.user_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| adk_core::AdkError::Memory(format!("search failed: {e}")))?;

        let memories = rows
            .iter()
            .map(|row| {
                let content_str: String = row.get("content");
                let content: adk_core::Content =
                    serde_json::from_str(&content_str).unwrap_or_else(|_| adk_core::Content {
                        role: "user".to_string(),
                        parts: vec![],
                    });
                let author: String = row.get("author");
                let timestamp_str: String = row.get("timestamp");
                let timestamp = chrono::DateTime::parse_from_rfc3339(&timestamp_str)
                    .map(|dt| dt.with_timezone(&chrono::Utc))
                    .unwrap_or_default();
                MemoryEntry { content, author, timestamp }
            })
            .collect();

        Ok(SearchResponse { memories })
    }

    #[instrument(skip_all, fields(app_name = %app_name, user_id = %user_id))]
    async fn delete_user(&self, app_name: &str, user_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM memory_entries WHERE app_name = ? AND user_id = ?")
            .bind(app_name)
            .bind(user_id)
            .execute(&self.pool)
            .await
            .map_err(|e| adk_core::AdkError::Memory(format!("delete_user failed: {e}")))?;
        Ok(())
    }

    #[instrument(skip_all, fields(app_name = %app_name, user_id = %user_id, session_id = %session_id))]
    async fn delete_session(&self, app_name: &str, user_id: &str, session_id: &str) -> Result<()> {
        sqlx::query(
            "DELETE FROM memory_entries WHERE app_name = ? AND user_id = ? AND session_id = ?",
        )
        .bind(app_name)
        .bind(user_id)
        .bind(session_id)
        .execute(&self.pool)
        .await
        .map_err(|e| adk_core::AdkError::Memory(format!("delete_session failed: {e}")))?;
        Ok(())
    }

    #[instrument(skip_all)]
    async fn health_check(&self) -> Result<()> {
        sqlx::query("SELECT 1")
            .execute(&self.pool)
            .await
            .map_err(|e| adk_core::AdkError::Memory(format!("health check failed: {e}")))?;
        Ok(())
    }
}
