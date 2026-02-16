//! SurrealDB vector store backend.
//!
//! Provides [`SurrealVectorStore`] which implements [`VectorStore`] using
//! [SurrealDB](https://surrealdb.com/) — a multi-model database written in
//! Rust with native HNSW vector indexing and KNN search.
//!
//! SurrealDB can run embedded (in-process, zero infrastructure) or connect
//! to a remote server, making it suitable for both development and production.
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_rag::surrealdb::SurrealVectorStore;
//!
//! // In-memory embedded (no server needed)
//! let store = SurrealVectorStore::in_memory().await?;
//!
//! // File-backed embedded (RocksDB)
//! let store = SurrealVectorStore::rocksdb("data/my-vectors").await?;
//!
//! // Remote server
//! let store = SurrealVectorStore::remote("ws://localhost:8000").await?;
//!
//! store.create_collection("docs", 384).await?;
//! store.upsert("docs", &chunks).await?;
//! let results = store.search("docs", &query_embedding, 5).await?;
//! ```

use std::collections::HashMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use surrealdb::Surreal;
use surrealdb::engine::any::{Any, connect};
use tracing::debug;

use crate::document::{Chunk, SearchResult};
use crate::error::{RagError, Result};
use crate::vectorstore::VectorStore;

/// Default namespace used by the vector store.
const DEFAULT_NS: &str = "adk_rag";
/// Default database used by the vector store.
const DEFAULT_DB: &str = "vectors";

/// A row stored in SurrealDB representing a chunk with its embedding.
#[derive(Debug, Serialize, Deserialize)]
struct ChunkRow {
    text: String,
    embedding: Vec<f32>,
    metadata: HashMap<String, String>,
    document_id: String,
}

/// A row returned from a KNN search query.
#[derive(Debug, Deserialize)]
struct SearchRow {
    id: surrealdb::RecordId,
    text: String,
    metadata: HashMap<String, String>,
    document_id: String,
    distance: f32,
}

/// A [`VectorStore`] backed by [SurrealDB](https://surrealdb.com/).
///
/// Supports three deployment modes:
/// - **In-memory** — zero infrastructure, ideal for tests and development
/// - **RocksDB** — file-backed persistence via embedded RocksDB
/// - **Remote** — connects to a running SurrealDB server via WebSocket
///
/// Each collection maps to a SurrealDB table with an HNSW cosine index
/// on the `embedding` field.
pub struct SurrealVectorStore {
    db: Surreal<Any>,
}

impl SurrealVectorStore {
    /// Create an in-memory embedded SurrealDB vector store.
    ///
    /// No server or files needed. Data is lost when the process exits.
    pub async fn in_memory() -> Result<Self> {
        let db = connect("mem://").await.map_err(Self::map_err)?;
        db.use_ns(DEFAULT_NS).use_db(DEFAULT_DB).await.map_err(Self::map_err)?;
        Ok(Self { db })
    }

    /// Create a file-backed embedded SurrealDB vector store using RocksDB.
    ///
    /// Data persists across restarts. Requires the `kv-rocksdb` feature
    /// on the `surrealdb` crate (enabled by default in this integration).
    pub async fn rocksdb(path: &str) -> Result<Self> {
        let db = connect(format!("rocksdb://{path}")).await.map_err(Self::map_err)?;
        db.use_ns(DEFAULT_NS).use_db(DEFAULT_DB).await.map_err(Self::map_err)?;
        Ok(Self { db })
    }

    /// Connect to a remote SurrealDB server.
    ///
    /// The URL should be a WebSocket endpoint, e.g. `ws://localhost:8000`.
    pub async fn remote(url: &str) -> Result<Self> {
        let db = connect(url).await.map_err(Self::map_err)?;
        db.use_ns(DEFAULT_NS).use_db(DEFAULT_DB).await.map_err(Self::map_err)?;
        Ok(Self { db })
    }

    /// Create a vector store from an existing SurrealDB connection.
    ///
    /// The caller is responsible for selecting the namespace and database
    /// before passing the connection.
    pub fn from_connection(db: Surreal<Any>) -> Self {
        Self { db }
    }

    fn map_err(e: surrealdb::Error) -> RagError {
        RagError::VectorStoreError { backend: "surrealdb".to_string(), message: e.to_string() }
    }

