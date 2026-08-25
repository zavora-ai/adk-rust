//! Vertex AI RAG Engine backend: corpus reads and context retrieval.
//!
//! [`VertexRagEngineClient`] is a read-only data-plane client for the RAG
//! Engine v1beta1 surface under `projects/{project}/locations/{location}`:
//! it gets and lists `ragCorpora`, lists `ragFiles`, and retrieves contexts
//! with `:retrieveContexts`. Corpus lifecycle and file ingestion are
//! provisioning concerns and are out of scope — use the Vertex AI console or
//! the `RagCorpora`/`RagFiles` management APIs.
//!
//! [`VertexAiRagRetrievalTool`] exposes retrieval as an [`adk_core::Tool`],
//! the Rust analog of adk-python's `VertexAiRagRetrieval`.
//!
//! Transport and credential caching come from [`adk_gcp::GcpHttpClient`],
//! branded with this backend's error identity (component `Memory`, codes
//! `rag.vertex.*` — `AdkError` has no dedicated RAG component, and retrieval
//! is the memory domain).
//!
//! # Example
//!
//! ```rust,no_run
//! use adk_rag::vertex_rag::{VertexRagConfig, VertexRagEngineClient};
//!
//! # fn main() -> adk_core::Result<()> {
//! let config = VertexRagConfig::new("my-project", "us-central1");
//! let client = VertexRagEngineClient::new_with_adc(config)?;
//! # Ok(())
//! # }
//! ```

use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result, Tool, ToolContext};
use adk_gcp::{GcpErrorCodes, GcpErrorContext, GcpHttpClient, truncate_for_error};
use async_trait::async_trait;
use google_cloud_auth::credentials::Credentials;
use reqwest::Method;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use tracing::debug;

const RAG_API_VERSION: &str = "v1beta1";
const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const AUTH_HEADERS_TIMEOUT: Duration = Duration::from_secs(30);
/// Upper bound on list pagination rounds, so a server that keeps returning
/// page tokens cannot spin this client forever.
const LIST_MAX_PAGES: usize = 1_000;

/// Environment variable holding the GCP project.
const ENV_GOOGLE_CLOUD_PROJECT: &str = "GOOGLE_CLOUD_PROJECT";
/// Environment variable holding the GCP location.
const ENV_GOOGLE_CLOUD_LOCATION: &str = "GOOGLE_CLOUD_LOCATION";

/// The machine-readable codes this backend stamps on shared-plumbing errors.
const ERROR_CODES: GcpErrorCodes = GcpErrorCodes {
    invalid_input: "rag.vertex.invalid_input",
    unauthorized: "rag.vertex.unauthorized",
    forbidden: "rag.vertex.forbidden",
    not_found: "rag.vertex.not_found",
    rate_limited: "rag.vertex.rate_limited",
    timeout: "rag.vertex.timeout",
    unavailable: "rag.vertex.unavailable",
    credentials_unavailable: "rag.vertex.credentials_unavailable",
    invalid_response: "rag.vertex.invalid_response",
    invalid_request: "rag.vertex.invalid_request",
    upstream_error: "rag.vertex.upstream_error",
    // The read-only surface has no long-running operations; required by the
    // table but never stamped.
    operation_failed: "rag.vertex.operation_failed",
};

/// This backend's error identity: component Memory (retrieval is the memory
/// domain; `AdkError` has no dedicated RAG component), provider `vertex_ai`.
fn error_context() -> GcpErrorContext {
    GcpErrorContext::new(ErrorComponent::Memory, ERROR_CODES, "vertex rag")
}

/// Configuration for [`VertexRagEngineClient`].
///
/// Mirrors `VertexAiMemoryConfig`: project, location, optional endpoint
/// override, and a [`from_env`](Self::from_env) constructor.
///
/// # Example
///
/// ```rust
/// use adk_rag::vertex_rag::VertexRagConfig;
///
/// let config = VertexRagConfig::new("my-project", "us-central1");
/// ```
#[derive(Debug, Clone)]
pub struct VertexRagConfig {
    project_id: String,
    location: String,
    endpoint: Option<String>,
}

