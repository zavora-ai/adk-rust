//! pgvector (PostgreSQL) vector store backend.
//!
//! Provides [`PgVectorStore`] which implements [`VectorStore`] using
//! [sqlx](https://docs.rs/sqlx) with the
//! [pgvector](https://github.com/pgvector/pgvector) PostgreSQL extension.
//!
//! # Prerequisites
//!
//! - PostgreSQL with the `pgvector` extension installed
//! - The extension must be created: `CREATE EXTENSION IF NOT EXISTS vector;`
//!
//! # Collection names
//!
//! Each collection is stored in the table `rag_<name>`. A name must be non-empty,
//! contain only ASCII lowercase letters, digits and underscores, and be at most 59
//! bytes long, so the table name fits PostgreSQL's 63-byte identifier limit. Any
//! other name is rejected with [`RagError::VectorStoreError`] instead of being
//! rewritten, so two distinct names never share a table.
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_rag::pgvector::PgVectorStore;
//!
//! let store = PgVectorStore::new("postgres://user:pass@localhost/mydb").await?;
//! store.create_collection("docs", 384).await?;
//! store.upsert("docs", &chunks).await?;
//! let results = store.search("docs", &query_embedding, 5).await?;
//! ```

use std::collections::HashMap;

use async_trait::async_trait;
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
use tracing::debug;

use crate::document::{Chunk, SearchResult};
use crate::error::{RagError, Result};
use crate::vectorstore::VectorStore;

/// A [`VectorStore`] backed by PostgreSQL with the pgvector extension.
///
/// Each collection is stored as a separate table named `rag_<name>` with columns:
/// `id`, `text`, `embedding` (vector), `metadata` (jsonb), `document_id`, `source_uri`,
/// and `title`. `create_collection` adds `source_uri` and `title` to a table created
/// before they existed, keeping its rows. See the
/// [module documentation](self) for the collection naming rule.
pub struct PgVectorStore {
    pool: PgPool,
}

impl PgVectorStore {
    /// Create a new pgvector store by connecting to the given database URL.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use adk_rag::pgvector::PgVectorStore;
    ///
    /// # async fn example() -> adk_rag::Result<()> {
    /// let store = PgVectorStore::new("postgres://user:pass@localhost/mydb").await?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`RagError::VectorStoreError`] when the database cannot be reached.
    pub async fn new(database_url: &str) -> Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(database_url)
            .await
            .map_err(Self::map_err)?;
        Ok(Self { pool })
    }

    /// Create a new pgvector store from an existing connection pool.
    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    fn map_err(e: sqlx::Error) -> RagError {
        RagError::VectorStoreError { backend: "pgvector".to_string(), message: e.to_string() }
    }

    /// Maps a collection name to its table name, rejecting names outside the rule.
    ///
    /// Rejecting rather than rewriting keeps the mapping one-to-one: PostgreSQL folds
    /// unquoted identifiers to lowercase and truncates them past 63 bytes, so any
    /// rewrite could send two collections to one table.
    fn table_name(name: &str) -> Result<String> {
        const PREFIX: &str = "rag_";
        if name.is_empty()
            || PREFIX.len() + name.len() > 63
            || !name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        {
            return Err(RagError::VectorStoreError {
                backend: "pgvector".to_string(),
                message: format!(
                    "invalid collection name `{name}`: use 1 to 59 ASCII lowercase letters, \
                     digits or underscores"
                ),
            });
        }
        Ok(format!("{PREFIX}{name}"))
    }
}

#[async_trait]
impl VectorStore for PgVectorStore {
    async fn create_collection(&self, name: &str, dimensions: usize) -> Result<()> {
        let table_name = Self::table_name(name)?;

        // Ensure the pgvector extension exists
        sqlx::query("CREATE EXTENSION IF NOT EXISTS vector")
            .execute(&self.pool)
            .await
            .map_err(Self::map_err)?;

        // `table_name` passed `Self::table_name` and `dimensions` is an integer.
        let create_sql = format!(
            "CREATE TABLE IF NOT EXISTS {table_name} (\
                id TEXT PRIMARY KEY, \
                text TEXT NOT NULL, \
                embedding vector({dimensions}), \
                metadata JSONB NOT NULL DEFAULT '{{}}'::jsonb, \
                document_id TEXT NOT NULL, \
                source_uri TEXT, \
                title TEXT\
            )"
        );
        // Tables created before provenance was stored gain the columns in place.
        let migrate_sql = format!(
            "ALTER TABLE {table_name} \
                ADD COLUMN IF NOT EXISTS source_uri TEXT, \
                ADD COLUMN IF NOT EXISTS title TEXT"
        );

        for sql in [create_sql, migrate_sql] {
            sqlx::query(sqlx::AssertSqlSafe(sql.as_str()))
                .execute(&self.pool)
                .await
                .map_err(Self::map_err)?;
        }

        debug!(collection = name, table = %table_name, dimensions, "created pgvector table");
        Ok(())
    }

    async fn delete_collection(&self, name: &str) -> Result<()> {
        let table_name = Self::table_name(name)?;

        // `table_name` passed `Self::table_name`.
        let drop_sql = format!("DROP TABLE IF EXISTS {table_name}");
        sqlx::query(sqlx::AssertSqlSafe(drop_sql.as_str()))
            .execute(&self.pool)
            .await
            .map_err(Self::map_err)?;

        debug!(collection = name, table = %table_name, "deleted pgvector table");
        Ok(())
    }

