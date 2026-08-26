//! Agent Retrieval (formerly Vector Search 2.0) [`VectorStore`] backend.
//!
//! Agent Retrieval is Google's managed vector database on the Gemini
//! Enterprise Agent Platform, served by `vectorsearch.googleapis.com` (v1,
//! GA). This backend maps the [`VectorStore`] trait onto Collections and
//! Data Objects:
//!
//! | Trait operation | Agent Retrieval call |
//! |-----------------|----------------------|
//! | `create_collection` | `POST {parent}/collections` (LRO; `ALREADY_EXISTS` is a no-op) |
//! | `delete_collection` | `DELETE {collection}` (LRO; `NOT_FOUND` is a no-op) |
//! | `upsert` | `dataObjects:batchCreate`, falling back to per-object create-else-patch when IDs already exist |
//! | `delete` | `dataObjects:batchDelete`, falling back to per-object delete when IDs are missing |
//! | `search` | `dataObjects:search` with a `vectorSearch` query (`DOT_PRODUCT`) |
//!
//! Chunk text and metadata are stored **in** the Data Object alongside the
//! vector, so no companion store is needed. The store operates in BYOE
//! (bring-your-own-embeddings) mode — embeddings come from adk-rag's
//! embedder pipeline, and auto-embedding is deliberately not enabled, to
//! keep parity with the other backends. Scores are `DOT_PRODUCT` distances,
//! which equal cosine similarity for the normalized embeddings the pipeline
//! produces (higher is more relevant).
//!
//! **Vector Search 1.0 is deliberately not implemented.** Its
//! index/endpoint infrastructure model fits the [`VectorStore`] trait
//! poorly, and Google positions Agent Retrieval as its successor.
//!
//! Collection IDs must be RFC 1035 labels, so trait-level collection names
//! are sanitized deterministically (lowercased, invalid characters become
//! hyphens). Distinct names that sanitize identically collide; choose
//! names that differ in more than case or punctuation.
//!
//! Beyond the trait, [`AgentRetrievalStore::hybrid_search`] exposes
//! semantic + text search fused with reciprocal rank fusion for callers
//! holding the concrete type.

use crate::document::{Chunk, SearchResult};
use crate::error::{RagError, Result};
use crate::vectorstore::VectorStore;
use adk_core::ErrorComponent;
use adk_gcp::{GcpErrorCodes, GcpErrorContext, GcpHttpClient, LroPoller};
use async_trait::async_trait;
use reqwest::Method;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;

const DEFAULT_ENDPOINT: &str = "https://vectorsearch.googleapis.com";
const API_VERSION: &str = "v1";
const VECTOR_FIELD: &str = "embedding";
const BATCH_LIMIT: usize = 1000;
const ENV_GOOGLE_CLOUD_PROJECT: &str = "GOOGLE_CLOUD_PROJECT";
const ENV_GOOGLE_CLOUD_LOCATION: &str = "GOOGLE_CLOUD_LOCATION";

const GCP_ERROR_CODES: GcpErrorCodes = GcpErrorCodes {
    invalid_input: "rag.agent_retrieval.invalid_input",
    unauthorized: "rag.agent_retrieval.unauthorized",
    forbidden: "rag.agent_retrieval.forbidden",
    not_found: "rag.agent_retrieval.not_found",
    rate_limited: "rag.agent_retrieval.rate_limited",
    timeout: "rag.agent_retrieval.timeout",
    unavailable: "rag.agent_retrieval.unavailable",
    credentials_unavailable: "rag.agent_retrieval.credentials_unavailable",
    invalid_response: "rag.agent_retrieval.invalid_response",
    invalid_request: "rag.agent_retrieval.invalid_request",
    upstream_error: "rag.agent_retrieval.upstream_error",
    operation_failed: "rag.agent_retrieval.operation_failed",
};

fn gcp_error_context() -> GcpErrorContext {
    GcpErrorContext::new(ErrorComponent::Memory, GCP_ERROR_CODES, "agent retrieval")
}

/// Maps an `adk-gcp` error onto the crate's vector-store error surface.
fn store_error(error: adk_core::AdkError) -> RagError {
    RagError::VectorStoreError { backend: "agent_retrieval".to_string(), message: error.message }
}

/// Configuration for [`AgentRetrievalStore`].
///
/// # Example
///
/// ```rust,no_run
/// use adk_rag::agent_retrieval::AgentRetrievalConfig;
///
/// let config = AgentRetrievalConfig::new("my-project", "us-central1")
///     .with_collection_prefix("rag-")
///     .with_existing_collections_only(true);
/// # let _ = config;
/// ```
#[derive(Debug, Clone)]
pub struct AgentRetrievalConfig {
    project_id: String,
    location: String,
    collection_prefix: Option<String>,
    existing_collections_only: bool,
    endpoint: Option<String>,
}