impl VertexRagConfig {
    /// Creates a config for the given project and location.
    pub fn new(project_id: impl Into<String>, location: impl Into<String>) -> Self {
        Self { project_id: project_id.into(), location: location.into(), endpoint: None }
    }

    /// Builds a config from `GOOGLE_CLOUD_PROJECT` and
    /// `GOOGLE_CLOUD_LOCATION`. Values are trimmed; blank counts as missing.
    ///
    /// # Errors
    ///
    /// Returns an invalid-input error naming every missing or blank variable.
    pub fn from_env() -> Result<Self> {
        let read = |key: &str| {
            std::env::var(key)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        };
        let project_id = read(ENV_GOOGLE_CLOUD_PROJECT);
        let location = read(ENV_GOOGLE_CLOUD_LOCATION);

        match (project_id, location) {
            (Some(project_id), Some(location)) => Ok(Self::new(project_id, location)),
            (project_id, location) => {
                let missing = [
                    (ENV_GOOGLE_CLOUD_PROJECT, project_id.is_none()),
                    (ENV_GOOGLE_CLOUD_LOCATION, location.is_none()),
                ]
                .into_iter()
                .filter_map(|(key, is_missing)| is_missing.then_some(key))
                .collect::<Vec<_>>()
                .join(", ");
                Err(AdkError::new(
                    ErrorComponent::Memory,
                    ErrorCategory::InvalidInput,
                    "rag.vertex.missing_env",
                    format!(
                        "missing or blank environment variable(s): {missing}. Set them, or construct the config with VertexRagConfig::new",
                    ),
                )
                .with_provider("vertex_ai"))
            }
        }
    }

    /// Sets a custom API origin.
    ///
    /// The origin receives Google authorization headers plus corpus content.
    /// Use only a trusted HTTPS origin, or loopback HTTP for local tests.
    /// Userinfo, paths, queries, and fragments are rejected before transport.
    #[must_use]
    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = Some(endpoint.into());
        self
    }

    fn endpoint(&self) -> String {
        self.endpoint
            .clone()
            .unwrap_or_else(|| format!("https://{}-aiplatform.googleapis.com", self.location))
    }
}

// ===== Wire types (v1beta1, camelCase JSON, lenient deserialization) =====

/// A RAG corpus (`google.cloud.aiplatform.v1beta1.RagCorpus`), read-only view.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RagCorpus {
    /// Full `projects/*/locations/*/ragCorpora/*` resource name.
    pub name: Option<String>,
    /// Human-readable display name.
    pub display_name: Option<String>,
    /// Corpus description.
    pub description: Option<String>,
    /// Backing vector database configuration, kept opaque.
    pub vector_db_config: Option<Value>,
    /// Corpus lifecycle status.
    pub corpus_status: Option<CorpusStatus>,
    /// Number of imported files (v1beta1 only).
    #[serde(deserialize_with = "lenient_i64")]
    pub rag_files_count: Option<i64>,
    /// Creation timestamp (RFC 3339).
    pub create_time: Option<String>,
    /// Last-update timestamp (RFC 3339).
    pub update_time: Option<String>,
}

/// Lifecycle status of a [`RagCorpus`].
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CorpusStatus {
    /// Lifecycle state, e.g. `ACTIVE` or `ERROR`.
    pub state: Option<String>,
    /// Error reason when the state is `ERROR`.
    pub error_status: Option<String>,
}

/// A file imported into a RAG corpus
/// (`google.cloud.aiplatform.v1beta1.RagFile`), read-only view.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RagFile {
    /// Full `.../ragCorpora/*/ragFiles/*` resource name.
    pub name: Option<String>,
    /// Human-readable display name.
    pub display_name: Option<String>,
    /// File description.
    pub description: Option<String>,
    /// File size in bytes (proto JSON `int64`, string or number).
    #[serde(deserialize_with = "lenient_i64")]
    pub size_bytes: Option<i64>,
    /// File type, e.g. `RAG_FILE_TYPE_PDF`.
    pub rag_file_type: Option<String>,
    /// Import status, kept opaque.
    pub file_status: Option<Value>,
    /// Creation timestamp (RFC 3339).
    pub create_time: Option<String>,
    /// Last-update timestamp (RFC 3339).
    pub update_time: Option<String>,
}

