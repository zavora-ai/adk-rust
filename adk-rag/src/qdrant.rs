//! Qdrant vector store backend.
//!
//! Provides [`QdrantVectorStore`] which implements [`VectorStore`] using
//! the [qdrant-client](https://docs.rs/qdrant-client) crate over gRPC.
//!
//! Qdrant accepts only unsigned integers and UUIDs as point IDs, so each chunk is
//! stored under the UUID v5 of its chunk ID, and the chunk ID itself is kept in the
//! point's `chunk_id` payload field, which search results read back. Upsert and delete
//! derive the same point ID, so any chunk ID — `doc_0` from the built-in chunkers
//! included — round-trips unchanged.
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
    CreateCollectionBuilder, DeletePointsBuilder, Distance, PointId, PointStruct, PointsIdsList,
    SearchPointsBuilder, UpsertPointsBuilder, Value as QdrantValue, VectorParamsBuilder,
};
use qdrant_client::{Payload, Qdrant};
use tracing::debug;
use uuid::Uuid;

use crate::document::{Chunk, SearchResult};
use crate::error::{RagError, Result};
use crate::vectorstore::VectorStore;

/// Namespace of the UUID v5 point IDs: the v5 UUID of
/// `https://github.com/zavora-ai/adk-rust/adk-rag/qdrant/chunk-id` in the URL namespace.
/// Changing it orphans every stored point.
const CHUNK_ID_NAMESPACE: Uuid = Uuid::from_u128(0xd821_7313_3204_50e2_a8d8_df0d_2973_2b60);

/// Payload field holding the chunk ID a point ID was derived from.
const CHUNK_ID_FIELD: &str = "chunk_id";

/// Maps a chunk ID to the point ID Qdrant stores it under.
fn point_id(chunk_id: &str) -> PointId {
    Uuid::new_v5(&CHUNK_ID_NAMESPACE, chunk_id.as_bytes()).to_string().into()
}

/// A [`VectorStore`] backed by [Qdrant](https://qdrant.tech/).
///
/// Wraps a [`qdrant_client::Qdrant`] client and maps collections to Qdrant
/// collections with cosine distance. Chunk metadata is stored as Qdrant payload.
/// Points are keyed by the UUID v5 of the chunk ID, which the payload keeps.
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
                payload_map.insert(
                    CHUNK_ID_FIELD.to_string(),
                    serde_json::Value::String(chunk.id.clone()),
                );
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

                PointStruct::new(point_id(&chunk.id), chunk.embedding.clone(), payload)
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

        let point_ids: Vec<PointId> = ids.iter().copied().map(point_id).collect();

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
                // A point without the field predates derived point IDs; its point ID was the
                // chunk ID.
                let id = scored
                    .payload
                    .get(CHUNK_ID_FIELD)
                    .and_then(Self::extract_string)
                    .or_else(|| {
                        scored.id.as_ref().and_then(|pid| match &pid.point_id_options {
                            Some(PointIdOptions::Uuid(s)) => Some(s.clone()),
                            Some(PointIdOptions::Num(n)) => Some(n.to_string()),
                            None => None,
                        })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn uuid_of(point: PointId) -> Option<String> {
        match point.point_id_options {
            Some(PointIdOptions::Uuid(uuid)) => Some(uuid),
            Some(PointIdOptions::Num(_)) | None => None,
        }
    }

    #[test]
    fn the_namespace_is_the_documented_v5_uuid() {
        assert_eq!(
            CHUNK_ID_NAMESPACE,
            Uuid::new_v5(
                &Uuid::NAMESPACE_URL,
                b"https://github.com/zavora-ai/adk-rust/adk-rag/qdrant/chunk-id"
            )
        );
    }

    #[test]
    fn point_ids_are_stable_uuid_v5_values_of_the_chunk_id() {
        // Stored points are addressed by these values; a change orphans them.
        assert_eq!(
            [uuid_of(point_id("doc_0")), uuid_of(point_id("alpha"))],
            [
                Some("fdf4af33-6752-51d2-9876-6584e0ce6507".to_string()),
                Some("f45e5236-380c-5f1e-939c-58d77904a26e".to_string()),
            ]
        );
    }

    #[test]
    fn distinct_chunk_ids_get_distinct_point_ids_even_when_uuid_shaped() {
        let uuid_shaped = "550e8400-e29b-41d4-a716-446655440000";
        let ids = ["doc_0", "doc_1", "42", uuid_shaped, "550E8400-E29B-41D4-A716-446655440000"];
        let points: std::collections::BTreeSet<String> =
            ids.iter().filter_map(|id| uuid_of(point_id(id))).collect();
        assert_eq!(points.len(), ids.len());
        assert!(!points.contains(uuid_shaped), "a UUID-shaped chunk ID is mapped like any other");
    }
}
