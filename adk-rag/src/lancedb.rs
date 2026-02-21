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

use arrow_array::cast::AsArray;
use arrow_array::types::Float32Type;
use arrow_array::{
    Array, FixedSizeListArray, Float32Array, RecordBatch, RecordBatchIterator, StringArray,
};
use arrow_schema::{DataType, Field, Schema};
use async_trait::async_trait;
use futures::TryStreamExt;
use lancedb::Connection;
use lancedb::query::{ExecutableQuery, QueryBase};
use tracing::debug;

use crate::document::{Chunk, SearchResult};
use crate::error::{RagError, Result};
use crate::vectorstore::VectorStore;

/// A [`VectorStore`] backed by [LanceDB](https://lancedb.github.io/lancedb/).
///
/// Wraps a [`lancedb::Connection`] and maps collections to LanceDB tables.
/// Runs in embedded mode by default using a local directory for storage.
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
        ]))
    }
}

#[async_trait]
impl VectorStore for LanceDBVectorStore {
    async fn create_collection(&self, name: &str, dimensions: usize) -> Result<()> {
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
        self.connection.drop_table(name, &[]).await.map_err(Self::map_err)?;
        debug!(collection = name, "deleted lancedb table");
        Ok(())
    }

    async fn upsert(&self, collection: &str, chunks: &[Chunk]) -> Result<()> {
        if chunks.is_empty() {
            return Ok(());
        }

        let dimensions = chunks[0].embedding.len();
        let schema = Self::build_schema(dimensions);

        let ids: Vec<&str> = chunks.iter().map(|c| c.id.as_str()).collect();
        let texts: Vec<&str> = chunks.iter().map(|c| c.text.as_str()).collect();
        let doc_ids: Vec<&str> = chunks.iter().map(|c| c.document_id.as_str()).collect();
        let metadata_jsons: Vec<String> = chunks
            .iter()
            .map(|c| serde_json::to_string(&c.metadata).unwrap_or_else(|_| "{}".to_string()))
            .collect();
        let metadata_refs: Vec<&str> = metadata_jsons.iter().map(|s| s.as_str()).collect();

        let all_values: Vec<f32> = chunks.iter().flat_map(|c| c.embedding.clone()).collect();
        let values_array = Arc::new(Float32Array::from(all_values));
        let list_field = Arc::new(Field::new("item", DataType::Float32, true));
        let vector_array =
            FixedSizeListArray::new(list_field, dimensions as i32, values_array, None);

        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(StringArray::from(ids)),
                Arc::new(StringArray::from(texts)),
                Arc::new(vector_array) as Arc<dyn Array>,
                Arc::new(StringArray::from(doc_ids)),
                Arc::new(StringArray::from(metadata_refs)),
            ],
        )
        .map_err(|e| RagError::VectorStoreError {
            backend: "lancedb".to_string(),
            message: format!("failed to build record batch: {e}"),
        })?;

        let table =
            self.connection.open_table(collection).execute().await.map_err(Self::map_err)?;
        let batches = RecordBatchIterator::new(vec![Ok(batch)], schema);
        table.add(Box::new(batches)).execute().await.map_err(Self::map_err)?;

        debug!(collection, count = chunks.len(), "upserted chunks to lancedb");
        Ok(())
    }

    async fn delete(&self, collection: &str, ids: &[&str]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }

        let table =
            self.connection.open_table(collection).execute().await.map_err(Self::map_err)?;
        let id_list: Vec<String> = ids.iter().map(|id| format!("'{id}'")).collect();
        let predicate = format!("id IN ({})", id_list.join(", "));
        table.delete(&predicate).await.map_err(Self::map_err)?;

        debug!(collection, count = ids.len(), "deleted chunks from lancedb");
        Ok(())
    }

    async fn search(
        &self,
        collection: &str,
        embedding: &[f32],
        top_k: usize,
    ) -> Result<Vec<SearchResult>> {
        let table =
            self.connection.open_table(collection).execute().await.map_err(Self::map_err)?;

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
            let distance_col = batch.column_by_name("_distance");

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

                results.push(SearchResult {
                    chunk: Chunk { id, text, embedding: vec![], metadata, document_id },
                    score,
                });
            }
        }

        Ok(results)
    }
}
