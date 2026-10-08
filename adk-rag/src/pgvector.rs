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
use sqlx::{AssertSqlSafe, PgPool, Row};
use tracing::debug;

use crate::document::{Chunk, SearchResult};
use crate::error::{RagError, Result};
use crate::vectorstore::VectorStore;

/// A [`VectorStore`] backed by PostgreSQL with the pgvector extension.
///
/// Each collection is stored as a separate table with columns:
/// `id`, `text`, `embedding` (vector), `metadata` (jsonb), `document_id`.
pub struct PgVectorStore {
    pool: PgPool,
}

impl PgVectorStore {
    /// Create a new pgvector store by connecting to the given database URL.
    pub async fn new(database_url: &str) -> std::result::Result<Self, sqlx::Error> {
        let pool = PgPoolOptions::new().max_connections(5).connect(database_url).await?;
        Ok(Self { pool })
    }

    /// Create a new pgvector store from an existing connection pool.
    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    fn map_err(e: sqlx::Error) -> RagError {
        RagError::VectorStoreError { backend: "pgvector".to_string(), message: e.to_string() }
    }

    /// Sanitize a collection name for use as a table name.
    /// Only allows alphanumeric characters and underscores.
    ///
    /// Every query that formats a table name in wraps it in `AssertSqlSafe` on
    /// the strength of this function; the other formatted values are integers.
    fn sanitize_table_name(name: &str) -> Result<String> {
        let sanitized: String =
            name.chars().map(|c| if c.is_alphanumeric() || c == '_' { c } else { '_' }).collect();
        if sanitized.is_empty() {
            return Err(RagError::VectorStoreError {
                backend: "pgvector".to_string(),
                message: "collection name is empty after sanitization".to_string(),
            });
        }
        Ok(format!("rag_{sanitized}"))
    }
}

#[async_trait]
impl VectorStore for PgVectorStore {
    async fn create_collection(&self, name: &str, dimensions: usize) -> Result<()> {
        let table_name = Self::sanitize_table_name(name)?;

        // Ensure the pgvector extension exists
        sqlx::query("CREATE EXTENSION IF NOT EXISTS vector")
            .execute(&self.pool)
            .await
            .map_err(Self::map_err)?;

        let create_sql = format!(
            "CREATE TABLE IF NOT EXISTS {table_name} (\
                id TEXT PRIMARY KEY, \
                text TEXT NOT NULL, \
                embedding vector({dimensions}), \
                metadata JSONB NOT NULL DEFAULT '{{}}'::jsonb, \
                document_id TEXT NOT NULL\
            )"
        );

        sqlx::query(AssertSqlSafe(create_sql)).execute(&self.pool).await.map_err(Self::map_err)?;

        debug!(collection = name, table = %table_name, dimensions, "created pgvector table");
        Ok(())
    }

    async fn delete_collection(&self, name: &str) -> Result<()> {
        let table_name = Self::sanitize_table_name(name)?;

        let drop_sql = format!("DROP TABLE IF EXISTS {table_name}");
        sqlx::query(AssertSqlSafe(drop_sql)).execute(&self.pool).await.map_err(Self::map_err)?;

        debug!(collection = name, table = %table_name, "deleted pgvector table");
        Ok(())
    }

    async fn upsert(&self, collection: &str, chunks: &[Chunk]) -> Result<()> {
        if chunks.is_empty() {
            return Ok(());
        }

        let table_name = Self::sanitize_table_name(collection)?;

        let upsert_sql = format!(
            "INSERT INTO {table_name} (id, text, embedding, metadata, document_id) \
             VALUES ($1, $2, $3::vector, $4::jsonb, $5) \
             ON CONFLICT (id) DO UPDATE SET \
                text = EXCLUDED.text, \
                embedding = EXCLUDED.embedding, \
                metadata = EXCLUDED.metadata, \
                document_id = EXCLUDED.document_id"
        );

        for chunk in chunks {
            let metadata_json =
                serde_json::to_string(&chunk.metadata).unwrap_or_else(|_| "{}".to_string());

            // pgvector expects the vector as a string like '[1.0, 2.0, 3.0]'
            let embedding_str = format!(
                "[{}]",
                chunk.embedding.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(",")
            );

            sqlx::query(AssertSqlSafe(upsert_sql.as_str()))
                .bind(&chunk.id)
                .bind(&chunk.text)
                .bind(&embedding_str)
                .bind(&metadata_json)
                .bind(&chunk.document_id)
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

        let table_name = Self::sanitize_table_name(collection)?;

        // Use parameterized ANY($1) for safe deletion
        let delete_sql = format!("DELETE FROM {table_name} WHERE id = ANY($1)");
        let id_vec: Vec<String> = ids.iter().map(|s| s.to_string()).collect();

        sqlx::query(AssertSqlSafe(delete_sql))
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
        let table_name = Self::sanitize_table_name(collection)?;

        // pgvector cosine distance operator: <=>
        // Returns distance (0 = identical), so score = 1 - distance
        let search_sql = format!(
            "SELECT id, text, metadata, document_id, \
                    1 - (embedding <=> $1::vector) AS score \
             FROM {table_name} \
             ORDER BY embedding <=> $1::vector \
             LIMIT $2"
        );

        let embedding_str =
            format!("[{}]", embedding.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(","));

        let rows = sqlx::query(AssertSqlSafe(search_sql))
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

                SearchResult {
                    chunk: Chunk { id, text, embedding: vec![], metadata, document_id },
                    score: score as f32,
                }
            })
            .collect();

        Ok(results)
    }
}