    /// Sanitize a collection name for use as a SurrealDB table name.
    fn sanitize_table_name(name: &str) -> Result<String> {
        let sanitized: String =
            name.chars().map(|c| if c.is_alphanumeric() || c == '_' { c } else { '_' }).collect();
        if sanitized.is_empty() {
            return Err(RagError::VectorStoreError {
                backend: "surrealdb".to_string(),
                message: "collection name is empty after sanitization".to_string(),
            });
        }
        Ok(sanitized)
    }
}

#[async_trait]
impl VectorStore for SurrealVectorStore {
    async fn create_collection(&self, name: &str, dimensions: usize) -> Result<()> {
        let table = Self::sanitize_table_name(name)?;

        let sql = format!(
            "DEFINE TABLE IF NOT EXISTS {table}; \
             DEFINE FIELD IF NOT EXISTS text ON {table} TYPE string; \
             DEFINE FIELD IF NOT EXISTS embedding ON {table} TYPE array<float>; \
             DEFINE FIELD IF NOT EXISTS metadata ON {table} FLEXIBLE TYPE object; \
             DEFINE FIELD IF NOT EXISTS document_id ON {table} TYPE string; \
             DEFINE INDEX IF NOT EXISTS idx_{table}_hnsw ON {table} \
                 FIELDS embedding HNSW DIMENSION {dimensions} DIST COSINE;"
        );

        self.db.query(&sql).await.map_err(Self::map_err)?;

        debug!(collection = name, table = %table, dimensions, "created surrealdb collection");
        Ok(())
    }

    async fn delete_collection(&self, name: &str) -> Result<()> {
        let table = Self::sanitize_table_name(name)?;

        self.db.query(format!("REMOVE TABLE IF EXISTS {table};")).await.map_err(Self::map_err)?;

        debug!(collection = name, table = %table, "deleted surrealdb collection");
        Ok(())
    }

    async fn upsert(&self, collection: &str, chunks: &[Chunk]) -> Result<()> {
        if chunks.is_empty() {
            return Ok(());
        }

        let table = Self::sanitize_table_name(collection)?;

        for chunk in chunks {
            let row = ChunkRow {
                text: chunk.text.clone(),
                embedding: chunk.embedding.clone(),
                metadata: chunk.metadata.clone(),
                document_id: chunk.document_id.clone(),
            };

            // Use record ID as (table, chunk.id) for upsert semantics
            let _: Option<ChunkRow> = self
                .db
                .upsert((&table as &str, &chunk.id as &str))
                .content(row)
                .await
                .map_err(Self::map_err)?;
        }

        debug!(collection, count = chunks.len(), "upserted chunks to surrealdb");
        Ok(())
    }

    async fn delete(&self, collection: &str, ids: &[&str]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }

        let table = Self::sanitize_table_name(collection)?;

        for id in ids {
            let _: Option<ChunkRow> =
                self.db.delete((&table as &str, *id)).await.map_err(Self::map_err)?;
        }

        debug!(collection, count = ids.len(), "deleted chunks from surrealdb");
        Ok(())
    }

    async fn search(
        &self,
        collection: &str,
        embedding: &[f32],
        top_k: usize,
    ) -> Result<Vec<SearchResult>> {
        let table = Self::sanitize_table_name(collection)?;

        // Use SurrealDB's KNN operator with cosine distance
        let sql = format!(
            "SELECT id, text, metadata, document_id, \
                    vector::distance::knn() AS distance \
             FROM {table} \
             WHERE embedding <|{top_k},COSINE|> $embedding \
             ORDER BY distance;"
        );

        let embedding_vec: Vec<f32> = embedding.to_vec();

        let mut response =
            self.db.query(&sql).bind(("embedding", embedding_vec)).await.map_err(Self::map_err)?;

        let rows: Vec<SearchRow> = response.take(0).map_err(Self::map_err)?;

        let results = rows
            .into_iter()
            .map(|row| {
                // SurrealDB record IDs are in the format "table:id"
                let id = row.id.key().to_string();
                // Cosine distance → similarity: score = 1.0 - distance
                let score = 1.0 - row.distance;

                SearchResult {
                    chunk: Chunk {
                        id,
                        text: row.text,
                        embedding: vec![],
                        metadata: row.metadata,
                        document_id: row.document_id,
                    },
                    score,
                }
            })
            .collect();

        Ok(results)
    }
}