/// A retrieved context passage from `:retrieveContexts`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RagContext {
    /// URI of the source file the passage came from.
    pub source_uri: Option<String>,
    /// Display name of the source file.
    pub source_display_name: Option<String>,
    /// The retrieved passage text.
    pub text: Option<String>,
    /// Relevance score (semantics depend on the corpus's distance measure).
    pub score: Option<f64>,
    /// The chunk the passage came from.
    pub chunk: Option<RagChunk>,
}

/// The stored chunk backing a [`RagContext`].
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RagChunk {
    /// Chunk identifier within its file.
    pub chunk_id: Option<String>,
    /// Identifier of the file the chunk belongs to.
    pub file_id: Option<String>,
    /// Chunk text.
    pub text: Option<String>,
    /// Page range the chunk spans, for paginated sources.
    pub page_span: Option<PageSpan>,
}

/// Page range of a [`RagChunk`], 1-based and inclusive.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PageSpan {
    /// First page of the span.
    #[serde(deserialize_with = "lenient_i64")]
    pub first_page: Option<i64>,
    /// Last page of the span.
    #[serde(deserialize_with = "lenient_i64")]
    pub last_page: Option<i64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct ListRagCorporaResponse {
    rag_corpora: Vec<RagCorpus>,
    next_page_token: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct ListRagFilesResponse {
    rag_files: Vec<RagFile>,
    next_page_token: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RetrieveContextsResponse {
    contexts: RagContextsEnvelope,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RagContextsEnvelope {
    contexts: Vec<RagContext>,
}

/// Accepts proto JSON `int64` as either a string or a number.
fn lenient_i64<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<i64>, D::Error> {
    match Option::<Value>::deserialize(deserializer)? {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => Ok(number.as_i64()),
        Some(Value::String(text)) => Ok(text.parse().ok()),
        Some(_) => Ok(None),
    }
}

// ===== Retrieval request =====

/// A corpus to retrieve from, optionally narrowed to specific files.
#[derive(Debug, Clone)]
pub struct RagResource {
    rag_corpus: String,
    rag_file_ids: Vec<String>,
}

impl RagResource {
    /// Creates a resource for a corpus ID or full
    /// `projects/*/locations/*/ragCorpora/*` resource name.
    pub fn new(rag_corpus: impl Into<String>) -> Self {
        Self { rag_corpus: rag_corpus.into(), rag_file_ids: Vec::new() }
    }

    /// Restricts retrieval to the given file IDs within the corpus.
    #[must_use]
    pub fn with_rag_file_ids(mut self, ids: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.rag_file_ids = ids.into_iter().map(Into::into).collect();
        self
    }
}

/// A `:retrieveContexts` request.
///
/// The `similarity_top_k` and `vector_distance_threshold` knobs keep
/// adk-python's names but are sent on the current wire path —
/// `query.ragRetrievalConfig.topK` and
/// `query.ragRetrievalConfig.filter.vectorDistanceThreshold`. The v1beta1
/// spellings `query.similarityTopK` and
/// `vertexRagStore.vectorDistanceThreshold` are deprecated and removed from
/// v1, so this client never emits them.
///
/// # Example
///
/// ```rust
/// use adk_rag::vertex_rag::RetrieveContextsRequest;
///
/// let request = RetrieveContextsRequest::new("what is our refund policy?", ["support-docs"])
///     .similarity_top_k(5)
///     .vector_distance_threshold(0.7);
/// ```
#[derive(Debug, Clone)]
pub struct RetrieveContextsRequest {
    query: String,
    resources: Vec<RagResource>,
    top_k: Option<u32>,
    vector_distance_threshold: Option<f64>,
    vector_similarity_threshold: Option<f64>,
}

impl RetrieveContextsRequest {
    /// Creates a request for a query over one or more corpora.
    ///
    /// Each corpus may be a bare ID or a full
    /// `projects/*/locations/*/ragCorpora/*` resource name; bare IDs are
    /// resolved against the client's project and location. Use
    /// [`with_resources`](Self::with_resources) to narrow corpora to
    /// specific files.
    pub fn new(
        query: impl Into<String>,
        rag_corpora: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            query: query.into(),
            resources: rag_corpora.into_iter().map(RagResource::new).collect(),
            top_k: None,
            vector_distance_threshold: None,
            vector_similarity_threshold: None,
        }
    }

    /// Replaces the plain corpora with explicit [`RagResource`]s.
    #[must_use]
    pub fn with_resources(mut self, resources: impl IntoIterator<Item = RagResource>) -> Self {
        self.resources = resources.into_iter().collect();
        self
    }

    /// Sets the number of contexts to retrieve
    /// (`query.ragRetrievalConfig.topK` on the wire).
    #[must_use]
    pub fn similarity_top_k(mut self, top_k: u32) -> Self {
        self.top_k = Some(top_k);
        self
    }

    /// Keeps only contexts within this vector distance
    /// (`query.ragRetrievalConfig.filter.vectorDistanceThreshold` on the
    /// wire). Mutually exclusive with
    /// [`vector_similarity_threshold`](Self::vector_similarity_threshold).
    #[must_use]
    pub fn vector_distance_threshold(mut self, threshold: f64) -> Self {
        self.vector_distance_threshold = Some(threshold);
        self
    }

    /// Keeps only contexts at or above this vector similarity
    /// (`query.ragRetrievalConfig.filter.vectorSimilarityThreshold` on the
    /// wire). Mutually exclusive with
    /// [`vector_distance_threshold`](Self::vector_distance_threshold).
    #[must_use]
    pub fn vector_similarity_threshold(mut self, threshold: f64) -> Self {
        self.vector_similarity_threshold = Some(threshold);
        self
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WireRetrieveRequest<'a> {
    vertex_rag_store: WireRagStore,
    query: WireRagQuery<'a>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WireRagStore {
    rag_resources: Vec<WireRagResource>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WireRagResource {
    rag_corpus: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    rag_file_ids: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WireRagQuery<'a> {
    text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    rag_retrieval_config: Option<WireRetrievalConfig>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WireRetrievalConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    top_k: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    filter: Option<WireRetrievalFilter>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WireRetrievalFilter {
    #[serde(skip_serializing_if = "Option::is_none")]
    vector_distance_threshold: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    vector_similarity_threshold: Option<f64>,
}

// ===== Client =====

/// ADC-authenticated, read-only client for the Vertex AI RAG Engine
/// v1beta1 data plane.
///
/// Performs [`get_corpus`](Self::get_corpus),
/// [`list_corpora`](Self::list_corpora),
/// [`list_rag_files`](Self::list_rag_files), and
/// [`retrieve_contexts`](Self::retrieve_contexts) against pre-provisioned
/// `ragCorpora`. Corpus creation and file import are provisioning concerns
/// and are out of scope.
pub struct VertexRagEngineClient {
    client: GcpHttpClient,
    project_id: String,
    location: String,
}

impl std::fmt::Debug for VertexRagEngineClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The transport carries credentials; expose only the scope.
        f.debug_struct("VertexRagEngineClient")
            .field("project_id", &self.project_id)
            .field("location", &self.location)
            .finish_non_exhaustive()
    }
}

impl VertexRagEngineClient {
    /// Creates a new client using Application Default Credentials (ADC).
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use adk_rag::vertex_rag::{VertexRagConfig, VertexRagEngineClient};
    ///
    /// # fn main() -> adk_core::Result<()> {
    /// let config = VertexRagConfig::new("my-project", "us-central1");
    /// let client = VertexRagEngineClient::new_with_adc(config)?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error when ADC cannot be constructed, the endpoint is not
    /// a valid secure origin, or the HTTP client cannot be built.
    pub fn new_with_adc(config: VertexRagConfig) -> Result<Self> {
        Self::build(config, None)
    }

    /// Creates a new client with explicit credentials.
    ///
    /// # Errors
    ///
    /// Returns an error when the endpoint is not a valid secure origin or
    /// the redirect-disabled HTTP client cannot be built.
    pub fn with_credentials(config: VertexRagConfig, credentials: Credentials) -> Result<Self> {
        Self::build(config, Some(credentials))
    }

    fn build(config: VertexRagConfig, credentials: Option<Credentials>) -> Result<Self> {
        let mut builder = GcpHttpClient::builder(error_context(), config.endpoint())
            .api_version(RAG_API_VERSION)
            .connect_timeout(HTTP_CONNECT_TIMEOUT)
            .request_timeout(HTTP_REQUEST_TIMEOUT)
            .auth_timeout(AUTH_HEADERS_TIMEOUT);
        if let Some(credentials) = credentials {
            builder = builder.credentials(credentials);
        }
        Ok(Self {
            client: builder.build()?,
            project_id: config.project_id,
            location: config.location,
        })
    }

    /// The `projects/{project}/locations/{location}` parent this client
    /// operates under.
    pub fn location_path(&self) -> String {
        format!("projects/{}/locations/{}", self.project_id, self.location)
    }

    /// Resolves a bare corpus ID to a full resource name; full names pass
    /// through after a shape check.
    fn corpus_resource_name(&self, corpus: &str) -> Result<String> {
        let corpus = corpus.trim();
        if corpus.is_empty() {
            return Err(self
                .client
                .errors()
                .invalid_input("rag corpus must be a corpus ID or a full resource name"));
        }
        if !corpus.contains('/') {
            return Ok(format!("{}/ragCorpora/{corpus}", self.location_path()));
        }
        if corpus.starts_with("projects/") && corpus.contains("/ragCorpora/") {
            return Ok(corpus.to_string());
        }
        Err(self.client.errors().invalid_input(format!(
            "invalid rag corpus '{}': expected a bare corpus ID or a projects/*/locations/*/ragCorpora/* resource name",
            truncate_for_error(corpus),
        )))
    }

    /// Gets a corpus by ID or full resource name.
    ///
    /// # Errors
    ///
    /// Returns a not-found error with provisioning guidance when the corpus
    /// does not exist, and transport/status/parse errors otherwise.
    pub async fn get_corpus(&self, corpus: &str) -> Result<RagCorpus> {
        let name = self.corpus_resource_name(corpus)?;
        debug!(rag.corpus = name.as_str(), "fetching rag corpus");
        let request = self.client.request(Method::GET, &name).await?;
        let value = self.client.send_value_allow_not_found(request).await?.ok_or_else(|| {
            self.client.errors().error(
                ErrorCategory::NotFound,
                ERROR_CODES.not_found,
                format!(
                    "rag corpus '{name}' was not found. Verify the corpus ID, project, and location; corpus creation is a provisioning concern outside this read-only client — create it in the Vertex AI console or with the RagCorpora API",
                ),
            )
        })?;
        self.parse("ragCorpora get", value)
    }

    /// Gets a corpus and verifies it is ready for retrieval.
    ///
    /// # Errors
    ///
    /// Returns the [`get_corpus`](Self::get_corpus) errors, an error when
    /// the corpus reports the `ERROR` state, and an invalid-input error with
    /// import guidance when the corpus has no imported files.
    pub async fn ensure_corpus_ready(&self, corpus: &str) -> Result<RagCorpus> {
        let corpus = self.get_corpus(corpus).await?;
        let name = corpus.name.as_deref().unwrap_or("<unnamed>");
        if let Some(status) = &corpus.corpus_status
            && status.state.as_deref() == Some("ERROR")
        {
            let reason = status.error_status.as_deref().unwrap_or("no error detail reported");
            return Err(self.client.errors().error(
                ErrorCategory::Unavailable,
                ERROR_CODES.unavailable,
                format!("rag corpus '{name}' is in the ERROR state: {reason}. Re-import the failed files or recreate the corpus before retrieving"),
            ));
        }
        if corpus.rag_files_count == Some(0) {
            return Err(self.client.errors().invalid_input(format!(
                "rag corpus '{name}' has no imported files, so retrieval would always return nothing. Import documents with the RagFiles import API or the Vertex AI console first",
            )));
        }
        Ok(corpus)
    }

    /// Lists every corpus in the project and location, following pagination.
    ///
    /// # Errors
    ///
    /// Returns an error on transport failure, timeout, a non-success HTTP
    /// status, an unparseable response body, or runaway pagination.
    pub async fn list_corpora(&self) -> Result<Vec<RagCorpus>> {
        let path = format!("{}/ragCorpora", self.location_path());
        let mut corpora = Vec::new();
        let mut page_token: Option<String> = None;
        for _ in 0..LIST_MAX_PAGES {
            let mut request = self.client.request(Method::GET, &path).await?;
            if let Some(token) = &page_token {
                request = request.query(&[("pageToken", token)]);
            }
            let value = self.client.send_value(request).await?;
            let page: ListRagCorporaResponse = self.parse("ragCorpora list", value)?;
            corpora.extend(page.rag_corpora);
            page_token = page.next_page_token.filter(|token| !token.is_empty());
            if page_token.is_none() {
                return Ok(corpora);
            }
        }
        Err(self.client.errors().invalid_response(format!(
            "ragCorpora list did not terminate within {LIST_MAX_PAGES} pages; the server kept returning page tokens",
        )))
    }

    /// Lists every file in a corpus, following pagination.
    ///
    /// # Errors
    ///
    /// Returns an error on transport failure, timeout, a non-success HTTP
    /// status, an unparseable response body, or runaway pagination.
    pub async fn list_rag_files(&self, corpus: &str) -> Result<Vec<RagFile>> {
        let path = format!("{}/ragFiles", self.corpus_resource_name(corpus)?);
        let mut files = Vec::new();
        let mut page_token: Option<String> = None;
        for _ in 0..LIST_MAX_PAGES {
            let mut request = self.client.request(Method::GET, &path).await?;
            if let Some(token) = &page_token {
                request = request.query(&[("pageToken", token)]);
            }
            let value = self.client.send_value(request).await?;
            let page: ListRagFilesResponse = self.parse("ragFiles list", value)?;
            files.extend(page.rag_files);
            page_token = page.next_page_token.filter(|token| !token.is_empty());
            if page_token.is_none() {
                return Ok(files);
            }
        }
        Err(self.client.errors().invalid_response(format!(
            "ragFiles list did not terminate within {LIST_MAX_PAGES} pages; the server kept returning page tokens",
        )))
    }

    /// Retrieves the contexts most relevant to a query.
    ///
    /// `POST {location}:retrieveContexts` on the current wire path:
    /// `query.ragRetrievalConfig.{topK,filter}` — never the deprecated
    /// `query.similarityTopK` or `vertexRagStore.vectorDistanceThreshold`
    /// spellings.
    ///
    /// # Errors
    ///
    /// Returns an invalid-input error when the query is blank, no corpus is
    /// given, or both filter thresholds are set, and transport/status/parse
    /// errors otherwise.
    pub async fn retrieve_contexts(
        &self,
        request: &RetrieveContextsRequest,
    ) -> Result<Vec<RagContext>> {
        if request.query.trim().is_empty() {
            return Err(self.client.errors().invalid_input("retrieval query must not be blank"));
        }
        if request.resources.is_empty() {
            return Err(self.client.errors().invalid_input(
                "at least one rag corpus is required; pass a corpus ID or full resource name",
            ));
        }
        if request.vector_distance_threshold.is_some()
            && request.vector_similarity_threshold.is_some()
        {
            return Err(self.client.errors().invalid_input(
                "vector_distance_threshold and vector_similarity_threshold are mutually exclusive; the ragRetrievalConfig filter is a oneof",
            ));
        }

        let rag_resources = request
            .resources
            .iter()
            .map(|resource| {
                Ok(WireRagResource {
                    rag_corpus: self.corpus_resource_name(&resource.rag_corpus)?,
                    rag_file_ids: resource.rag_file_ids.clone(),
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let filter = match (request.vector_distance_threshold, request.vector_similarity_threshold)
        {
            (None, None) => None,
            (distance, similarity) => Some(WireRetrievalFilter {
                vector_distance_threshold: distance,
                vector_similarity_threshold: similarity,
            }),
        };
        let rag_retrieval_config = if request.top_k.is_none() && filter.is_none() {
            None
        } else {
            Some(WireRetrievalConfig { top_k: request.top_k, filter })
        };
        let body = WireRetrieveRequest {
            vertex_rag_store: WireRagStore { rag_resources },
            query: WireRagQuery { text: &request.query, rag_retrieval_config },
        };

        let path = format!("{}:retrieveContexts", self.location_path());
        debug!(rag.corpora = request.resources.len(), "retrieving rag contexts");
        let http_request = self.client.request(Method::POST, &path).await?.json(&body);
        let value = self.client.send_value(http_request).await?;
        let response: RetrieveContextsResponse = self.parse("retrieveContexts", value)?;
        Ok(response.contexts.contexts)
    }

    fn parse<R: for<'de> Deserialize<'de>>(&self, operation: &str, value: Value) -> Result<R> {
        serde_json::from_value(value).map_err(|error| {
            let error = truncate_for_error(&error.to_string());
            self.client
                .errors()
                .invalid_response(format!("failed to parse {operation} response: {error}"))
        })
    }
}

// ===== Tool =====

/// Retrieval from Vertex AI RAG Engine corpora as an [`adk_core::Tool`].
///
/// The Rust analog of adk-python's `VertexAiRagRetrieval`: the agent passes
/// a `query` string and receives a JSON array of
/// `{text, sourceUri, sourceDisplayName, score}` objects. The tool is
/// read-only and concurrency-safe, so `ToolExecutionStrategy::Auto` may
/// dispatch it in parallel.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use adk_rag::vertex_rag::{
///     VertexAiRagRetrievalTool, VertexRagConfig, VertexRagEngineClient,
/// };
///
/// # fn main() -> adk_core::Result<()> {
/// let config = VertexRagConfig::new("my-project", "us-central1");
/// let client = Arc::new(VertexRagEngineClient::new_with_adc(config)?);
/// let tool = VertexAiRagRetrievalTool::new(client, vec!["support-docs".into()])
///     .similarity_top_k(5)
///     .vector_distance_threshold(0.7);
/// # let _ = tool;
/// # Ok(())
/// # }
/// ```
pub struct VertexAiRagRetrievalTool {
    client: Arc<VertexRagEngineClient>,
    rag_corpora: Vec<String>,
    similarity_top_k: Option<u32>,
    vector_distance_threshold: Option<f64>,
}

impl VertexAiRagRetrievalTool {
    /// Creates a retrieval tool over the given corpora (bare IDs or full
    /// resource names).
    pub fn new(client: Arc<VertexRagEngineClient>, rag_corpora: Vec<String>) -> Self {
        Self { client, rag_corpora, similarity_top_k: None, vector_distance_threshold: None }
    }

    /// Sets the number of contexts to retrieve per query
    /// (`query.ragRetrievalConfig.topK` on the wire).
    #[must_use]
    pub fn similarity_top_k(mut self, top_k: u32) -> Self {
        self.similarity_top_k = Some(top_k);
        self
    }

    /// Keeps only contexts within this vector distance
    /// (`query.ragRetrievalConfig.filter.vectorDistanceThreshold` on the
    /// wire).
    #[must_use]
    pub fn vector_distance_threshold(mut self, threshold: f64) -> Self {
        self.vector_distance_threshold = Some(threshold);
        self
    }
}

#[async_trait]
impl Tool for VertexAiRagRetrievalTool {
    fn name(&self) -> &str {
        "vertex_rag_retrieval"
    }

    fn description(&self) -> &str {
        "Retrieve the passages most relevant to a query from Vertex AI RAG Engine corpora"
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The natural-language query to retrieve relevant passages for"
                }
            },
            "required": ["query"]
        }))
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn is_concurrency_safe(&self) -> bool {
        true
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .ok_or_else(|| AdkError::tool("missing required 'query' parameter"))?;

        let mut request = RetrieveContextsRequest::new(query, self.rag_corpora.clone());
        if let Some(top_k) = self.similarity_top_k {
            request = request.similarity_top_k(top_k);
        }
        if let Some(threshold) = self.vector_distance_threshold {
            request = request.vector_distance_threshold(threshold);
        }

        let contexts = self.client.retrieve_contexts(&request).await?;
        debug!(rag.contexts = contexts.len(), "vertex_rag_retrieval returned contexts");
        Ok(Value::Array(contexts.iter().map(context_to_tool_output).collect()))
    }
}

/// Projects a [`RagContext`] onto the tool's output shape.
fn context_to_tool_output(context: &RagContext) -> Value {
    let mut output = json!({ "text": context.text.as_deref().unwrap_or("") });
    if let Some(source_uri) = &context.source_uri {
        output["sourceUri"] = json!(source_uri);
    }
    if let Some(source_display_name) = &context.source_display_name {
        output["sourceDisplayName"] = json!(source_display_name);
    }
    if let Some(score) = context.score {
        output["score"] = json!(score);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client() -> VertexRagEngineClient {
        let credentials =
            google_cloud_auth::credentials::api_key_credentials::Builder::new("test-key").build();
        VertexRagEngineClient::with_credentials(
            VertexRagConfig::new("proj", "us-central1"),
            credentials,
        )
        .expect("build test client")
    }

    #[test]
    fn endpoint_defaults_to_regional_origin() {
        let config = VertexRagConfig::new("proj", "europe-west4");
        assert_eq!(config.endpoint(), "https://europe-west4-aiplatform.googleapis.com");
    }

    // The credentials builder requires a running Tokio runtime.
    #[tokio::test]
    async fn corpus_names_resolve_bare_ids_and_pass_full_names_through() {
        let client = client();
        assert_eq!(
            client.corpus_resource_name("1234").unwrap(),
            "projects/proj/locations/us-central1/ragCorpora/1234",
        );
        let full = "projects/other/locations/eu/ragCorpora/9";
        assert_eq!(client.corpus_resource_name(full).unwrap(), full);
        assert_eq!(client.corpus_resource_name("").unwrap_err().http_status_code(), 400);
        assert_eq!(client.corpus_resource_name("a/b/c").unwrap_err().http_status_code(), 400);
    }

    #[test]
    fn deprecated_knob_names_serialize_on_the_modern_wire_path() {
        let request = RetrieveContextsRequest::new("q", ["projects/p/locations/l/ragCorpora/1"])
            .similarity_top_k(3)
            .vector_distance_threshold(0.5);
        let body = WireRetrieveRequest {
            vertex_rag_store: WireRagStore {
                rag_resources: vec![WireRagResource {
                    rag_corpus: "projects/p/locations/l/ragCorpora/1".into(),
                    rag_file_ids: vec![],
                }],
            },
            query: WireRagQuery {
                text: &request.query,
                rag_retrieval_config: Some(WireRetrievalConfig {
                    top_k: request.top_k,
                    filter: Some(WireRetrievalFilter {
                        vector_distance_threshold: request.vector_distance_threshold,
                        vector_similarity_threshold: None,
                    }),
                }),
            },
        };
        assert_eq!(
            serde_json::to_value(&body).unwrap(),
            json!({
                "vertexRagStore": {
                    "ragResources": [
                        { "ragCorpus": "projects/p/locations/l/ragCorpora/1" }
                    ]
                },
                "query": {
                    "text": "q",
                    "ragRetrievalConfig": {
                        "topK": 3,
                        "filter": { "vectorDistanceThreshold": 0.5 }
                    }
                }
            }),
        );
    }

    #[test]
    fn corpus_and_file_responses_deserialize_leniently() {
        let corpus: RagCorpus = serde_json::from_value(json!({
            "name": "projects/p/locations/l/ragCorpora/1",
            "displayName": "docs",
            "corpusStatus": { "state": "ACTIVE" },
            "ragFilesCount": "12",
            "someFutureField": { "nested": true },
        }))
        .unwrap();
        assert_eq!(corpus.rag_files_count, Some(12));
        assert_eq!(corpus.corpus_status.unwrap().state.as_deref(), Some("ACTIVE"));

        let file: RagFile = serde_json::from_value(json!({
            "name": "projects/p/locations/l/ragCorpora/1/ragFiles/9",
            "sizeBytes": 2048,
            "ragFileType": "RAG_FILE_TYPE_PDF",
        }))
        .unwrap();
        assert_eq!(file.size_bytes, Some(2048));
    }
}