impl AgentRetrievalConfig {
    /// Creates a config for the given project and location.
    pub fn new(project_id: impl Into<String>, location: impl Into<String>) -> Self {
        Self {
            project_id: project_id.into(),
            location: location.into(),
            collection_prefix: None,
            existing_collections_only: false,
            endpoint: None,
        }
    }

    /// Creates a config from `GOOGLE_CLOUD_PROJECT` and
    /// `GOOGLE_CLOUD_LOCATION`.
    ///
    /// # Errors
    ///
    /// Returns an error when either variable is unset or blank.
    pub fn from_env() -> Result<Self> {
        let read = |key: &str| {
            std::env::var(key).ok().filter(|value| !value.trim().is_empty()).ok_or_else(|| {
                RagError::ConfigError(format!(
                    "missing or blank environment variable {key}; set it or use AgentRetrievalConfig::new",
                ))
            })
        };
        Ok(Self::new(read(ENV_GOOGLE_CLOUD_PROJECT)?, read(ENV_GOOGLE_CLOUD_LOCATION)?))
    }

    /// Prefixes every sanitized collection ID, isolating this store's
    /// collections from others in the same project.
    #[must_use]
    pub fn with_collection_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.collection_prefix = Some(prefix.into());
        self
    }

    /// Turns `create_collection` into validate-and-fail-if-missing for
    /// deployments that pre-provision collections.
    #[must_use]
    pub fn with_existing_collections_only(mut self, existing_only: bool) -> Self {
        self.existing_collections_only = existing_only;
        self
    }

    /// Sets a custom API origin (loopback HTTP allowed for tests).
    #[must_use]
    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = Some(endpoint.into());
        self
    }

    fn endpoint(&self) -> String {
        self.endpoint.clone().unwrap_or_else(|| DEFAULT_ENDPOINT.to_string())
    }
}

/// [`VectorStore`] backend for Agent Retrieval.
///
/// # Example
///
/// ```rust,no_run
/// use adk_rag::VectorStore;
/// use adk_rag::agent_retrieval::{AgentRetrievalConfig, AgentRetrievalStore};
///
/// # async fn run() -> adk_rag::Result<()> {
/// let store = AgentRetrievalStore::new_with_adc(
///     AgentRetrievalConfig::new("my-project", "us-central1"),
/// )?;
/// store.create_collection("docs", 768).await?;
/// # Ok(())
/// # }
/// ```
pub struct AgentRetrievalStore {
    client: GcpHttpClient,
    poller: LroPoller,
    project_id: String,
    location: String,
    collection_prefix: Option<String>,
    existing_collections_only: bool,
}

impl AgentRetrievalStore {
    /// Creates a store using Application Default Credentials (ADC).
    ///
    /// # Errors
    ///
    /// Returns an error when ADC cannot be constructed or the endpoint is
    /// not a valid secure origin.
    pub fn new_with_adc(config: AgentRetrievalConfig) -> Result<Self> {
        let client = GcpHttpClient::builder(gcp_error_context(), config.endpoint())
            .api_version(API_VERSION)
            .build()
            .map_err(store_error)?;
        Ok(Self::with_client(config, client))
    }

    /// Creates a store with explicit credentials.
    ///
    /// # Errors
    ///
    /// Returns an error when the endpoint is not a valid secure origin.
    pub fn with_credentials(
        config: AgentRetrievalConfig,
        credentials: google_cloud_auth::credentials::Credentials,
    ) -> Result<Self> {
        let client = GcpHttpClient::builder(gcp_error_context(), config.endpoint())
            .api_version(API_VERSION)
            .credentials(credentials)
            .build()
            .map_err(store_error)?;
        Ok(Self::with_client(config, client))
    }

    fn with_client(config: AgentRetrievalConfig, client: GcpHttpClient) -> Self {
        Self {
            client,
            poller: LroPoller::new(),
            project_id: config.project_id,
            location: config.location,
            collection_prefix: config.collection_prefix,
            existing_collections_only: config.existing_collections_only,
        }
    }

    fn parent(&self) -> String {
        format!("projects/{}/locations/{}", self.project_id, self.location)
    }

    /// The full resource name for a trait-level collection name.
    fn collection_resource(&self, name: &str) -> String {
        format!("{}/collections/{}", self.parent(), self.collection_id(name))
    }

