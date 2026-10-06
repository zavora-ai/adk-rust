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
use adk_core::Result;
use async_trait::async_trait;
use chrono::Utc;
use serde::Deserialize;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{Row, SqlitePool};
use std::path::Path;
use std::str::FromStr;
use tracing::instrument;

/// Private struct for deserializing JSON memory entries during import.
#[derive(Deserialize)]
struct JsonMemoryEntry {
    content: serde_json::Value,
    author: String,
    #[serde(default)]
    timestamp: Option<chrono::DateTime<Utc>>,
    #[serde(default)]
    app_name: Option<String>,
    #[serde(default)]
    user_id: Option<String>,
}

/// Extract searchable text from a JSON content value.
///
/// - If the value is a string, returns it directly.
/// - If the value is an object with a `parts` array, extracts `text` fields from each part.
/// - Otherwise, returns the JSON serialized form as a fallback.
fn extract_content_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Object(obj) => {
            if let Some(serde_json::Value::Array(parts)) = obj.get("parts") {
                parts
                    .iter()
                    .filter_map(|part| part.get("text").and_then(|t| t.as_str()).map(String::from))
                    .collect::<Vec<_>>()
                    .join(" ")
            } else {
                value.to_string()
            }
        }
        _ => value.to_string(),
    }
}

/// Converts free text into an FTS5 query matching entries that contain every word.
///
/// Each whitespace-separated token becomes a quoted phrase, so FTS5 syntax
/// characters (`"`, `'`, `*`, `^`, `:`, `?`, parentheses) are matched as text
/// instead of parsed, and the bare operators `AND`, `OR`, `NOT` and `NEAR` are
/// dropped. Tokens without a letter or digit produce no FTS5 term and are
/// skipped. As with `plainto_tsquery` in the Postgres backend, the remaining
/// terms are ANDed. Returns `None` when nothing searchable remains.
fn fts5_match_query(query: &str) -> Option<String> {
    let terms: Vec<String> = query
        .split_whitespace()
        .filter(|token| !matches!(*token, "AND" | "OR" | "NOT" | "NEAR"))
        .filter(|token| token.chars().any(char::is_alphanumeric))
        .map(|token| format!("\"{}\"", token.replace('"', "\"\"")))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" "))
}

