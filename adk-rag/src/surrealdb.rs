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
//! use adk_rag::{CollectionSpec, Scope, VectorQuery, VectorStore};
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
//! let scope = Scope::single_tenant();
//! store.create_collection("docs", &CollectionSpec::new(384)).await?;
//! store.upsert("docs", &scope, &chunks).await?;
//! let results = store.search("docs", &scope, &VectorQuery::new(&query_embedding, 5)).await?;
//! ```

use std::collections::HashMap;

use async_trait::async_trait;
use surrealdb::Surreal;
use surrealdb::engine::any::{Any, connect};
use surrealdb_types::SurrealValue;
use tracing::debug;

use crate::document::{Chunk, SearchResult};
use crate::error::{RagError, Result};
use crate::filter::{Field, Filter};
use crate::scope::Scope;
use crate::vectorstore::{CollectionSpec, VectorQuery, VectorStore};

/// Default namespace used by the vector store.
const DEFAULT_NS: &str = "adk_rag";
/// Default database used by the vector store.
const DEFAULT_DB: &str = "vectors";

/// A row stored in SurrealDB representing a chunk with its embedding.
#[derive(Debug, SurrealValue)]
struct ChunkRow {
    scope: String,
    chunk_id: String,
    text: String,
    embedding: Vec<f32>,
    metadata: HashMap<String, String>,
    document_id: String,
    source_uri: Option<String>,
    title: Option<String>,
}

/// A row returned from a KNN search query.
#[derive(Debug, SurrealValue)]
struct SearchRow {
    chunk_id: String,
    text: String,
    metadata: HashMap<String, String>,
    document_id: String,
    source_uri: Option<String>,
    title: Option<String>,
    distance: f32,
}

/// A bound value of a compiled filter.
#[derive(Debug, PartialEq)]
enum Bind {
    Text(String),
    List(Vec<String>),
}

/// A filter compiled to a SurrealQL condition whose every value is a bound parameter.
///
/// Metadata keys are spliced as backtick-quoted field names; [`Filter::validate`] limits them
/// to ASCII letters, digits, and underscores.
#[derive(Debug, PartialEq)]
struct SurrealFilter {
    condition: String,
    binds: Vec<(String, Bind)>,
}

impl SurrealFilter {
    /// Compiles `filter`, or the always-true condition when there is none.
    fn compile(filter: Option<&Filter>) -> Result<Self> {
        let mut compiled = Self { condition: "true".to_string(), binds: Vec::new() };
        if let Some(filter) = filter {
            filter.validate()?;
            compiled.condition = compiled.push(filter);
        }
        Ok(compiled)
    }

    fn param(&mut self, bind: Bind) -> String {
        let name = format!("f{}", self.binds.len());
        self.binds.push((name.clone(), bind));
        format!("${name}")
    }

    fn path(field: &Field) -> String {
        match field {
            Field::Metadata(key) => format!("metadata.`{key}`"),
            Field::DocumentId => "document_id".to_string(),
            Field::SourceUri => "source_uri".to_string(),
            Field::ChunkId => "chunk_id".to_string(),
        }
    }

    /// A missing field is NONE, which equals no string and is in no list, so every leaf is
    /// two-valued and `!` matches a record that lacks the field.
    fn push(&mut self, filter: &Filter) -> String {
        match filter {
            Filter::Eq(field, value) => {
                let value = self.param(Bind::Text(value.clone()));
                format!("({} = {value})", Self::path(field))
            }
            Filter::In(field, values) => {
                let values = self.param(Bind::List(values.clone()));
                format!("({} IN {values})", Self::path(field))
            }
            Filter::Exists(field) => {
                let path = Self::path(field);
                format!("({path} != NONE AND {path} != NULL)")
            }
            Filter::And(filters) | Filter::Or(filters) => {
                let joiner = if matches!(filter, Filter::And(_)) { " AND " } else { " OR " };
                let parts: Vec<String> = filters.iter().map(|filter| self.push(filter)).collect();
                format!("({})", parts.join(joiner))
            }
            Filter::Not(filter) => format!("!{}", self.push(filter)),
        }
    }

    /// Binds the filter's parameters to `query`.
    fn bind<'r, C: surrealdb::Connection>(
        self,
        mut query: surrealdb::method::Query<'r, C>,
    ) -> surrealdb::method::Query<'r, C> {
        for (name, bind) in self.binds {
            query = match bind {
                Bind::Text(value) => query.bind((name, value)),
                Bind::List(values) => query.bind((name, values)),
            };
        }
        query
    }
}

/// A row returned from a count query.
#[derive(Debug, SurrealValue)]
struct CountRow {
    count: i64,
}