    /// Sanitizes a trait-level collection name into an RFC 1035 label.
    fn collection_id(&self, name: &str) -> String {
        let raw = format!("{}{name}", self.collection_prefix.as_deref().unwrap_or(""));
        sanitize_collection_id(&raw)
    }

    async fn wait_for_operation(&self, operation: Value, kind: &str) -> Result<Option<Value>> {
        self.poller
            .wait_for_operation(
                &self.client,
                operation,
                kind,
                false,
                &self.project_id,
                &self.location,
            )
            .await
            .map_err(store_error)
    }

    async fn get_collection(&self, resource: &str) -> Result<Option<Value>> {
        let request = self.client.request(Method::GET, resource).await.map_err(store_error)?;
        self.client.send_value_allow_not_found(request).await.map_err(store_error)
    }

    fn data_object_name(&self, collection: &str, id: &str) -> String {
        format!("{}/dataObjects/{id}", self.collection_resource(collection))
    }

    async fn create_objects_batch(&self, collection: &str, chunks: &[Chunk]) -> Result<()> {
        let parent = self.collection_resource(collection);
        let requests: Vec<Value> = chunks
            .iter()
            .map(|chunk| {
                json!({
                    "parent": parent,
                    "dataObjectId": chunk.id,
                    "dataObject": data_object_body(chunk),
                })
            })
            .collect();
        let request = self
            .client
            .request(Method::POST, &format!("{parent}/dataObjects:batchCreate"))
            .await
            .map_err(store_error)?
            .json(&json!({ "requests": requests }));
        self.client.send_value(request).await.map_err(store_error)?;
        Ok(())
    }

    /// Last-write-wins upsert for one object: create, then patch on
    /// `ALREADY_EXISTS`.
    async fn upsert_object(&self, collection: &str, chunk: &Chunk) -> Result<()> {
        let parent = self.collection_resource(collection);
        let request = self
            .client
            .request(Method::POST, &format!("{parent}/dataObjects"))
            .await
            .map_err(store_error)?
            .query(&[("dataObjectId", chunk.id.as_str())])
            .json(&data_object_body(chunk));
        match self.client.send_value(request).await {
            Ok(_) => Ok(()),
            Err(error) if error.details.upstream_status_code == Some(409) => {
                let name = self.data_object_name(collection, &chunk.id);
                let request = self
                    .client
                    .request(Method::PATCH, &name)
                    .await
                    .map_err(store_error)?
                    .json(&data_object_body(chunk));
                self.client.send_value(request).await.map_err(store_error)?;
                Ok(())
            }
            Err(error) => Err(store_error(error)),
        }
    }

    async fn delete_objects_batch(&self, collection: &str, ids: &[&str]) -> Result<()> {
        let parent = self.collection_resource(collection);
        let requests: Vec<Value> =
            ids.iter().map(|id| json!({ "name": self.data_object_name(collection, id) })).collect();
        let request = self
            .client
            .request(Method::POST, &format!("{parent}/dataObjects:batchDelete"))
            .await
            .map_err(store_error)?
            .json(&json!({ "requests": requests }));
        self.client.send_value(request).await.map_err(store_error)?;
        Ok(())
    }

    /// Semantic + text search fused with reciprocal rank fusion.
    ///
    /// A beyond-trait extra for callers holding the concrete type: runs a
    /// vector query and a keyword query over `text` in one `:batchSearch`
    /// call and returns the fused ranked list. `weights` are the RRF
    /// weights for the vector and text searches respectively.
    ///
    /// # Errors
    ///
    /// Returns an error when the request fails or the response cannot be
    /// parsed.
    pub async fn hybrid_search(
        &self,
        collection: &str,
        embedding: &[f32],
        query_text: &str,
        top_k: usize,
        weights: (f64, f64),
    ) -> Result<Vec<SearchResult>> {
        let parent = self.collection_resource(collection);
        let body = json!({
            "searches": [
                { "vectorSearch": {
                    "searchField": VECTOR_FIELD,
                    "vector": { "values": embedding },
                    "topK": top_k,
                    "outputFields": output_fields(),
                } },
                { "textSearch": {
                    "searchText": query_text,
                    "dataFieldNames": ["text"],
                    "topK": top_k,
                    "outputFields": output_fields(),
                } },
            ],
            "combine": {
                "ranker": { "rrf": { "weights": [weights.0, weights.1] } },
                "topK": top_k,
                "outputFields": output_fields(),
            },
        });
        let request = self
            .client
            .request(Method::POST, &format!("{parent}/dataObjects:batchSearch"))
            .await
            .map_err(store_error)?
            .json(&body);
        let value = self.client.send_value(request).await.map_err(store_error)?;
        let response: BatchSearchResponse =
            serde_json::from_value(value).map_err(|error| RagError::VectorStoreError {
                backend: "agent_retrieval".to_string(),
                message: format!("failed to parse batchSearch response: {error}"),
            })?;
        let fused = response.results.into_iter().next().unwrap_or_default();
        fused.results.into_iter().map(search_result_from_wire).collect()
    }
}

