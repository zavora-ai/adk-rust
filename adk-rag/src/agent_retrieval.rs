//! Agent Retrieval (formerly Vector Search 2.0) [`VectorStore`] backend.
//!
//! Agent Retrieval is Google's managed vector database on the Gemini
//! Enterprise Agent Platform, served by `vectorsearch.googleapis.com` (v1,
//! GA). This backend maps the [`VectorStore`] trait onto Collections and
//! Data Objects:
//!
//! | Trait operation | Agent Retrieval call |
//! |-----------------|----------------------|
//! | `create_collection` | `POST {parent}/collections` with a data schema (LRO; `ALREADY_EXISTS` is a no-op) |
//! | `delete_collection` | `DELETE {collection}` (LRO; `NOT_FOUND` is a no-op) |
//! | `upsert` | `dataObjects:batchCreate`, falling back to per-object create-else-patch when IDs already exist |
//! | `delete` | `dataObjects:batchDelete`, falling back to per-object delete when IDs are missing |
//! | `search` | `dataObjects:search` with a `vectorSearch` query (`DOT_PRODUCT`) filtered by scope |
//! | `count` | `dataObjects:query` filtered by scope, paged |
//! | `purge_scope` | `dataObjects:query` filtered by scope, then `dataObjects:batchDelete` |
//!
//! Chunk text, metadata, source URI, title, scope, and chunk ID are stored **in** the
//! Data Object alongside the vector, so no companion store is needed. Agent Retrieval
//! validates every Data Object against the collection's data schema and allows no
//! undeclared field, so `create_collection` declares these fields and metadata is stored
//! as one JSON string, `metadataJson`.
//!
//! Data Object IDs must be RFC 1035 labels, which chunk IDs such as `doc_0` are not, so
//! each chunk is stored under `c` followed by the UUID v5 of its scope and chunk ID in
//! hexadecimal. The chunk ID itself is kept in the `chunkId` field, which search returns.
//! Search, count, and purge filter on the `scope` field inside the service, so the scope
//! applies before the nearest objects are chosen. A collection written by an earlier
//! release has no data schema; delete it, create it again, and re-ingest. The store operates in BYOE
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
//! Collection IDs must be RFC 1035 labels. Trait-level collection names use
//! ASCII lowercase letters, digits, and underscores, and each underscore
//! becomes a hyphen in the collection ID (`my_docs` is `my-docs`). Any other
//! name fails with [`RagError::VectorStoreError`] rather than being rewritten,
//! so two names never share a collection.
//!
//! Beyond the trait, [`AgentRetrievalStore::hybrid_search`] exposes
//! semantic + text search fused with reciprocal rank fusion for callers
//! holding the concrete type.

use crate::document::{Chunk, SearchResult};
use crate::error::{RagError, Result};
use crate::filter::{Field, Filter};
use crate::scope::Scope;
use crate::vectorstore::{CollectionSpec, VectorQuery, VectorStore};
use adk_core::ErrorComponent;
use adk_gcp::{GcpErrorCodes, GcpErrorContext, GcpHttpClient, LroPoller};
use async_trait::async_trait;
use reqwest::Method;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashMap};
use tokio::sync::RwLock;
use uuid::Uuid;