    async fn upsert(&self, collection: &str, chunks: &[Chunk]) -> Result<()> {
        if chunks.is_empty() {
            return Ok(());
        }

        let table_name = Self::table_name(collection)?;

        // `table_name` passed `Self::table_name`; every value is a bound parameter.
        let upsert_sql = format!(
            "INSERT INTO {table_name} \
                (id, text, embedding, metadata, document_id, source_uri, title) \
             VALUES ($1, $2, $3::vector, $4::jsonb, $5, $6, $7) \
             ON CONFLICT (id) DO UPDATE SET \
                text = EXCLUDED.text, \
                embedding = EXCLUDED.embedding, \
                metadata = EXCLUDED.metadata, \
                document_id = EXCLUDED.document_id, \
                source_uri = EXCLUDED.source_uri, \
                title = EXCLUDED.title"
        );

        for chunk in chunks {
            let metadata_json =
                serde_json::to_string(&chunk.metadata).unwrap_or_else(|_| "{}".to_string());

            // pgvector expects the vector as a string like '[1.0, 2.0, 3.0]'
            let embedding_str = format!(
                "[{}]",
                chunk.embedding.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(",")
            );

            sqlx::query(sqlx::AssertSqlSafe(upsert_sql.as_str()))
                .bind(&chunk.id)
                .bind(&chunk.text)
                .bind(&embedding_str)
                .bind(&metadata_json)
                .bind(&chunk.document_id)
                .bind(&chunk.source_uri)
                .bind(&chunk.title)
                .execute(&self.pool)
                .await
                .map_err(Self::map_err)?;
        }

        debug!(collection, count = chunks.len(), "upserted chunks to pgvector");
        Ok(())
    }

    async fn delete(&self, collection: &str, ids: &[&str]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }

        let table_name = Self::table_name(collection)?;

        // `table_name` passed `Self::table_name`; the ids are a bound parameter.
        let delete_sql = format!("DELETE FROM {table_name} WHERE id = ANY($1)");
        let id_vec: Vec<String> = ids.iter().map(|s| s.to_string()).collect();

        sqlx::query(sqlx::AssertSqlSafe(delete_sql.as_str()))
            .bind(&id_vec)
            .execute(&self.pool)
            .await
            .map_err(Self::map_err)?;

        debug!(collection, count = ids.len(), "deleted chunks from pgvector");
        Ok(())
    }

    async fn search(
        &self,
        collection: &str,
        embedding: &[f32],
        top_k: usize,
    ) -> Result<Vec<SearchResult>> {
        let table_name = Self::table_name(collection)?;

        // pgvector cosine distance operator: <=>
        // Returns distance (0 = identical), so score = 1 - distance.
        // `table_name` passed `Self::table_name`; the query vector is a bound parameter.
        let search_sql = format!(
            "SELECT id, text, metadata, document_id, source_uri, title, \
                    1 - (embedding <=> $1::vector) AS score \
             FROM {table_name} \
             ORDER BY embedding <=> $1::vector \
             LIMIT $2"
        );

        let embedding_str =
            format!("[{}]", embedding.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(","));

        let rows = sqlx::query(sqlx::AssertSqlSafe(search_sql.as_str()))
            .bind(&embedding_str)
            .bind(top_k as i64)
            .fetch_all(&self.pool)
            .await
            .map_err(Self::map_err)?;

        let results = rows
            .iter()
            .map(|row| {
                let id: String = row.get("id");
                let text: String = row.get("text");
                let document_id: String = row.get("document_id");
                let score: f64 = row.get("score");
                let metadata_value: serde_json::Value = row.get("metadata");
                let metadata: HashMap<String, String> = metadata_value
                    .as_object()
                    .map(|obj| {
                        obj.iter()
                            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                            .collect()
                    })
                    .unwrap_or_default();

                let mut chunk = Chunk::new(id, document_id, text);
                chunk.metadata = metadata;
                chunk.source_uri = row.get("source_uri");
                chunk.title = row.get("title");
                SearchResult { chunk, score: score as f32 }
            })
            .collect();

        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::PgVectorStore;

    #[test]
    fn accepts_lowercase_ascii_identifiers() {
        for (name, table) in [
            ("docs", "rag_docs"),
            ("docs_v2", "rag_docs_v2"),
            ("2026_reports", "rag_2026_reports"),
            ("_", "rag__"),
        ] {
            assert_eq!(PgVectorStore::table_name(name).unwrap(), table, "{name}");
        }
        let longest = "a".repeat(59);
        assert_eq!(PgVectorStore::table_name(&longest).unwrap(), format!("rag_{longest}"));
    }

    #[test]
    fn rejects_names_that_could_share_a_table() {
        for name in
            ["", "Docs", "a-b", "a b", "a.b", "caf\u{e9}", "docs;drop", "\"docs\"", &"a".repeat(60)]
        {
            let error = PgVectorStore::table_name(name).unwrap_err();
            assert!(error.to_string().contains("invalid collection name"), "{name}: {error}");
        }
    }
}
