//! LanceDB vector store backend.
//!
//! Provides [`LanceDBVectorStore`] which implements [`VectorStore`] using
//! the [lancedb](https://docs.rs/lancedb) crate in embedded mode.
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_rag::lancedb::LanceDBVectorStore;
//!
//! let store = LanceDBVectorStore::new("data/my-vectors").await?;
//! store.create_collection("docs", 384).await?;
//! store.upsert("docs", &chunks).await?;
//! let results = store.search("docs", &query_embedding, 5).await?;
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use futures::TryStreamExt;
use lancedb::Connection;
// All arrow types come from lancedb's own re-exports, so their arrow version is stated
// exactly once (by lancedb) and cannot drift out from under us. See the comment on the
// `lancedb` dependency in Cargo.toml.
use lancedb::arrow::arrow_array::cast::AsArray;
use lancedb::arrow::arrow_array::types::Float32Type;
use lancedb::arrow::arrow_array::{
    Array, FixedSizeListArray, Float32Array, RecordBatch, RecordBatchIterator, RecordBatchReader,
    StringArray,
};
use lancedb::arrow::arrow_schema::{DataType, Field, Schema};
use lancedb::expr::{DfExpr, col, is_in, lit};
use lancedb::query::{ExecutableQuery, QueryBase};
use tracing::debug;

use crate::document::{Chunk, SearchResult};
use crate::error::{RagError, Result};
use crate::vectorstore::VectorStore;

/// A [`VectorStore`] backed by [LanceDB](https://lancedb.github.io/lancedb/).
///
/// Wraps a [`lancedb::Connection`] and maps collections to LanceDB tables.
/// Runs in embedded mode by default using a local directory for storage.
///
/// `upsert` replaces rows by chunk id through a merge-insert, so ingesting the
/// same chunks again updates them instead of adding duplicates. Every embedding
/// in an `upsert` must match the collection's dimensions.
///
/// A chunk's `source_uri` and `title` are columns of their own. A table created
/// before those columns existed fails `upsert` with guidance to re-create it and
/// re-ingest its documents.
pub struct LanceDBVectorStore {
    connection: Connection,
}

impl LanceDBVectorStore {
    /// Create a new LanceDB vector store at the given storage path.
    pub async fn new(path: &str) -> std::result::Result<Self, lancedb::Error> {
        let connection = lancedb::connect(path).execute().await?;
        Ok(Self { connection })
    }

    /// Create a new LanceDB vector store from an existing connection.
    pub fn from_connection(connection: Connection) -> Self {
        Self { connection }
    }

    fn map_err(e: lancedb::Error) -> RagError {
        RagError::VectorStoreError { backend: "lancedb".to_string(), message: e.to_string() }
    }

    fn store_error(message: String) -> RagError {
        RagError::VectorStoreError { backend: "lancedb".to_string(), message }
    }

    /// Checks a collection name against LanceDB's table-name rule before any call.
    ///
    /// lancedb 0.31 panics instead of erroring when it creates or opens a table whose
    /// name breaks the rule.
    fn table_name(name: &str) -> Result<&str> {
        if name.is_empty()
            || !name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
        {
            return Err(Self::store_error(format!(
                "invalid collection name `{name}`: use one or more ASCII letters, digits, \
                 underscores, hyphens, or periods"
            )));
        }
        Ok(name)
    }

    /// Builds the `id IN (...)` predicate that selects rows by chunk id.
    ///
    /// Ids become typed string literals in a DataFusion expression rather than
    /// text spliced into SQL, so a quote in an id cannot alter the predicate.
    fn id_predicate(ids: &[&str]) -> DfExpr {
        is_in(col("id"), ids.iter().map(|id| lit(*id)).collect())
    }

    fn build_schema(dimensions: usize) -> Arc<Schema> {
        Arc::new(Schema::new(vec![
            Field::new("id", DataType::Utf8, false),
            Field::new("text", DataType::Utf8, false),
            Field::new(
                "vector",
                DataType::FixedSizeList(
                    Arc::new(Field::new("item", DataType::Float32, true)),
                    dimensions as i32,
                ),
                true,
            ),
            Field::new("document_id", DataType::Utf8, false),
            Field::new("metadata_json", DataType::Utf8, false),
            Field::new("source_uri", DataType::Utf8, true),
            Field::new("title", DataType::Utf8, true),
        ]))
    }
}

