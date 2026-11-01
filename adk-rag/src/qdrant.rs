//! Qdrant vector store backend.
//!
//! Provides [`QdrantVectorStore`] which implements [`VectorStore`] using
//! the [qdrant-client](https://docs.rs/qdrant-client) crate over gRPC.
//!
//! Qdrant accepts only unsigned integers and UUIDs as point IDs, so each chunk is
//! stored under the UUID v5 of its scope and chunk ID, and the chunk ID itself is kept in
//! the point's `chunk_id` payload field, which search results read back. Upsert and delete
//! derive the same point ID, so any chunk ID — `doc_0` from the built-in chunkers
//! included — round-trips unchanged, and two scopes never share a point.
//!
//! The scope is kept in the `scope` payload field, which `create_collection` indexes as
//! Qdrant's tenant field. Search, count, and purge filter on it inside Qdrant, so the scope
//! applies before the nearest points are chosen.
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_rag::qdrant::QdrantVectorStore;
//! use adk_rag::{CollectionSpec, Scope, VectorQuery, VectorStore};
//!
//! let store = QdrantVectorStore::new("http://localhost:6334")?;
//! let scope = Scope::single_tenant();
//! store.create_collection("docs", &CollectionSpec::new(384)).await?;
//! store.upsert("docs", &scope, &chunks).await?;
//! let results = store.search("docs", &scope, &VectorQuery::new(&query_embedding, 5)).await?;
//! ```

use std::collections::HashMap;

use async_trait::async_trait;
use qdrant_client::qdrant::point_id::PointIdOptions;
use qdrant_client::qdrant::value::Kind;
use qdrant_client::qdrant::{
    Condition, CountPointsBuilder, CreateCollectionBuilder, CreateFieldIndexCollectionBuilder,
    DeletePointsBuilder, Distance, FieldType, Filter, KeywordIndexParamsBuilder, PointId,
    PointStruct, PointsIdsList, SearchPointsBuilder, UpsertPointsBuilder, Value as QdrantValue,
    VectorParamsBuilder,
};
use qdrant_client::{Payload, Qdrant};
use tracing::debug;
use uuid::Uuid;

use crate::document::{Chunk, SearchResult};
use crate::error::{RagError, Result};
use crate::scope::Scope;
use crate::vectorstore::{CollectionSpec, VectorQuery, VectorStore};

/// Namespace of the UUID v5 point IDs: the v5 UUID of
/// `https://github.com/zavora-ai/adk-rust/adk-rag/qdrant/chunk-id` in the URL namespace.
/// Changing it orphans every stored point.
const CHUNK_ID_NAMESPACE: Uuid = Uuid::from_u128(0xd821_7313_3204_50e2_a8d8_df0d_2973_2b60);

/// Payload field holding the chunk ID a point ID was derived from.
const CHUNK_ID_FIELD: &str = "chunk_id";

/// Payload field holding the point's scope, indexed as the collection's tenant field.
const SCOPE_FIELD: &str = "scope";

/// Maps a scope and chunk ID to the point ID Qdrant stores the chunk under.
fn point_id(scope: &Scope, chunk_id: &str) -> PointId {
    Uuid::new_v5(&CHUNK_ID_NAMESPACE, scope.record_key(chunk_id).as_bytes()).to_string().into()
}

/// Selects a scope's points.
fn scope_filter(scope: &Scope) -> Filter {
    Filter::must([Condition::matches(SCOPE_FIELD, scope.as_str().to_string())])
}

