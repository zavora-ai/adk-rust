//! Qdrant vector store backend.
//!
//! Provides [`QdrantVectorStore`] which implements [`VectorStore`] using
//! the [qdrant-client](https://docs.rs/qdrant-client) crate over gRPC.
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_rag::qdrant::QdrantVectorStore;
//!
//! let store = QdrantVectorStore::new("http://localhost:6334")?;
//! store.create_collection("docs", 384).await?;
//! store.upsert("docs", &chunks).await?;
//! let results = store.search("docs", &query_embedding, 5).await?;
//! ```

use std::collections::HashMap;

use async_trait::async_trait;
use qdrant_client::qdrant::point_id::PointIdOptions;
use qdrant_client::qdrant::value::Kind;
use qdrant_client::qdrant::{
    CreateCollectionBuilder, DeletePointsBuilder, Distance, PointStruct, PointsIdsList,
    SearchPointsBuilder, UpsertPointsBuilder, Value as QdrantValue, VectorParamsBuilder,
};
use qdrant_client::{Payload, Qdrant};
use tracing::debug;

use crate::document::{Chunk, SearchResult};
use crate::error::{RagError, Result};
use crate::vectorstore::VectorStore;

/// A [`VectorStore`] backed by [Qdrant](https://qdrant.tech/).
///
/// Wraps a [`qdrant_client::Qdrant`] client and maps collections to Qdrant
/// collections with cosine distance. Chunk metadata is stored as Qdrant payload.
pub struct QdrantVectorStore {
    client: Qdrant,
}

impl QdrantVectorStore {
    /// Create a new Qdrant vector store connecting to the given URL.
    pub fn new(url: &str) -> Result<Self> {
        let client = Qdrant::from_url(url).build().map_err(Self::map_err)?;
        Ok(Self { client })
    }

    /// Create a new Qdrant vector store with default URL (`http://localhost:6334`).
    pub fn default_url() -> Result<Self> {
        Self::new("http://localhost:6334")
    }

    /// Create a new Qdrant vector store from an existing client.
    pub fn from_client(client: Qdrant) -> Self {
        Self { client }
    }

    fn map_err(e: qdrant_client::QdrantError) -> RagError {
        RagError::VectorStoreError { backend: "qdrant".to_string(), message: e.to_string() }
    }

    /// Extract a string from a Qdrant payload value.
    fn extract_string(value: &QdrantValue) -> Option<String> {
        match &value.kind {
            Some(Kind::StringValue(s)) => Some(s.clone()),
            _ => None,
        }
    }
}

#[async_trait]
impl VectorStore for QdrantVectorStore {
    async fn create_collection(&self, name: &str, dimensions: usize) -> Result<()> {
        let collections = self.client.list_collections().await.map_err(Self::map_err)?;
        let exists = collections.collections.iter().any(|c| c.name == name);
        if exists {
            debug!(collection = name, "qdrant collection already exists, skipping creation");
            return Ok(());
        }

        self.client
            .create_collection(
                CreateCollectionBuilder::new(name)
                    .vectors_config(VectorParamsBuilder::new(dimensions as u64, Distance::Cosine)),
            )
            .await
            .map_err(Self::map_err)?;

        debug!(collection = name, dimensions, "created qdrant collection");
        Ok(())
    }

    async fn delete_collection(&self, name: &str) -> Result<()> {
        self.client.delete_collection(name).await.map_err(Self::map_err)?;
        debug!(collection = name, "deleted qdrant collection");
        Ok(())
    }

    async fn upsert(&self, collection: &str, chunks: &[Chunk]) -> Result<()> {
        if chunks.is_empty() {
            return Ok(());
        }

        let points: Vec<PointStruct> = chunks
            .iter()
            .map(|chunk| {
                let mut payload_map = serde_json::Map::new();
                payload_map
                    .insert("text".to_string(), serde_json::Value::String(chunk.text.clone()));
                payload_map.insert(
                    "document_id".to_string(),
                    serde_json::Value::String(chunk.document_id.clone()),
                );
                let metadata_obj: serde_json::Map<String, serde_json::Value> = chunk
                    .metadata
                    .iter()
                    .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                    .collect();
                payload_map.insert("metadata".to_string(), serde_json::Value::Object(metadata_obj));

                let payload =
                    Payload::try_from(serde_json::Value::Object(payload_map)).unwrap_or_default();

                PointStruct::new(chunk.id.clone(), chunk.embedding.clone(), payload)
            })
            .collect();

        self.client
            .upsert_points(UpsertPointsBuilder::new(collection, points).wait(true))
            .await
            .map_err(Self::map_err)?;

        debug!(collection, count = chunks.len(), "upserted chunks to qdrant");
        Ok(())
    }

    async fn delete(&self, collection: &str, ids: &[&str]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }

        let point_ids: Vec<qdrant_client::qdrant::PointId> =
            ids.iter().map(|&id| qdrant_client::qdrant::PointId::from(id)).collect();

        self.client
            .delete_points(
                DeletePointsBuilder::new(collection)
                    .points(PointsIdsList { ids: point_ids })
                    .wait(true),
            )
            .await
            .map_err(Self::map_err)?;

        debug!(collection, count = ids.len(), "deleted points from qdrant");
        Ok(())
    }

    async fn search(
        &self,
        collection: &str,
        embedding: &[f32],
        top_k: usize,
    ) -> Result<Vec<SearchResult>> {
        let response = self
            .client
            .search_points(
                SearchPointsBuilder::new(collection, embedding.to_vec(), top_k as u64)
                    .with_payload(true),
            )
            .await
            .map_err(Self::map_err)?;

        let results = response
            .result
            .into_iter()
            .map(|scored| {
                let id = scored
                    .id
                    .as_ref()
                    .and_then(|pid| match &pid.point_id_options {
                        Some(PointIdOptions::Uuid(s)) => Some(s.clone()),
                        Some(PointIdOptions::Num(n)) => Some(n.to_string()),
                        None => None,
                    })
                    .unwrap_or_default();

                let text =
                    scored.payload.get("text").and_then(Self::extract_string).unwrap_or_default();

                let document_id = scored
                    .payload
                    .get("document_id")
                    .and_then(Self::extract_string)
                    .unwrap_or_default();

                let metadata: HashMap<String, String> = scored
                    .payload
                    .get("metadata")
                    .and_then(|v| match &v.kind {
                        Some(Kind::StructValue(s)) => Some(
                            s.fields
                                .iter()
                                .filter_map(|(k, v)| {
                                    Self::extract_string(v).map(|s| (k.clone(), s))
                                })
                                .collect(),
                        ),
                        _ => None,
                    })
                    .unwrap_or_default();

                SearchResult {
                    chunk: Chunk { id, text, embedding: vec![], metadata, document_id },
                    score: scored.score,
                }
            })
            .collect();

        Ok(results)
    }
}