#[async_trait]
impl VectorStore for LanceDBVectorStore {
    async fn create_collection(&self, name: &str, dimensions: usize) -> Result<()> {
        let name = Self::table_name(name)?;
        let tables = self.connection.table_names().execute().await.map_err(Self::map_err)?;
        if tables.iter().any(|t| t == name) {
            debug!(collection = name, "lancedb table already exists, skipping creation");
            return Ok(());
        }

        let schema = Self::build_schema(dimensions);
        self.connection.create_empty_table(name, schema).execute().await.map_err(Self::map_err)?;

        debug!(collection = name, dimensions, "created lancedb table");
        Ok(())
    }

    async fn delete_collection(&self, name: &str) -> Result<()> {
        self.connection.drop_table(Self::table_name(name)?, &[]).await.map_err(Self::map_err)?;
        debug!(collection = name, "deleted lancedb table");
        Ok(())
    }

    async fn upsert(&self, collection: &str, chunks: &[Chunk]) -> Result<()> {
        if chunks.is_empty() {
            return Ok(());
        }

        // Merge-insert rejects a source batch with two rows for one id; the last
        // occurrence wins, matching the other backends.
        let mut last_index: HashMap<&str, usize> = HashMap::with_capacity(chunks.len());
        for (i, chunk) in chunks.iter().enumerate() {
            last_index.insert(chunk.id.as_str(), i);
        }
        let chunks: Vec<&Chunk> = chunks
            .iter()
            .enumerate()
            .filter(|(i, chunk)| last_index[chunk.id.as_str()] == *i)
            .map(|(_, chunk)| chunk)
            .collect();

        let table = self
            .connection
            .open_table(Self::table_name(collection)?)
            .execute()
            .await
            .map_err(Self::map_err)?;
        let table_schema = table.schema().await.map_err(Self::map_err)?;
        let dimensions = match table_schema.field_with_name("vector").map(Field::data_type) {
            Ok(DataType::FixedSizeList(_, size)) => *size as usize,
            _ => {
                return Err(Self::store_error(format!(
                    "collection '{collection}' has no fixed-size `vector` column; \
                     create it with `create_collection`"
                )));
            }
        };
        if table_schema.field_with_name("source_uri").is_err() {
            return Err(Self::store_error(format!(
                "collection '{collection}' predates the `source_uri` and `title` columns; \
                 delete it, create it again, and re-ingest its documents"
            )));
        }
        if let Some(chunk) = chunks.iter().find(|c| c.embedding.len() != dimensions) {
            return Err(Self::store_error(format!(
                "chunk '{}' has a {}-dimensional embedding but collection '{collection}' \
                 stores {dimensions} dimensions",
                chunk.id,
                chunk.embedding.len()
            )));
        }
        let schema = Self::build_schema(dimensions);

        let ids: Vec<&str> = chunks.iter().map(|c| c.id.as_str()).collect();
        let texts: Vec<&str> = chunks.iter().map(|c| c.text.as_str()).collect();
        let doc_ids: Vec<&str> = chunks.iter().map(|c| c.document_id.as_str()).collect();
        let metadata_jsons: Vec<String> = chunks
            .iter()
            .map(|c| serde_json::to_string(&c.metadata).unwrap_or_else(|_| "{}".to_string()))
            .collect();
        let metadata_refs: Vec<&str> = metadata_jsons.iter().map(|s| s.as_str()).collect();
        let source_uris: Vec<Option<&str>> =
            chunks.iter().map(|c| c.source_uri.as_deref()).collect();
        let titles: Vec<Option<&str>> = chunks.iter().map(|c| c.title.as_deref()).collect();

        let all_values: Vec<f32> =
            chunks.iter().flat_map(|c| c.embedding.iter().copied()).collect();
        let values_array = Arc::new(Float32Array::from(all_values));
        let list_field = Arc::new(Field::new("item", DataType::Float32, true));
        let vector_array =
            FixedSizeListArray::try_new(list_field, dimensions as i32, values_array, None)
                .map_err(|e| Self::store_error(format!("failed to build vector column: {e}")))?;

        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(StringArray::from(ids)),
                Arc::new(StringArray::from(texts)),
                Arc::new(vector_array) as Arc<dyn Array>,
                Arc::new(StringArray::from(doc_ids)),
                Arc::new(StringArray::from(metadata_refs)),
                Arc::new(StringArray::from(source_uris)),
                Arc::new(StringArray::from(titles)),
            ],
        )
        .map_err(|e| Self::store_error(format!("failed to build record batch: {e}")))?;

        let batches: Box<dyn RecordBatchReader + Send> =
            Box::new(RecordBatchIterator::new(vec![Ok(batch)], schema));
        let mut merge = table.merge_insert(&["id"]);
        merge.when_matched_update_all(None).when_not_matched_insert_all();
        merge.execute(batches).await.map_err(Self::map_err)?;

        debug!(collection, count = chunks.len(), "upserted chunks to lancedb");
        Ok(())
    }

    async fn delete(&self, collection: &str, ids: &[&str]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }

        let table = self
            .connection
            .open_table(Self::table_name(collection)?)
            .execute()
            .await
            .map_err(Self::map_err)?;
        table.delete(&Self::id_predicate(ids)).await.map_err(Self::map_err)?;

        debug!(collection, count = ids.len(), "deleted chunks from lancedb");
        Ok(())
    }

    async fn search(
        &self,
        collection: &str,
        embedding: &[f32],
        top_k: usize,
    ) -> Result<Vec<SearchResult>> {
        let table = self
            .connection
            .open_table(Self::table_name(collection)?)
            .execute()
            .await
            .map_err(Self::map_err)?;

        let stream = table
            .query()
            .nearest_to(embedding)
            .map_err(|e| RagError::VectorStoreError {
                backend: "lancedb".to_string(),
                message: format!("failed to build vector query: {e}"),
            })?
            .limit(top_k)
            .execute()
            .await
            .map_err(Self::map_err)?;

        let batches: Vec<RecordBatch> =
            stream.try_collect().await.map_err(|e| RagError::VectorStoreError {
                backend: "lancedb".to_string(),
                message: format!("failed to collect search results: {e}"),
            })?;

        let mut results = Vec::new();
        for batch in &batches {
            let id_col = batch.column_by_name("id");
            let text_col = batch.column_by_name("text");
            let doc_id_col = batch.column_by_name("document_id");
            let metadata_col = batch.column_by_name("metadata_json");
            let source_uri_col = batch.column_by_name("source_uri");
            let title_col = batch.column_by_name("title");
            let distance_col = batch.column_by_name("_distance");
            let optional = |column: Option<&Arc<dyn Array>>, i: usize| {
                column
                    .and_then(|c| c.as_string_opt::<i32>())
                    .filter(|a: &&StringArray| a.is_valid(i))
                    .map(|a| a.value(i).to_string())
            };

            for i in 0..batch.num_rows() {
                let id = id_col
                    .and_then(|c| c.as_string_opt::<i32>())
                    .map(|a: &StringArray| a.value(i).to_string())
                    .unwrap_or_default();

                let text = text_col
                    .and_then(|c| c.as_string_opt::<i32>())
                    .map(|a: &StringArray| a.value(i).to_string())
                    .unwrap_or_default();

                let document_id = doc_id_col
                    .and_then(|c| c.as_string_opt::<i32>())
                    .map(|a: &StringArray| a.value(i).to_string())
                    .unwrap_or_default();

                let metadata: HashMap<String, String> = metadata_col
                    .and_then(|c| c.as_string_opt::<i32>())
                    .and_then(|a: &StringArray| serde_json::from_str(a.value(i)).ok())
                    .unwrap_or_default();

                let distance = distance_col
                    .and_then(|c| c.as_primitive_opt::<Float32Type>())
                    .map(|a| a.value(i))
                    .unwrap_or(0.0);
                let score = 1.0 - distance;

                let mut chunk = Chunk::new(id, document_id, text);
                chunk.metadata = metadata;
                chunk.source_uri = optional(source_uri_col, i);
                chunk.title = optional(title_col, i);
                results.push(SearchResult { chunk, score });
            }
        }

        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lancedb::expr::expr_to_sql_string;

    #[test]
    fn id_predicate_keeps_quotes_inside_the_literal() {
        let injection = "x') OR ('1'='1";
        assert_eq!(
            LanceDBVectorStore::id_predicate(&["a", injection]),
            col("id").in_list(vec![lit("a"), lit(injection)], false)
        );

        let sql = expr_to_sql_string(&LanceDBVectorStore::id_predicate(&[injection]))
            .expect("render predicate");
        assert!(sql.contains("'x'') OR (''1''=''1'"), "quote not escaped: {sql}");
    }
}