/// A [`VectorStore`] backed by [Qdrant](https://qdrant.tech/).
///
/// Wraps a [`qdrant_client::Qdrant`] client and maps collections to Qdrant
/// collections with cosine distance. Chunk text, document ID, metadata, source URI,
/// title, and scope are stored as Qdrant payload.
/// Points are keyed by the UUID v5 of the scope and chunk ID; the payload keeps both.
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
    async fn create_collection(&self, name: &str, spec: &CollectionSpec) -> Result<()> {
        let dimensions = spec.dimensions;
        let collections = self.client.list_collections().await.map_err(Self::map_err)?;
        let exists = collections.collections.iter().any(|c| c.name == name);
        if exists {
            debug!(collection = name, "qdrant collection already exists, skipping creation");
        } else {
            self.client
                .create_collection(
                    CreateCollectionBuilder::new(name).vectors_config(VectorParamsBuilder::new(
                        dimensions as u64,
                        Distance::Cosine,
                    )),
                )
                .await
                .map_err(Self::map_err)?;
            debug!(collection = name, dimensions, "created qdrant collection");
        }

        // Idempotent, so a collection created by an earlier release gains the index too.
        self.client
            .create_field_index(
                CreateFieldIndexCollectionBuilder::new(name, SCOPE_FIELD, FieldType::Keyword)
                    .field_index_params(KeywordIndexParamsBuilder::default().is_tenant(true))
                    .wait(true),
            )
            .await
            .map_err(Self::map_err)?;
        Ok(())
    }

    async fn delete_collection(&self, name: &str) -> Result<()> {
        self.client.delete_collection(name).await.map_err(Self::map_err)?;
        debug!(collection = name, "deleted qdrant collection");
        Ok(())
    }

    async fn upsert(&self, collection: &str, scope: &Scope, chunks: &[Chunk]) -> Result<()> {
        if chunks.is_empty() {
            return Ok(());
        }

        let points: Vec<PointStruct> = chunks
            .iter()
            .map(|chunk| {
                let mut payload_map = serde_json::Map::new();
                payload_map.insert(
                    SCOPE_FIELD.to_string(),
                    serde_json::Value::String(scope.as_str().to_string()),
                );
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
                for (field, value) in [("source_uri", &chunk.source_uri), ("title", &chunk.title)] {
                    if let Some(value) = value {
                        payload_map
                            .insert(field.to_string(), serde_json::Value::String(value.clone()));
                    }
                }
                let metadata_obj: serde_json::Map<String, serde_json::Value> = chunk
                    .metadata
                    .iter()
                    .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                    .collect();
                payload_map.insert("metadata".to_string(), serde_json::Value::Object(metadata_obj));

                let payload =
                    Payload::try_from(serde_json::Value::Object(payload_map)).unwrap_or_default();

                PointStruct::new(point_id(scope, &chunk.id), chunk.embedding.clone(), payload)
            })
            .collect();

        self.client
            .upsert_points(UpsertPointsBuilder::new(collection, points).wait(true))
            .await
            .map_err(Self::map_err)?;

        debug!(collection, count = chunks.len(), "upserted chunks to qdrant");
        Ok(())
    }

    async fn delete(&self, collection: &str, scope: &Scope, ids: &[&str]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }

        let point_ids: Vec<PointId> = ids.iter().map(|id| point_id(scope, id)).collect();

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
        scope: &Scope,
        query: &VectorQuery<'_>,
    ) -> Result<Vec<SearchResult>> {
        let response = self
            .client
            .search_points(
                SearchPointsBuilder::new(collection, query.embedding.to_vec(), query.top_k as u64)
                    .filter(scope_filter(scope))
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

                let mut chunk = Chunk::new(id, document_id, text);
                chunk.metadata = metadata;
                chunk.source_uri = scored.payload.get("source_uri").and_then(Self::extract_string);
                chunk.title = scored.payload.get("title").and_then(Self::extract_string);
                SearchResult { chunk, score: scored.score }
            })
            .collect();

        Ok(results)
    }

    async fn count(&self, collection: &str, scope: &Scope) -> Result<u64> {
        let response = self
            .client
            .count(CountPointsBuilder::new(collection).filter(scope_filter(scope)).exact(true))
            .await
            .map_err(Self::map_err)?;
        Ok(response.result.map_or(0, |result| result.count))
    }

    async fn purge_scope(&self, collection: &str, scope: &Scope) -> Result<u64> {
        // Qdrant's delete reports no count, so the count is taken first.
        let count = self.count(collection, scope).await?;
        self.client
            .delete_points(
                DeletePointsBuilder::new(collection).points(scope_filter(scope)).wait(true),
            )
            .await
            .map_err(Self::map_err)?;
        debug!(collection, purged = count, "purged scope from qdrant");
        Ok(count)
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
    fn point_ids_are_stable_uuid_v5_values_of_the_scope_and_chunk_id() {
        let scope = Scope::single_tenant();
        // Stored points are addressed by these values; a change orphans them.
        assert_eq!(
            [uuid_of(point_id(&scope, "doc_0")), uuid_of(point_id(&scope, "alpha")),],
            [
                Some(Uuid::new_v5(&CHUNK_ID_NAMESPACE, b"14::single-tenantdoc_0").to_string()),
                Some(Uuid::new_v5(&CHUNK_ID_NAMESPACE, b"14::single-tenantalpha").to_string()),
            ]
        );
    }

    #[test]
    fn distinct_scope_and_chunk_id_pairs_get_distinct_point_ids() {
        let uuid_shaped = "550e8400-e29b-41d4-a716-446655440000";
        let ids = ["doc_0", "doc_1", "42", uuid_shaped, "550E8400-E29B-41D4-A716-446655440000"];
        let scopes = [Scope::single_tenant(), Scope::new("a").unwrap(), Scope::new("a1").unwrap()];
        let points: std::collections::BTreeSet<String> = scopes
            .iter()
            .flat_map(|scope| ids.iter().filter_map(move |id| uuid_of(point_id(scope, id))))
            .collect();
        assert_eq!(points.len(), ids.len() * scopes.len());
        assert!(!points.contains(uuid_shaped), "a UUID-shaped chunk ID is mapped like any other");
    }
}