const DEFAULT_ENDPOINT: &str = "https://vectorsearch.googleapis.com";
const API_VERSION: &str = "v1";
const VECTOR_FIELD: &str = "embedding";
const BATCH_LIMIT: usize = 1000;
/// Prefix of the data field that holds a filterable metadata key.
const METADATA_FIELD_PREFIX: &str = "m_";
/// The deepest nesting of `$and` and `$or` Agent Retrieval accepts.
const MAX_FILTER_DEPTH: usize = 2;
/// Namespace of the derived Data Object IDs: the v5 UUID of
/// `https://github.com/zavora-ai/adk-rust/adk-rag/agent-retrieval/data-object-id` in the URL
/// namespace. Changing it orphans every stored Data Object.
const DATA_OBJECT_ID_NAMESPACE: Uuid = Uuid::from_u128(0xd55e_0355_dcf7_5a46_be9f_65f7_1b09_575f);
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

    /// Prefixes every collection ID, isolating this store's collections from
    /// others in the same project.
    ///
    /// The prefix uses ASCII lowercase letters, digits, and hyphens and starts
    /// with a letter, such as `rag-`; with any other prefix every operation
    /// fails with [`RagError::VectorStoreError`].
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
/// use adk_rag::agent_retrieval::{AgentRetrievalConfig, AgentRetrievalStore};
/// use adk_rag::{CollectionSpec, VectorStore};
///
/// # async fn run() -> adk_rag::Result<()> {
/// let store = AgentRetrievalStore::new_with_adc(
///     AgentRetrievalConfig::new("my-project", "us-central1"),
/// )?;
/// store.create_collection("docs", &CollectionSpec::new(768)).await?;
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
    /// Filterable metadata keys per collection name, read from the data schema once.
    filterable: RwLock<HashMap<String, BTreeSet<String>>>,
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
            filterable: RwLock::new(HashMap::new()),
        }
    }

    /// The filterable metadata keys of `collection`: the `m_` fields of its data schema.
    async fn filterable_keys(&self, collection: &str) -> Result<BTreeSet<String>> {
        if let Some(keys) = self.filterable.read().await.get(collection) {
            return Ok(keys.clone());
        }
        let resource = self.collection_resource(collection)?;
        let found =
            self.get_collection(&resource).await?.ok_or_else(|| RagError::VectorStoreError {
                backend: "agent_retrieval".to_string(),
                message: format!("collection '{resource}' does not exist"),
            })?;
        let keys: BTreeSet<String> = found["dataSchema"]["properties"]
            .as_object()
            .into_iter()
            .flat_map(|properties| properties.keys())
            .filter_map(|field| field.strip_prefix(METADATA_FIELD_PREFIX).map(str::to_string))
            .collect();
        self.filterable.write().await.insert(collection.to_string(), keys.clone());
        Ok(keys)
    }

    /// The scope's selector, joined with `filter`, as an Agent Retrieval filter expression.
    async fn filter_value(
        &self,
        collection: &str,
        scope: &Scope,
        filter: Option<&Filter>,
    ) -> Result<Value> {
        let Some(filter) = filter else {
            return Ok(scope_filter(scope));
        };
        filter.validate()?;
        let mut names_metadata = false;
        filter.visit_fields(&mut |field| names_metadata |= matches!(field, Field::Metadata(_)));
        let declared =
            if names_metadata { self.filterable_keys(collection).await? } else { BTreeSet::new() };
        let compiled = compile_filter(collection, filter, &declared)?;
        let mut clauses = vec![scope_filter(scope)];
        match compiled {
            Value::Object(mut object) if object.len() == 1 && object.contains_key("$and") => {
                if let Some(Value::Array(children)) = object.remove("$and") {
                    clauses.extend(children);
                }
            }
            other => clauses.push(other),
        }
        let joined = json!({ "$and": clauses });
        if filter_depth(&joined) > MAX_FILTER_DEPTH {
            return Err(RagError::UnsupportedFilter(format!(
                "Agent Retrieval nests `$and` and `$or` at most {MAX_FILTER_DEPTH} levels deep, \
                 counting the scope's `$and`"
            )));
        }
        Ok(joined)
    }

    fn parent(&self) -> String {
        format!("projects/{}/locations/{}", self.project_id, self.location)
    }

    /// The full resource name for a trait-level collection name.
    fn collection_resource(&self, name: &str) -> Result<String> {
        Ok(format!("{}/collections/{}", self.parent(), self.collection_id(name)?))
    }

    /// Maps a trait-level collection name to its collection ID, rejecting names
    /// that would need rewriting.
    ///
    /// Names use ASCII lowercase letters, digits, and underscores, IDs use
    /// lowercase letters, digits, and hyphens, and each underscore becomes a
    /// hyphen, so the mapping is one-to-one.
    fn collection_id(&self, name: &str) -> Result<String> {
        let invalid = |message: String| RagError::VectorStoreError {
            backend: "agent_retrieval".to_string(),
            message,
        };
        let prefix = self.collection_prefix.as_deref().unwrap_or("");
        if !prefix.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            || prefix.starts_with(|c: char| !c.is_ascii_lowercase())
        {
            return Err(invalid(format!(
                "invalid collection prefix `{prefix}`: use ASCII lowercase letters, digits, or \
                 hyphens, starting with a letter"
            )));
        }
        let id = format!("{prefix}{}", name.replace('_', "-"));
        if name.is_empty()
            || !name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            || id.len() > 63
            || !id.starts_with(|c: char| c.is_ascii_lowercase())
            || id.ends_with('-')
        {
            let max = 63_usize.saturating_sub(prefix.len());
            let first = if prefix.is_empty() { ", starting with a letter" } else { "" };
            return Err(invalid(format!(
                "invalid collection name `{name}`: use 1 to {max} ASCII lowercase letters, \
                 digits, or underscores{first}, ending with a letter or digit; each underscore \
                 becomes a hyphen in the Agent Retrieval collection ID"
            )));
        }
        Ok(id)
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

    fn data_object_name(&self, collection: &str, scope: &Scope, chunk_id: &str) -> Result<String> {
        Ok(format!(
            "{}/dataObjects/{}",
            self.collection_resource(collection)?,
            data_object_id(scope, chunk_id)
        ))
    }

    async fn create_objects_batch(
        &self,
        collection: &str,
        scope: &Scope,
        chunks: &[Chunk],
        filterable: &BTreeSet<String>,
    ) -> Result<()> {
        let parent = self.collection_resource(collection)?;
        let requests: Vec<Value> = chunks
            .iter()
            .map(|chunk| {
                json!({
                    "parent": parent,
                    "dataObjectId": data_object_id(scope, &chunk.id),
                    "dataObject": data_object_body(scope, chunk, filterable),
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
    async fn upsert_object(
        &self,
        collection: &str,
        scope: &Scope,
        chunk: &Chunk,
        filterable: &BTreeSet<String>,
    ) -> Result<()> {
        let parent = self.collection_resource(collection)?;
        let request = self
            .client
            .request(Method::POST, &format!("{parent}/dataObjects"))
            .await
            .map_err(store_error)?
            .query(&[("dataObjectId", data_object_id(scope, &chunk.id).as_str())])
            .json(&data_object_body(scope, chunk, filterable));
        match self.client.send_value(request).await {
            Ok(_) => Ok(()),
            Err(error) if error.details.upstream_status_code == Some(409) => {
                let name = self.data_object_name(collection, scope, &chunk.id)?;
                let request = self
                    .client
                    .request(Method::PATCH, &name)
                    .await
                    .map_err(store_error)?
                    .json(&data_object_body(scope, chunk, filterable));
                self.client.send_value(request).await.map_err(store_error)?;
                Ok(())
            }
            Err(error) => Err(store_error(error)),
        }
    }

    /// Deletes Data Objects by resource name in one atomic batch.
    async fn delete_objects_batch(&self, collection: &str, names: &[String]) -> Result<()> {
        let parent = self.collection_resource(collection)?;
        let requests: Vec<Value> = names.iter().map(|name| json!({ "name": name })).collect();
        let request = self
            .client
            .request(Method::POST, &format!("{parent}/dataObjects:batchDelete"))
            .await
            .map_err(store_error)?
            .json(&json!({ "requests": requests }));
        self.client.send_value(request).await.map_err(store_error)?;
        Ok(())
    }

    /// Deletes Data Objects by resource name, falling back to per-object deletes that
    /// ignore `NOT_FOUND` when an atomic batch fails.
    async fn delete_objects(&self, collection: &str, names: &[String]) -> Result<()> {
        for batch in names.chunks(BATCH_LIMIT) {
            if self.delete_objects_batch(collection, batch).await.is_err() {
                for name in batch {
                    let request =
                        self.client.request(Method::DELETE, name).await.map_err(store_error)?;
                    self.client.send_value_allow_not_found(request).await.map_err(store_error)?;
                }
            }
        }
        Ok(())
    }

    /// The resource names of every Data Object `filter` selects, through paged
    /// `dataObjects:query` calls.
    async fn object_names(&self, collection: &str, filter: &Value) -> Result<Vec<String>> {
        let parent = self.collection_resource(collection)?;
        let mut names = Vec::new();
        let mut page_token = String::new();
        loop {
            let mut body = json!({
                "filter": filter,
                "pageSize": BATCH_LIMIT,
                "outputFields": { "dataFields": ["chunkId"] },
            });
            if !page_token.is_empty() {
                body["pageToken"] = json!(page_token);
            }
            let request = self
                .client
                .request(Method::POST, &format!("{parent}/dataObjects:query"))
                .await
                .map_err(store_error)?
                .json(&body);
            let value = self.client.send_value(request).await.map_err(store_error)?;
            let page: QueryResponse =
                serde_json::from_value(value).map_err(|error| RagError::VectorStoreError {
                    backend: "agent_retrieval".to_string(),
                    message: format!("failed to parse query response: {error}"),
                })?;
            for object in page.data_objects {
                let name = if object.name.is_empty() {
                    let id = object.data_object_id.unwrap_or_default();
                    format!("{parent}/dataObjects/{id}")
                } else {
                    object.name
                };
                names.push(name);
            }
            if page.next_page_token.is_empty() {
                return Ok(names);
            }
            page_token = page.next_page_token;
        }
    }

    /// Semantic + text search fused with reciprocal rank fusion.
    ///
    /// A beyond-trait extra for callers holding the concrete type: runs a
    /// vector query and a keyword query over `text` in one `:batchSearch`
    /// call and returns the fused ranked list. Both searches are filtered by
    /// `scope` inside the service. `weights` are the RRF weights for the
    /// vector and text searches respectively.
    ///
    /// # Errors
    ///
    /// Returns an error when the request fails or the response cannot be
    /// parsed.
    pub async fn hybrid_search(
        &self,
        collection: &str,
        scope: &Scope,
        embedding: &[f32],
        query_text: &str,
        top_k: usize,
        weights: (f64, f64),
    ) -> Result<Vec<SearchResult>> {
        let parent = self.collection_resource(collection)?;
        let body = json!({
            "searches": [
                { "vectorSearch": {
                    "searchField": VECTOR_FIELD,
                    "vector": { "values": embedding },
                    "filter": scope_filter(scope),
                    "topK": top_k,
                    "outputFields": output_fields(),
                } },
                { "textSearch": {
                    "searchText": query_text,
                    "dataFieldNames": ["text"],
                    "filter": scope_filter(scope),
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
    async fn create_collection(&self, name: &str, spec: &CollectionSpec) -> Result<()> {
        let resource = self.collection_resource(name)?;
        spec.validate()?;
        if self.existing_collections_only {
            return match self.get_collection(&resource).await? {
                Some(_) => Ok(()),
                None => Err(RagError::ConfigError(format!(
                    "collection '{resource}' does not exist and existing_collections_only is set; provision it with platform tooling",
                ))),
            };
        }
        let string = json!({ "type": "string" });
        let mut properties = json!({
            "scope": string,
            "chunkId": string,
            "documentId": string,
            "text": string,
            "metadataJson": string,
            "sourceUri": string,
            "title": string,
        });
        for key in &spec.filterable {
            properties[format!("{METADATA_FIELD_PREFIX}{key}")] = string.clone();
        }
        let body = json!({
            "displayName": name,
            "dataSchema": { "type": "object", "properties": properties },
            "vectorSchema": {
                VECTOR_FIELD: { "denseVector": { "dimensions": spec.dimensions } },
            },
        });
        let request = self
            .client
            .request(Method::POST, &format!("{}/collections", self.parent()))
            .await
            .map_err(store_error)?
            .query(&[("collectionId", self.collection_id(name)?.as_str())])
            .json(&body);
        // The trait requires idempotent creation; ALREADY_EXISTS preserves
        // the existing collection and its rows.
        let operation = match self.client.send_value(request).await {
            Ok(operation) => operation,
            Err(error) if error.details.upstream_status_code == Some(409) => return Ok(()),
            Err(error) => return Err(store_error(error)),
        };
        self.wait_for_operation(operation, "collection create").await?;
        self.filterable.write().await.insert(name.to_string(), spec.filterable.clone());
        Ok(())
    }

    async fn delete_collection(&self, name: &str) -> Result<()> {
        let resource = self.collection_resource(name)?;
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

    async fn upsert(&self, collection: &str, scope: &Scope, chunks: &[Chunk]) -> Result<()> {
        if chunks.is_empty() {
            return Ok(());
        }
        let filterable = self.filterable_keys(collection).await?;
        for batch in chunks.chunks(BATCH_LIMIT) {
            // Fast path: one atomic batchCreate per 1000. Batches are
            // atomic, so any ALREADY_EXISTS fails the whole batch; fall
            // back to per-object create-else-patch for last-write-wins.
            match self.create_objects_batch(collection, scope, batch, &filterable).await {
                Ok(()) => {}
                Err(_) => {
                    for chunk in batch {
                        self.upsert_object(collection, scope, chunk, &filterable).await?;
                    }
                }
            }
        }
        Ok(())
    }

    async fn delete(&self, collection: &str, scope: &Scope, ids: &[&str]) -> Result<()> {
        let names = ids
            .iter()
            .map(|id| self.data_object_name(collection, scope, id))
            .collect::<Result<Vec<_>>>()?;
        self.delete_objects(collection, &names).await
    }

    async fn search(
        &self,
        collection: &str,
        scope: &Scope,
        query: &VectorQuery<'_>,
    ) -> Result<Vec<SearchResult>> {
        let parent = self.collection_resource(collection)?;
        let filter = self.filter_value(collection, scope, query.filter).await?;
        let body = json!({
            "vectorSearch": {
                "searchField": VECTOR_FIELD,
                "vector": { "values": query.embedding },
                "filter": filter,
                "topK": query.top_k,
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

    async fn count(&self, collection: &str, scope: &Scope, filter: Option<&Filter>) -> Result<u64> {
        let filter = self.filter_value(collection, scope, filter).await?;
        Ok(self.object_names(collection, &filter).await?.len() as u64)
    }

    async fn delete_where(&self, collection: &str, scope: &Scope, filter: &Filter) -> Result<u64> {
        let filter = self.filter_value(collection, scope, Some(filter)).await?;
        let names = self.object_names(collection, &filter).await?;
        self.delete_objects(collection, &names).await?;
        Ok(names.len() as u64)
    }

    async fn purge_scope(&self, collection: &str, scope: &Scope) -> Result<u64> {
        let names = self.object_names(collection, &scope_filter(scope)).await?;
        self.delete_objects(collection, &names).await?;
        Ok(names.len() as u64)
    }
}

/// The Data Object ID a scope's chunk is stored under: an RFC 1035 label whatever the chunk
/// ID's characters.
fn data_object_id(scope: &Scope, chunk_id: &str) -> String {
    let uuid = Uuid::new_v5(&DATA_OBJECT_ID_NAMESPACE, scope.record_key(chunk_id).as_bytes());
    format!("c{}", uuid.simple())
}

/// Selects a scope's Data Objects.
fn scope_filter(scope: &Scope) -> Value {
    json!({ "scope": { "$eq": scope.as_str() } })
}

/// The nesting depth of `$and` and `$or` in a filter expression.
fn filter_depth(value: &Value) -> usize {
    value.as_object().map_or(0, |object| {
        object
            .iter()
            .filter(|(key, _)| matches!(key.as_str(), "$and" | "$or"))
            .map(|(_, children)| {
                1 + children.as_array().into_iter().flatten().map(filter_depth).max().unwrap_or(0)
            })
            .max()
            .unwrap_or(0)
    })
}

/// Compiles a validated filter to Agent Retrieval's filter language.
///
/// The language has no `$exists` or `$not`, and its `$ne` and `$nin` are documented only for
/// fields a Data Object has, so `Exists` fails, and `Not` is accepted only around `Eq` or `In`
/// on the document ID or chunk ID, which every chunk has.
fn compile_filter(collection: &str, filter: &Filter, declared: &BTreeSet<String>) -> Result<Value> {
    let unsupported =
        |reason: &str| RagError::UnsupportedFilter(format!("Agent Retrieval: {reason}"));
    let data_field = |field: &Field| -> Result<String> {
        Ok(match field {
            Field::Metadata(key) if declared.contains(key) => {
                format!("{METADATA_FIELD_PREFIX}{key}")
            }
            Field::Metadata(key) => {
                return Err(RagError::UnsupportedFilter(format!(
                    "metadata key `{key}` is not declared filterable on collection \
                     '{collection}'; create the collection with it in \
                     `CollectionSpec::with_filterable`"
                )));
            }
            Field::DocumentId => "documentId".to_string(),
            Field::SourceUri => "sourceUri".to_string(),
            Field::ChunkId => "chunkId".to_string(),
        })
    };
    let children = |filters: &[Filter]| -> Result<Vec<Value>> {
        filters.iter().map(|filter| compile_filter(collection, filter, declared)).collect()
    };
    Ok(match filter {
        Filter::Eq(field, value) => json!({ data_field(field)?: { "$eq": value } }),
        Filter::In(field, values) => json!({ data_field(field)?: { "$in": values } }),
        Filter::Exists(_) => return Err(unsupported("the filter language has no `$exists`")),
        Filter::And(filters) => json!({ "$and": children(filters)? }),
        Filter::Or(filters) => json!({ "$or": children(filters)? }),
        Filter::Not(inner) => match inner.as_ref() {
            Filter::Not(filter) => compile_filter(collection, filter, declared)?,
            Filter::Eq(field, value) if field.always_present() => {
                json!({ data_field(field)?: { "$ne": value } })
            }
            Filter::In(field, values) if field.always_present() => {
                json!({ data_field(field)?: { "$nin": values } })
            }
            _ => {
                return Err(unsupported(
                    "`Not` is supported only around `Eq` or `In` on the document ID or chunk ID",
                ));
            }
        },
    })
}

fn output_fields() -> Value {
    json!({
        "dataFields": ["chunkId", "text", "documentId", "metadataJson", "sourceUri", "title"],
        "vectorFields": [VECTOR_FIELD],
    })
}

fn data_object_body(scope: &Scope, chunk: &Chunk, filterable: &BTreeSet<String>) -> Value {
    let metadata_json = serde_json::to_string(&chunk.metadata).unwrap_or_else(|_| "{}".into());
    let mut data = json!({
        "scope": scope.as_str(),
        "chunkId": chunk.id,
        "text": chunk.text,
        "documentId": chunk.document_id,
        "metadataJson": metadata_json,
    });
    for (field, value) in [("sourceUri", &chunk.source_uri), ("title", &chunk.title)] {
        if let Some(value) = value {
            data[field] = json!(value);
        }
    }
    for key in filterable {
        if let Some(value) = chunk.metadata.get(key) {
            data[format!("{METADATA_FIELD_PREFIX}{key}")] = json!(value);
        }
    }
    json!({
        "data": data,
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
#[serde(rename_all = "camelCase")]
struct QueryResponse {
    #[serde(default)]
    data_objects: Vec<WireDataObject>,
    #[serde(default)]
    next_page_token: String,
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
    chunk_id: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    document_id: String,
    #[serde(default)]
    metadata_json: String,
    #[serde(default)]
    source_uri: Option<String>,
    #[serde(default)]
    title: Option<String>,
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
    let id = Some(wire.data_object.data.chunk_id.clone()).filter(|id| !id.is_empty()).ok_or_else(
        || RagError::VectorStoreError {
            backend: "agent_retrieval".to_string(),
            message: "search result data object carries no chunkId; re-ingest a collection \
                      written by an earlier release"
                .to_string(),
        },
    )?;
    let embedding = wire
        .data_object
        .vectors
        .get(VECTOR_FIELD)
        .and_then(|vector| vector.dense.as_ref())
        .map(|dense| dense.values.clone())
        .unwrap_or_default();
    let data = wire.data_object.data;
    let mut chunk = Chunk::new(id, data.document_id, data.text).with_embedding(embedding);
    chunk.metadata = serde_json::from_str(&data.metadata_json).unwrap_or_default();
    chunk.source_uri = data.source_uri;
    chunk.title = data.title;
    Ok(SearchResult { chunk, score: wire.distance as f32 })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with(prefix: Option<&str>) -> AgentRetrievalStore {
        let mut config = AgentRetrievalConfig::new("p", "l");
        if let Some(prefix) = prefix {
            config = config.with_collection_prefix(prefix);
        }
        AgentRetrievalStore::with_client(
            config,
            GcpHttpClient::builder(gcp_error_context(), "https://vectorsearch.googleapis.com")
                .api_version(API_VERSION)
                .credentials(
                    google_cloud_auth::credentials::api_key_credentials::Builder::new("k").build(),
                )
                .build()
                .expect("build client"),
        )
    }

    // The client's token cache needs a runtime.
    #[tokio::test]
    async fn underscores_become_hyphens_one_to_one() {
        let store = store_with(None);
        let ids: Vec<String> = ["contract_search", "docs", "a1_b2", "x"]
            .iter()
            .map(|name| store.collection_id(name).expect("valid name"))
            .collect();
        assert_eq!(ids, ["contract-search", "docs", "a1-b2", "x"]);
        assert_eq!(
            store_with(Some("rag-")).collection_resource("my_docs").expect("valid name"),
            "projects/p/locations/l/collections/rag-my-docs",
        );
        // With a prefix, the name need not start with a letter.
        assert_eq!(store_with(Some("rag-")).collection_id("9lives").expect("valid"), "rag-9lives");
    }

    #[tokio::test]
    async fn names_that_would_need_rewriting_are_refused() {
        let store = store_with(None);
        let long = "x".repeat(64);
        for name in ["contract-search", "Docs", "docs v2", "9lives", "trailing_", "", &long] {
            assert_eq!(
                store.collection_id(name).map_err(|error| error.to_string()),
                Err(format!(
                    "Vector store error (agent_retrieval): invalid collection name `{name}`: use 1 \
                     to 63 ASCII lowercase letters, digits, or underscores, starting with a \
                     letter, ending with a letter or digit; each underscore becomes a hyphen in \
                     the Agent Retrieval collection ID"
                )),
            );
        }
        assert_eq!(
            store.collection_id(&"x".repeat(63)).map(|id| id.len()).map_err(|e| e.to_string()),
            Ok(63),
            "63 characters is the longest ID",
        );
    }

    #[tokio::test]
    async fn a_prefix_that_would_need_rewriting_is_refused() {
        for prefix in ["Rag_", "rag_", "9rag-"] {
            assert_eq!(
                store_with(Some(prefix)).collection_id("docs").map_err(|error| error.to_string()),
                Err(format!(
                    "Vector store error (agent_retrieval): invalid collection prefix `{prefix}`: \
                     use ASCII lowercase letters, digits, or hyphens, starting with a letter"
                )),
            );
        }
    }

    #[test]
    fn data_object_body_stores_text_metadata_and_scope_with_the_vector() {
        let scope = Scope::new("tenant-a").expect("valid scope");
        let chunk =
            Chunk::new("c1", "d1", "hello").with_embedding(vec![0.1, 0.2]).with_metadata("k", "v");
        assert_eq!(
            data_object_body(&scope, &chunk, &BTreeSet::new()),
            json!({
                "data": {
                    "scope": "tenant-a",
                    "chunkId": "c1",
                    "text": "hello",
                    "documentId": "d1",
                    "metadataJson": "{\"k\":\"v\"}",
                },
                "vectors": { "embedding": { "dense": { "values": [0.1f32, 0.2f32] } } },
            }),
        );
        let cited = chunk.with_source_uri("https://example.com/a").with_title("A");
        assert_eq!(
            data_object_body(&scope, &cited, &BTreeSet::from(["k".to_string()]))["data"],
            json!({
                "scope": "tenant-a",
                "chunkId": "c1",
                "text": "hello",
                "documentId": "d1",
                "metadataJson": "{\"k\":\"v\"}",
                "sourceUri": "https://example.com/a",
                "title": "A",
                "m_k": "v",
            }),
        );
    }

    #[test]
    fn filters_compile_to_the_documented_operators_within_two_levels() {
        let declared = BTreeSet::from(["lang".to_string()]);
        let filter = Filter::or([
            Filter::eq(Field::metadata("lang"), "en"),
            !Filter::is_in(Field::DocumentId, ["a"]),
            !Filter::eq(Field::ChunkId, "c"),
        ]);
        assert_eq!(
            compile_filter("docs", &filter, &declared).unwrap(),
            json!({ "$or": [
                { "m_lang": { "$eq": "en" } },
                { "documentId": { "$nin": ["a"] } },
                { "chunkId": { "$ne": "c" } },
            ]})
        );
        for refused in [
            Filter::exists(Field::SourceUri),
            !Filter::eq(Field::SourceUri, "x"),
            !Filter::eq(Field::metadata("lang"), "en"),
            Filter::eq(Field::metadata("tier"), "gold"),
        ] {
            assert!(
                matches!(
                    compile_filter("docs", &refused, &declared),
                    Err(RagError::UnsupportedFilter(_))
                ),
                "{refused:?}"
            );
        }
        let deep = json!({ "$and": [{ "$or": [{ "$and": [{ "a": { "$eq": "b" } }] }] }] });
        assert_eq!(filter_depth(&deep), 3);
    }

    #[test]
    fn data_object_ids_are_rfc_1035_labels_unique_per_scope_and_chunk() {
        let single = Scope::single_tenant();
        // Stored objects are addressed by this value; a change orphans them.
        assert_eq!(data_object_id(&single, "doc_0"), "c0e6d271a29395745a14b97334b718e54");
        let ids: std::collections::BTreeSet<String> = [
            data_object_id(&single, "doc_0"),
            data_object_id(&single, "doc_1"),
            data_object_id(&Scope::new("a").unwrap(), "doc_0"),
            data_object_id(&Scope::new("a/b").unwrap(), "Weird ID/../x"),
        ]
        .into_iter()
        .collect();
        assert_eq!(ids.len(), 4);
        for id in &ids {
            assert_eq!(id.len(), 33);
            assert!(id.starts_with('c'));
            assert!(id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()), "{id}");
        }
    }
}