/// SQLite-backed memory service with FTS5 full-text search.
///
/// Stores memory entries in a SQLite database with an FTS5 virtual table
/// for efficient keyword search. No embedding provider is needed.
///
/// Search and query-based deletion treat the query as plain text: an entry
/// matches when it contains every word of the query, and FTS5 operators in the
/// query are matched literally rather than interpreted.
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
            .map_err(|e| adk_core::AdkError::memory(format!("invalid sqlite url: {e}")))?
            .create_if_missing(true);
        let pool = SqlitePool::connect_with(options)
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("sqlite connection failed: {e}")))?;
        Ok(Self { pool })
    }

    /// Create a memory service from an existing connection pool.
    pub fn from_pool(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// The registry table used to track applied migration versions.
    const REGISTRY_TABLE: &'static str = "_adk_memory_migrations";

    /// Compiled-in migration steps for the SQLite memory backend.
    ///
    /// Each entry is `(version, description, sql)`. Version 1 is the baseline
    /// that creates the initial schema matching the original `CREATE TABLE IF
    /// NOT EXISTS` / FTS5 / trigger statements.
    const SQLITE_MEMORY_MIGRATIONS: &'static [(i64, &'static str, &'static str)] = &[
        (
            1,
            "create memory_entries table, FTS5 virtual table, and sync triggers",
            "\
CREATE TABLE IF NOT EXISTS memory_entries (\
    id INTEGER PRIMARY KEY AUTOINCREMENT, \
    app_name TEXT NOT NULL, \
    user_id TEXT NOT NULL, \
    session_id TEXT NOT NULL, \
    content TEXT NOT NULL, \
    content_text TEXT NOT NULL, \
    author TEXT NOT NULL, \
    timestamp TEXT NOT NULL\
);\
CREATE INDEX IF NOT EXISTS idx_memory_app_user \
    ON memory_entries(app_name, user_id);\
CREATE VIRTUAL TABLE IF NOT EXISTS memory_entries_fts \
    USING fts5(content_text, content='memory_entries', content_rowid='id');\
CREATE TRIGGER IF NOT EXISTS memory_entries_ai AFTER INSERT ON memory_entries BEGIN \
    INSERT INTO memory_entries_fts(rowid, content_text) VALUES (new.id, new.content_text); \
END;\
CREATE TRIGGER IF NOT EXISTS memory_entries_ad AFTER DELETE ON memory_entries BEGIN \
    INSERT INTO memory_entries_fts(memory_entries_fts, rowid, content_text) VALUES('delete', old.id, old.content_text); \
END;",
        ),
        (
            2,
            "add project_id column and index",
            "\
ALTER TABLE memory_entries ADD COLUMN project_id TEXT;\
CREATE INDEX IF NOT EXISTS idx_memory_project_id ON memory_entries(project_id);",
        ),
    ];

    /// Create the `memory_entries` table and FTS5 virtual table.
    ///
    /// Uses the versioned migration runner to apply schema changes
    /// incrementally. Safe to call multiple times — already-applied
    /// steps are skipped.
    pub async fn migrate(&self) -> Result<()> {
        let pool = self.pool.clone();
        crate::migration::sqlite_runner::run_sql_migrations(
            &pool,
            Self::REGISTRY_TABLE,
            Self::SQLITE_MEMORY_MIGRATIONS,
            || async {
                let row = sqlx::query(
                    "SELECT COUNT(*) AS cnt FROM sqlite_master \
                     WHERE type='table' AND name='memory_entries'",
                )
                .fetch_one(&pool)
                .await
                .map_err(|e| {
                    adk_core::AdkError::memory(format!("baseline detection failed: {e}"))
                })?;
                let count: i64 = row.try_get("cnt").unwrap_or(0);
                Ok(count > 0)
            },
        )
        .await
    }

    /// Returns the highest applied migration version, or 0 if no registry
    /// exists or the registry is empty.
    pub async fn schema_version(&self) -> Result<i64> {
        crate::migration::sqlite_runner::sql_schema_version(&self.pool, Self::REGISTRY_TABLE).await
    }

    /// Import memory entries from a JSON file into the database.
    ///
    /// The file must contain a JSON array of objects, each with at least
    /// `content` (any JSON value) and `author` (string) fields. Optional
    /// fields: `timestamp`, `app_name`, `user_id`.
    ///
    /// Imported entries are appended — existing data is never modified.
    /// Returns the count of successfully imported entries.
    ///
    /// # Errors
    ///
    /// Returns a descriptive error if the file does not exist or contains
    /// invalid JSON.
    pub async fn import_json(&self, path: impl AsRef<Path>) -> Result<u64> {
        let pool = self.pool.clone();
        let path = path.as_ref();

        let file_content = std::fs::read_to_string(path).map_err(|e| {
            adk_core::AdkError::memory(format!("file not found: {}", path.display())).with_source(e)
        })?;

        let entries: Vec<JsonMemoryEntry> = serde_json::from_str(&file_content)
            .map_err(|e| adk_core::AdkError::memory(format!("JSON parse error: {e}")))?;

        let mut count: u64 = 0;
        for entry in &entries {
            let content_json = serde_json::to_string(&entry.content)
                .map_err(|e| adk_core::AdkError::memory(format!("serialization failed: {e}")))?;

            let content_text = extract_content_text(&entry.content);

            let timestamp_str = entry.timestamp.unwrap_or_else(Utc::now).to_rfc3339();

            let app_name = entry.app_name.as_deref().unwrap_or("__import__");

            let user_id = entry.user_id.as_deref().unwrap_or("__import__");

            sqlx::query(
                "INSERT INTO memory_entries \
                 (app_name, user_id, session_id, content, content_text, author, timestamp, project_id) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, NULL)",
            )
            .bind(app_name)
            .bind(user_id)
            .bind("__import__")
            .bind(&content_json)
            .bind(&content_text)
            .bind(&entry.author)
            .bind(&timestamp_str)
            .execute(&pool)
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("insert failed: {e}")))?;

            count += 1;
        }

        Ok(count)
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
        let pool = self.pool.clone();
        if entries.is_empty() {
            return Ok(());
        }

        for entry in &entries {
            let content_json = serde_json::to_string(&entry.content)
                .map_err(|e| adk_core::AdkError::memory(format!("serialization failed: {e}")))?;
            let content_text = crate::text::extract_text(&entry.content);
            let timestamp_str = entry.timestamp.to_rfc3339();

            sqlx::query(
                "INSERT INTO memory_entries \
                 (app_name, user_id, session_id, content, content_text, author, timestamp, project_id) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, NULL)",
            )
            .bind(app_name)
            .bind(user_id)
            .bind(session_id)
            .bind(&content_json)
            .bind(&content_text)
            .bind(&entry.author)
            .bind(&timestamp_str)
            .execute(&pool)
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("insert failed: {e}")))?;
        }

        Ok(())
    }

    #[instrument(skip_all, fields(app_name = %req.app_name, user_id = %req.user_id))]
    async fn search(&self, req: SearchRequest) -> Result<SearchResponse> {
        let pool = self.pool.clone();
        let limit = req.limit.unwrap_or(10) as i64;
        let Some(match_query) = fts5_match_query(&req.query) else {
            return Ok(SearchResponse { memories: Vec::new() });
        };

        let rows = if let Some(ref project_id) = req.project_id {
            sqlx::query(
                r#"
                SELECT m.content, m.author, m.timestamp, f.rank
                FROM memory_entries_fts f
                JOIN memory_entries m ON m.id = f.rowid
                WHERE memory_entries_fts MATCH ?
                  AND m.app_name = ? AND m.user_id = ?
                  AND (m.project_id IS NULL OR m.project_id = ?)
                ORDER BY f.rank
                LIMIT ?
                "#,
            )
            .bind(&match_query)
            .bind(&req.app_name)
            .bind(&req.user_id)
            .bind(project_id)
            .bind(limit)
            .fetch_all(&pool)
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("search failed: {e}")))?
        } else {
            sqlx::query(
                r#"
                SELECT m.content, m.author, m.timestamp, f.rank
                FROM memory_entries_fts f
                JOIN memory_entries m ON m.id = f.rowid
                WHERE memory_entries_fts MATCH ?
                  AND m.app_name = ? AND m.user_id = ?
                  AND m.project_id IS NULL
                ORDER BY f.rank
                LIMIT ?
                "#,
            )
            .bind(&match_query)
            .bind(&req.app_name)
            .bind(&req.user_id)
            .bind(limit)
            .fetch_all(&pool)
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("search failed: {e}")))?
        };

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
        let pool = self.pool.clone();
        sqlx::query("DELETE FROM memory_entries WHERE app_name = ? AND user_id = ?")
            .bind(app_name)
            .bind(user_id)
            .execute(&pool)
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("delete_user failed: {e}")))?;
        Ok(())
    }

    #[instrument(skip_all, fields(app_name = %app_name, user_id = %user_id, session_id = %session_id))]
    async fn delete_session(&self, app_name: &str, user_id: &str, session_id: &str) -> Result<()> {
        let pool = self.pool.clone();
        sqlx::query(
            "DELETE FROM memory_entries WHERE app_name = ? AND user_id = ? AND session_id = ?",
        )
        .bind(app_name)
        .bind(user_id)
        .bind(session_id)
        .execute(&pool)
        .await
        .map_err(|e| adk_core::AdkError::memory(format!("delete_session failed: {e}")))?;
        Ok(())
    }

    #[instrument(skip_all, fields(app_name = %app_name, user_id = %user_id))]
    async fn add_entry(&self, app_name: &str, user_id: &str, entry: MemoryEntry) -> Result<()> {
        let pool = self.pool.clone();
        let content_json = serde_json::to_string(&entry.content)
            .map_err(|e| adk_core::AdkError::memory(format!("serialization failed: {e}")))?;
        let content_text = crate::text::extract_text(&entry.content);
        let timestamp_str = entry.timestamp.to_rfc3339();

        sqlx::query(
            "INSERT INTO memory_entries \
             (app_name, user_id, session_id, content, content_text, author, timestamp, project_id) \
             VALUES (?, ?, ?, ?, ?, ?, ?, NULL)",
        )
        .bind(app_name)
        .bind(user_id)
        .bind("__direct__")
        .bind(&content_json)
        .bind(&content_text)
        .bind(&entry.author)
        .bind(&timestamp_str)
        .execute(&pool)
        .await
        .map_err(|e| adk_core::AdkError::memory(format!("insert failed: {e}")))?;

        Ok(())
    }

    #[instrument(skip_all, fields(app_name = %app_name, user_id = %user_id))]
    async fn delete_entries(&self, app_name: &str, user_id: &str, query: &str) -> Result<u64> {
        let pool = self.pool.clone();
        let Some(match_query) = fts5_match_query(query) else {
            return Ok(0);
        };
        let result = sqlx::query(
            "DELETE FROM memory_entries WHERE id IN (\
                SELECT m.id FROM memory_entries_fts f \
                JOIN memory_entries m ON m.id = f.rowid \
                WHERE memory_entries_fts MATCH ? \
                AND m.app_name = ? AND m.user_id = ? \
                AND m.project_id IS NULL\
            )",
        )
        .bind(&match_query)
        .bind(app_name)
        .bind(user_id)
        .execute(&pool)
        .await
        .map_err(|e| adk_core::AdkError::memory(format!("delete failed: {e}")))?;

        Ok(result.rows_affected())
    }

    #[instrument(skip_all)]
    async fn health_check(&self) -> Result<()> {
        let pool = self.pool.clone();
        sqlx::query("SELECT 1")
            .execute(&pool)
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("health check failed: {e}")))?;
        Ok(())
    }

    fn supports_project_scoping(&self) -> bool {
        true
    }

    #[instrument(skip_all, fields(app_name = %app_name, user_id = %user_id, session_id = %session_id, project_id = %project_id, entry_count = entries.len()))]
    async fn add_session_to_project(
        &self,
        app_name: &str,
        user_id: &str,
        session_id: &str,
        project_id: &str,
        entries: Vec<MemoryEntry>,
    ) -> Result<()> {
        validate_project_id(project_id)?;
        let pool = self.pool.clone();
        if entries.is_empty() {
            return Ok(());
        }

        for entry in &entries {
            let content_json = serde_json::to_string(&entry.content)
                .map_err(|e| adk_core::AdkError::memory(format!("serialization failed: {e}")))?;
            let content_text = crate::text::extract_text(&entry.content);
            let timestamp_str = entry.timestamp.to_rfc3339();

            sqlx::query(
                "INSERT INTO memory_entries \
                 (app_name, user_id, session_id, content, content_text, author, timestamp, project_id) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(app_name)
            .bind(user_id)
            .bind(session_id)
            .bind(&content_json)
            .bind(&content_text)
            .bind(&entry.author)
            .bind(&timestamp_str)
            .bind(project_id)
            .execute(&pool)
            .await
            .map_err(|e| adk_core::AdkError::memory(format!("insert failed: {e}")))?;
        }

        Ok(())
    }

    #[instrument(skip_all, fields(app_name = %app_name, user_id = %user_id, project_id = %project_id))]
    async fn add_entry_to_project(
        &self,
        app_name: &str,
        user_id: &str,
        project_id: &str,
        entry: MemoryEntry,
    ) -> Result<()> {
        validate_project_id(project_id)?;
        let pool = self.pool.clone();
        let content_json = serde_json::to_string(&entry.content)
            .map_err(|e| adk_core::AdkError::memory(format!("serialization failed: {e}")))?;
        let content_text = crate::text::extract_text(&entry.content);
        let timestamp_str = entry.timestamp.to_rfc3339();

        sqlx::query(
            "INSERT INTO memory_entries \
             (app_name, user_id, session_id, content, content_text, author, timestamp, project_id) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(app_name)
        .bind(user_id)
        .bind("__direct__")
        .bind(&content_json)
        .bind(&content_text)
        .bind(&entry.author)
        .bind(&timestamp_str)
        .bind(project_id)
        .execute(&pool)
        .await
        .map_err(|e| adk_core::AdkError::memory(format!("insert failed: {e}")))?;

        Ok(())
    }

    #[instrument(skip_all, fields(app_name = %app_name, user_id = %user_id, project_id = %project_id))]
    async fn delete_entries_in_project(
        &self,
        app_name: &str,
        user_id: &str,
        project_id: &str,
        query: &str,
    ) -> Result<u64> {
        let pool = self.pool.clone();
        let Some(match_query) = fts5_match_query(query) else {
            return Ok(0);
        };
        let result = sqlx::query(
            "DELETE FROM memory_entries WHERE id IN (\
                SELECT m.id FROM memory_entries_fts f \
                JOIN memory_entries m ON m.id = f.rowid \
                WHERE memory_entries_fts MATCH ? \
                AND m.app_name = ? AND m.user_id = ? \
                AND m.project_id = ?\
            )",
        )
        .bind(&match_query)
        .bind(app_name)
        .bind(user_id)
        .bind(project_id)
        .execute(&pool)
        .await
        .map_err(|e| adk_core::AdkError::memory(format!("delete failed: {e}")))?;

        Ok(result.rows_affected())
    }

    #[instrument(skip_all, fields(app_name = %app_name, user_id = %user_id, project_id = %project_id))]
    async fn delete_project(&self, app_name: &str, user_id: &str, project_id: &str) -> Result<u64> {
        let pool = self.pool.clone();
        let result = sqlx::query(
            "DELETE FROM memory_entries WHERE app_name = ? AND user_id = ? AND project_id = ?",
        )
        .bind(app_name)
        .bind(user_id)
        .bind(project_id)
        .execute(&pool)
        .await
        .map_err(|e| adk_core::AdkError::memory(format!("delete_project failed: {e}")))?;

        Ok(result.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::Content;
    use sqlx::sqlite::SqlitePoolOptions;

    const APP: &str = "app";
    const USER: &str = "user";

    async fn service_with(texts: &[&str]) -> SqliteMemoryService {
        // One connection: every connection to `sqlite::memory:` is its own database.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("open in-memory sqlite");
        let service = SqliteMemoryService::from_pool(pool);
        service.migrate().await.expect("migrate");
        let entries = texts
            .iter()
            .map(|text| MemoryEntry {
                content: Content::new("user").with_text(*text),
                author: "user".to_string(),
                timestamp: Utc::now(),
            })
            .collect();
        service.add_session(APP, USER, "s1", entries).await.expect("add_session");
        service
    }

    async fn search_texts(service: &SqliteMemoryService, query: &str) -> Vec<String> {
        let response = service
            .search(SearchRequest {
                query: query.to_string(),
                user_id: USER.to_string(),
                app_name: APP.to_string(),
                limit: None,
                min_score: None,
                project_id: None,
            })
            .await
            .unwrap_or_else(|e| panic!("search for {query:?} failed: {e}"));
        let mut texts: Vec<String> =
            response.memories.iter().map(|m| crate::text::extract_text(&m.content)).collect();
        texts.sort();
        texts
    }

    #[test]
    fn test_fts5_match_query_quotes_terms_and_drops_operators() {
        assert_eq!(
            [
                fts5_match_query("what's my name?"),
                fts5_match_query("rust AND programming OR NOT NEAR"),
                fts5_match_query(r#"say "hi"#),
                fts5_match_query("rust* ^start col:value (x)"),
                fts5_match_query("编程"),
            ],
            [
                Some(r#""what's" "my" "name?""#.to_string()),
                Some(r#""rust" "programming""#.to_string()),
                Some(r#""say" """hi""#.to_string()),
                Some(r#""rust*" "^start" "col:value" "(x)""#.to_string()),
                Some(r#""编程""#.to_string()),
            ]
        );
    }

    #[test]
    fn test_fts5_match_query_without_searchable_terms() {
        for query in ["", "   ", "?", "*", "AND", "OR NOT", "\"", "🦀 !"] {
            assert_eq!(fts5_match_query(query), None, "{query:?}");
        }
    }

    #[tokio::test]
    async fn test_search_accepts_ordinary_text() {
        let service = service_with(&[
            "what's my name? It is Alice",
            "Alice's favourite colour is blue",
            "rust programming is fun",
            "I like python",
            "我喜欢 编程",
        ])
        .await;

        assert_eq!(
            search_texts(&service, "what's my name?").await,
            ["what's my name? It is Alice"]
        );
        assert_eq!(
            search_texts(&service, "Alice's colour?").await,
            ["Alice's favourite colour is blue"]
        );
        assert_eq!(
            search_texts(&service, "rust AND programming").await,
            ["rust programming is fun"]
        );
        assert_eq!(search_texts(&service, "rust*").await, ["rust programming is fun"]);
        assert_eq!(search_texts(&service, "编程").await, ["我喜欢 编程"]);
        assert_eq!(search_texts(&service, "编程？").await, ["我喜欢 编程"]);
    }

    #[tokio::test]
    async fn test_search_with_fts5_syntax_does_not_error() {
        let service = service_with(&["rust programming is fun"]).await;

        for query in [
            "*",
            "?",
            "AND",
            "OR",
            "NOT rust",
            "rust OR python",
            "\"unbalanced",
            "NEAR(rust fun)",
            "content_text:rust",
            "(rust",
            "-rust +fun",
            "^rust",
        ] {
            // Each query must parse; the result set itself is query-specific.
            search_texts(&service, query).await;
        }
        assert_eq!(search_texts(&service, "*").await, Vec::<String>::new());
        assert_eq!(search_texts(&service, "NOT rust").await, ["rust programming is fun"]);
    }

    #[tokio::test]
    async fn test_delete_entries_accepts_ordinary_text() {
        let service = service_with(&["what's my name? It is Alice", "unrelated"]).await;

        assert_eq!(service.delete_entries(APP, USER, "what's my name?").await.unwrap(), 1);
        assert_eq!(service.delete_entries(APP, USER, "*").await.unwrap(), 0);
        assert_eq!(search_texts(&service, "unrelated").await, ["unrelated"]);
    }

    #[tokio::test]
    async fn test_delete_entries_in_project_accepts_ordinary_text() {
        let service = service_with(&[]).await;
        let entry = MemoryEntry {
            content: Content::new("user").with_text("what's the project's deadline?"),
            author: "user".to_string(),
            timestamp: Utc::now(),
        };
        service.add_entry_to_project(APP, USER, "proj", entry).await.unwrap();

        let deleted =
            service.delete_entries_in_project(APP, USER, "proj", "project's deadline?").await;
        assert_eq!(deleted.unwrap(), 1);
    }
}