#[async_trait]
impl VectorStore for AgentRetrievalStore {
    async fn create_collection(&self, name: &str, dimensions: usize) -> Result<()> {
        let resource = self.collection_resource(name);
        if self.existing_collections_only {
            return match self.get_collection(&resource).await? {
                Some(_) => Ok(()),
                None => Err(RagError::ConfigError(format!(
                    "collection '{resource}' does not exist and existing_collections_only is set; provision it with platform tooling",
                ))),
            };
        }
        let body = json!({
            "displayName": name,
            "vectorSchema": {
                VECTOR_FIELD: { "denseVector": { "dimensions": dimensions } },
            },
        });
        let request = self
            .client
            .request(Method::POST, &format!("{}/collections", self.parent()))
            .await
            .map_err(store_error)?
            .query(&[("collectionId", self.collection_id(name).as_str())])
            .json(&body);
        // The trait requires idempotent creation; ALREADY_EXISTS preserves
        // the existing collection and its rows.
        let operation = match self.client.send_value(request).await {
            Ok(operation) => operation,
            Err(error) if error.details.upstream_status_code == Some(409) => return Ok(()),
            Err(error) => return Err(store_error(error)),
        };
        self.wait_for_operation(operation, "collection create").await?;
        Ok(())
    }

    async fn delete_collection(&self, name: &str) -> Result<()> {
        let resource = self.collection_resource(name);
        let request = self.client.request(Method::DELETE, &resource).await.map_err(store_error)?;
        // Idempotent: deleting a missing collection is a no-op.
        let Some(operation) =
            self.client.send_value_allow_not_found(request).await.map_err(store_error)?
        else {
            return Ok(());
        };
        self.wait_for_operation(operation, "collection delete").await?;
        Ok(())
    }

    async fn upsert(&self, collection: &str, chunks: &[Chunk]) -> Result<()> {
        for batch in chunks.chunks(BATCH_LIMIT) {
            // Fast path: one atomic batchCreate per 1000. Batches are
            // atomic, so any ALREADY_EXISTS fails the whole batch; fall
            // back to per-object create-else-patch for last-write-wins.
            match self.create_objects_batch(collection, batch).await {
                Ok(()) => {}
                Err(_) => {
                    for chunk in batch {
                        self.upsert_object(collection, chunk).await?;
                    }
                }
            }
        }
        Ok(())
    }

    async fn delete(&self, collection: &str, ids: &[&str]) -> Result<()> {
        for batch in ids.chunks(BATCH_LIMIT) {
            // Batches are atomic, so one missing ID fails the whole batch;
            // fall back to per-object deletes that ignore NOT_FOUND.
            if self.delete_objects_batch(collection, batch).await.is_err() {
                for id in batch {
                    let name = self.data_object_name(collection, id);
                    let request =
                        self.client.request(Method::DELETE, &name).await.map_err(store_error)?;
                    self.client.send_value_allow_not_found(request).await.map_err(store_error)?;
                }
            }
        }
        Ok(())
    }

    async fn search(
        &self,
        collection: &str,
        embedding: &[f32],
        top_k: usize,
    ) -> Result<Vec<SearchResult>> {
        let parent = self.collection_resource(collection);
        let body = json!({
            "vectorSearch": {
                "searchField": VECTOR_FIELD,
                "vector": { "values": embedding },
                "topK": top_k,
                "outputFields": output_fields(),
            },
        });
        let request = self
            .client
            .request(Method::POST, &format!("{parent}/dataObjects:search"))
            .await
            .map_err(store_error)?
            .json(&body);
        let value = self.client.send_value(request).await.map_err(store_error)?;
        let response: SearchResponse =
            serde_json::from_value(value).map_err(|error| RagError::VectorStoreError {
                backend: "agent_retrieval".to_string(),
                message: format!("failed to parse search response: {error}"),
            })?;
        response.results.into_iter().map(search_result_from_wire).collect()
    }
}

/// Sanitizes a name into an RFC 1035 label (lowercase letter first, then
/// lowercase letters, digits, and hyphens, at most 63 characters).
fn sanitize_collection_id(name: &str) -> String {
    let mut id: String = name
        .chars()
        .map(|c| match c {
            'a'..='z' | '0'..='9' | '-' => c,
            'A'..='Z' => c.to_ascii_lowercase(),
            _ => '-',
        })
        .collect();
    if !id.starts_with(|c: char| c.is_ascii_lowercase()) {
        id = format!("c-{id}");
    }
    id.truncate(63);
    while id.ends_with('-') {
        id.pop();
    }
    if id.is_empty() { "c".to_string() } else { id }
}