/// A [`VectorStore`] backed by [SurrealDB](https://surrealdb.com/).
///
/// Supports three deployment modes:
/// - **In-memory** — zero infrastructure, ideal for tests and development
/// - **RocksDB** — file-backed persistence via embedded RocksDB
/// - **Remote** — connects to a running SurrealDB server via WebSocket
///
/// Each collection maps to a SurrealDB table with an HNSW cosine index
/// on the `embedding` field and an index on `scope`. A [`Filter`] compiles to a SurrealQL
/// condition beside the KNN operator, with every value a bound parameter. A record's ID combines its scope and
/// chunk ID, and the record keeps both as the `scope` and `chunk_id` fields. Records
/// written by an earlier release have no `scope` and are not found; re-ingest them.
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

    /// Maps a collection name to its table name, rejecting names outside the rule.
    ///
    /// Rejecting rather than rewriting keeps the mapping one-to-one, so two collections
    /// never share a table. Queries quote the name, so SurrealQL keywords and leading
    /// digits are valid names.
    fn table_name(name: &str) -> Result<&str> {
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return Err(RagError::VectorStoreError {
                backend: "surrealdb".to_string(),
                message: format!(
                    "invalid collection name `{name}`: use one or more ASCII letters, digits, or \
                     underscores"
                ),
            });
        }
        Ok(name)
    }
}

#[async_trait]
impl VectorStore for SurrealVectorStore {
    async fn create_collection(&self, name: &str, spec: &CollectionSpec) -> Result<()> {
        spec.validate()?;
        let table = Self::table_name(name)?;
        let dimensions = spec.dimensions;

        let sql = format!(
            "DEFINE TABLE IF NOT EXISTS `{table}`; \
             DEFINE FIELD IF NOT EXISTS scope ON `{table}` TYPE string; \
             DEFINE FIELD IF NOT EXISTS chunk_id ON `{table}` TYPE string; \
             DEFINE FIELD IF NOT EXISTS text ON `{table}` TYPE string; \
             DEFINE FIELD IF NOT EXISTS embedding ON `{table}` TYPE array<float>; \
             DEFINE FIELD IF NOT EXISTS metadata ON `{table}` TYPE object; \
             DEFINE FIELD IF NOT EXISTS document_id ON `{table}` TYPE string; \
             DEFINE FIELD IF NOT EXISTS source_uri ON `{table}` TYPE option<string>; \
             DEFINE FIELD IF NOT EXISTS title ON `{table}` TYPE option<string>; \
             DEFINE INDEX IF NOT EXISTS `idx_{table}_scope` ON `{table}` FIELDS scope; \
             DEFINE INDEX IF NOT EXISTS `idx_{table}_hnsw` ON `{table}` \
                 FIELDS embedding HNSW DIMENSION {dimensions} DIST COSINE;"
        );

        self.db.query(&sql).await.map_err(Self::map_err)?.check().map_err(Self::map_err)?;

        debug!(collection = name, table = %table, dimensions, "created surrealdb collection");
        Ok(())
    }

    async fn delete_collection(&self, name: &str) -> Result<()> {
        let table = Self::table_name(name)?;

        self.db.query(format!("REMOVE TABLE IF EXISTS `{table}`;")).await.map_err(Self::map_err)?;

        debug!(collection = name, table = %table, "deleted surrealdb collection");
        Ok(())
    }

    async fn upsert(&self, collection: &str, scope: &Scope, chunks: &[Chunk]) -> Result<()> {
        if chunks.is_empty() {
            return Ok(());
        }

        let table = Self::table_name(collection)?;

        for chunk in chunks {
            let row = ChunkRow {
                scope: scope.as_str().to_string(),
                chunk_id: chunk.id.clone(),
                text: chunk.text.clone(),
                embedding: chunk.embedding.clone(),
                metadata: chunk.metadata.clone(),
                document_id: chunk.document_id.clone(),
                source_uri: chunk.source_uri.clone(),
                title: chunk.title.clone(),
            };

            // The record ID combines the scope and chunk ID, so each scope keeps its own
            // record for a chunk ID.
            let _: Option<ChunkRow> = self
                .db
                .upsert((table, scope.record_key(&chunk.id)))
                .content(row)
                .await
                .map_err(Self::map_err)?;
        }

        debug!(collection, count = chunks.len(), "upserted chunks to surrealdb");
        Ok(())
    }

    async fn delete(&self, collection: &str, scope: &Scope, ids: &[&str]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }

        let table = Self::table_name(collection)?;
        let ids: Vec<String> = ids.iter().map(|id| id.to_string()).collect();

        self.db
            .query(format!("DELETE FROM `{table}` WHERE scope = $scope AND chunk_id IN $ids;"))
            .bind(("scope", scope.as_str().to_string()))
            .bind(("ids", ids.clone()))
            .await
            .map_err(Self::map_err)?
            .check()
            .map_err(Self::map_err)?;