fn output_fields() -> Value {
    json!({
        "dataFields": ["text", "documentId", "metadata"],
        "vectorFields": [VECTOR_FIELD],
    })
}

fn data_object_body(chunk: &Chunk) -> Value {
    json!({
        "data": {
            "text": chunk.text,
            "documentId": chunk.document_id,
            "metadata": chunk.metadata,
        },
        "vectors": {
            VECTOR_FIELD: { "dense": { "values": chunk.embedding } },
        },
    })
}

#[derive(Debug, Default, Deserialize)]
struct SearchResponse {
    #[serde(default)]
    results: Vec<WireSearchResult>,
}

#[derive(Debug, Default, Deserialize)]
struct BatchSearchResponse {
    #[serde(default)]
    results: Vec<SearchResponse>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireSearchResult {
    #[serde(default)]
    data_object: WireDataObject,
    #[serde(default)]
    distance: f64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireDataObject {
    #[serde(default)]
    name: String,
    #[serde(default)]
    data_object_id: Option<String>,
    #[serde(default)]
    data: WireData,
    #[serde(default)]
    vectors: HashMap<String, WireVector>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireData {
    #[serde(default)]
    text: String,
    #[serde(default)]
    document_id: String,
    #[serde(default)]
    metadata: HashMap<String, String>,
}

#[derive(Debug, Default, Deserialize)]
struct WireVector {
    #[serde(default)]
    dense: Option<WireDenseVector>,
}

#[derive(Debug, Default, Deserialize)]
struct WireDenseVector {
    #[serde(default)]
    values: Vec<f32>,
}

fn search_result_from_wire(wire: WireSearchResult) -> Result<SearchResult> {
    let id = wire
        .data_object
        .data_object_id
        .clone()
        .or_else(|| wire.data_object.name.rsplit('/').next().map(str::to_string))
        .filter(|id| !id.is_empty())
        .ok_or_else(|| RagError::VectorStoreError {
            backend: "agent_retrieval".to_string(),
            message: "search result data object carries no ID".to_string(),
        })?;
    let embedding = wire
        .data_object
        .vectors
        .get(VECTOR_FIELD)
        .and_then(|vector| vector.dense.as_ref())
        .map(|dense| dense.values.clone())
        .unwrap_or_default();
    Ok(SearchResult {
        chunk: Chunk {
            id,
            text: wire.data_object.data.text,
            embedding,
            metadata: wire.data_object.data.metadata,
            document_id: wire.data_object.data.document_id,
        },
        score: wire.distance as f32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collection_ids_are_rfc1035_labels() {
        assert_eq!(sanitize_collection_id("contract_search"), "contract-search");
        assert_eq!(sanitize_collection_id("Docs V2"), "docs-v2");
        assert_eq!(sanitize_collection_id("9lives"), "c-9lives");
        assert_eq!(sanitize_collection_id("trailing-"), "trailing");
        assert_eq!(sanitize_collection_id(""), "c");
        let long = sanitize_collection_id(&"x".repeat(100));
        assert_eq!(long.len(), 63);
    }

    #[tokio::test]
    async fn prefix_applies_before_sanitization() {
        let store = AgentRetrievalStore::with_client(
            AgentRetrievalConfig::new("p", "l").with_collection_prefix("Rag_"),
            GcpHttpClient::builder(gcp_error_context(), "https://vectorsearch.googleapis.com")
                .api_version(API_VERSION)
                .credentials(
                    google_cloud_auth::credentials::api_key_credentials::Builder::new("k").build(),
                )
                .build()
                .expect("build client"),
        );
        assert_eq!(
            store.collection_resource("Docs"),
            "projects/p/locations/l/collections/rag-docs",
        );
    }

    #[test]
    fn data_object_body_stores_text_and_metadata_with_the_vector() {
        let chunk = Chunk {
            id: "c1".to_string(),
            text: "hello".to_string(),
            embedding: vec![0.1, 0.2],
            metadata: HashMap::from([("k".to_string(), "v".to_string())]),
            document_id: "d1".to_string(),
        };
        assert_eq!(
            data_object_body(&chunk),
            json!({
                "data": { "text": "hello", "documentId": "d1", "metadata": { "k": "v" } },
                "vectors": { "embedding": { "dense": { "values": [0.1f32, 0.2f32] } } },
            }),
        );
    }
}