        debug!(collection, count = ids.len(), "deleted chunks from surrealdb");
        Ok(())
    }

    async fn search(
        &self,
        collection: &str,
        scope: &Scope,
        query: &VectorQuery<'_>,
    ) -> Result<Vec<SearchResult>> {
        let table = Self::table_name(collection)?;
        let top_k = query.top_k;

        let filter = SurrealFilter::compile(query.filter)?;
        let condition = &filter.condition;

        // The scope and filter conditions sit beside the KNN operator, so the HNSW search
        // only visits matching records and the nearest `top_k` are chosen among them.
        let sql = format!(
            "SELECT chunk_id, text, metadata, document_id, source_uri, title, \
                    vector::distance::knn() AS distance \
             FROM `{table}` \
             WHERE scope = $scope AND {condition} AND embedding <|{top_k},COSINE|> $embedding \
             ORDER BY distance;"
        );

        let request = self
            .db
            .query(&sql)
            .bind(("scope", scope.as_str().to_string()))
            .bind(("embedding", query.embedding.to_vec()));
        let mut response = filter.bind(request).await.map_err(Self::map_err)?;

        let rows: Vec<SearchRow> = response.take(0).map_err(Self::map_err)?;

        let results = rows
            .into_iter()
            .map(|row| {
                // Cosine distance → similarity: score = 1.0 - distance
                let score = 1.0 - row.distance;
                let mut chunk = Chunk::new(row.chunk_id, row.document_id, row.text);
                chunk.metadata = row.metadata;
                chunk.source_uri = row.source_uri;
                chunk.title = row.title;
                SearchResult { chunk, score }
            })
            .collect();

        Ok(results)
    }

    async fn count(&self, collection: &str, scope: &Scope, filter: Option<&Filter>) -> Result<u64> {
        let table = Self::table_name(collection)?;
        let filter = SurrealFilter::compile(filter)?;
        let condition = &filter.condition;

        let request = self
            .db
            .query(format!(
                "SELECT count() AS count FROM `{table}` WHERE scope = $scope AND {condition} \
                 GROUP ALL;"
            ))
            .bind(("scope", scope.as_str().to_string()));
        let mut response = filter.bind(request).await.map_err(Self::map_err)?;
        let rows: Vec<CountRow> = response.take(0).map_err(Self::map_err)?;
        Ok(rows.first().map_or(0, |row| row.count.unsigned_abs()))
    }

    async fn delete_where(&self, collection: &str, scope: &Scope, filter: &Filter) -> Result<u64> {
        let table = Self::table_name(collection)?;
        let compiled = SurrealFilter::compile(Some(filter))?;
        let condition = &compiled.condition;

        let count = self.count(collection, scope, Some(filter)).await?;
        let request = self
            .db
            .query(format!("DELETE FROM `{table}` WHERE scope = $scope AND {condition};"))
            .bind(("scope", scope.as_str().to_string()));
        compiled.bind(request).await.map_err(Self::map_err)?.check().map_err(Self::map_err)?;

        debug!(collection, deleted = count, "deleted matching chunks from surrealdb");
        Ok(count)
    }

    async fn purge_scope(&self, collection: &str, scope: &Scope) -> Result<u64> {
        let table = Self::table_name(collection)?;

        let count = self.count(collection, scope, None).await?;
        self.db
            .query(format!("DELETE FROM `{table}` WHERE scope = $scope;"))
            .bind(("scope", scope.as_str().to_string()))
            .await
            .map_err(Self::map_err)?
            .check()
            .map_err(Self::map_err)?;

        debug!(collection, purged = count, "purged scope from surrealdb");
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_compile_to_bound_values_and_quoted_keys() {
        let injection = "x' OR true OR '";
        let filter = Filter::or([
            Filter::eq(Field::metadata("select"), injection),
            !Filter::is_in(Field::ChunkId, ["a", "b"]),
            Filter::exists(Field::SourceUri),
        ]);
        let compiled = SurrealFilter::compile(Some(&filter)).expect("compiles");
        assert_eq!(
            compiled,
            SurrealFilter {
                condition: "((metadata.`select` = $f0) OR !(chunk_id IN $f1) OR \
                            (source_uri != NONE AND source_uri != NULL))"
                    .to_string(),
                binds: vec![
                    ("f0".to_string(), Bind::Text(injection.to_string())),
                    ("f1".to_string(), Bind::List(vec!["a".to_string(), "b".to_string()])),
                ],
            }
        );
        assert!(SurrealFilter::compile(Some(&Filter::eq(Field::metadata("a`b"), "x"))).is_err());
    }
}
