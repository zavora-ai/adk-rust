//! Vertex AI Agent Engine Memory Bank backend.
//!
//! Rust analog of Python ADK's `VertexAiMemoryBankService`: long-term
//! memories are generated from conversation content with
//! `memories:generate` (a long-running operation) and retrieved with
//! `memories:retrieve` (similarity search), scoped to
//! `{"app_name", "user_id"}` so one engine can serve many users.
//!
//! Endpoints are the v1beta1 Memory Bank surface under
//! `projects/{project}/locations/{location}/reasoningEngines/{engine}`.
//!
//! # Example
//!
//! ```rust,no_run
//! use adk_memory::{VertexAiMemoryBankService, VertexAiMemoryConfig};
//!
//! # fn main() -> adk_core::Result<()> {
//! // Inside a deployed engine container the platform sets the env vars.
//! let config = VertexAiMemoryConfig::from_env()?;
//! let memory = VertexAiMemoryBankService::new_with_adc(config)?;
//! # Ok(())
//! # }
//! ```
//!
//! `ToolContext::search_memory` works unchanged through the existing
//! adapter: wrap the service in
//! [`MemoryServiceAdapter`](crate::MemoryServiceAdapter) to obtain an
//! `adk_core::Memory`.

// The ADC credential caching, bounded HTTP, and LRO polling below are copied
// from adk-session/src/vertex.rs rather than shared: extracting the pattern
// into a helper crate is Wave 3's job (adk-gcp, PR 3.1/3.3 of the Agent
// Engine plan), which migrates both call sites at once.

use crate::{MemoryEntry, MemoryService, SearchRequest, SearchResponse};
use adk_core::{AdkError, Content, ErrorCategory, ErrorComponent, Event, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use google_cloud_auth::credentials::{self, CacheableResource, Credentials};
use reqwest::{Client, RequestBuilder, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::time::Instant;
use tracing::debug;

const MEMORY_API_VERSION: &str = "v1beta1";
const CLOUD_PLATFORM_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";
const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const AUTH_HEADERS_TIMEOUT: Duration = Duration::from_secs(30);
// Same backoff policy as adk-session's wait_for_operation: 100 ms initial,
// 2 s cap, bounded deadline.
const OPERATION_POLL_TIMEOUT: Duration = Duration::from_secs(120);
const OPERATION_POLL_INITIAL_DELAY: Duration = Duration::from_millis(100);
const OPERATION_POLL_MAX_DELAY: Duration = Duration::from_secs(2);
const MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;
/// Page size used when enumerating a scope's memories for deletion.
const DELETE_PAGE_SIZE: usize = 100;
/// Upper bound on delete pagination rounds, so a server that keeps returning
/// page tokens cannot spin this client forever.
const DELETE_MAX_PAGES: usize = 1_000;

/// Environment variable holding the GCP project (set inside deployed engines).
const ENV_GOOGLE_CLOUD_PROJECT: &str = "GOOGLE_CLOUD_PROJECT";
/// Environment variable holding the GCP location.
const ENV_GOOGLE_CLOUD_LOCATION: &str = "GOOGLE_CLOUD_LOCATION";
/// Environment variable holding the bare numeric engine ID.
const ENV_GOOGLE_CLOUD_AGENT_ENGINE_ID: &str = "GOOGLE_CLOUD_AGENT_ENGINE_ID";

/// Configuration for [`VertexAiMemoryBankService`].
///
/// Mirrors `VertexAiSessionConfig`: project, location, optional reasoning
/// engine, optional endpoint override, and a [`from_env`](Self::from_env)
/// constructor reading the platform's container environment.
///
/// # Example
///
/// ```rust
/// use adk_memory::VertexAiMemoryConfig;
///
/// let config = VertexAiMemoryConfig::new("my-project", "us-central1")
///     .with_reasoning_engine("1234567890");
/// ```
#[derive(Debug, Clone)]
pub struct VertexAiMemoryConfig {
    project_id: String,
    location: String,
    reasoning_engine: Option<String>,
    endpoint: Option<String>,
}

impl VertexAiMemoryConfig {
    /// Creates a config for the given project and location.
    pub fn new(project_id: impl Into<String>, location: impl Into<String>) -> Self {
        Self {
            project_id: project_id.into(),
            location: location.into(),
            reasoning_engine: None,
            endpoint: None,
        }
    }

    /// Builds a config from the environment variables the platform sets
    /// inside deployed containers: `GOOGLE_CLOUD_PROJECT`,
    /// `GOOGLE_CLOUD_LOCATION`, and `GOOGLE_CLOUD_AGENT_ENGINE_ID` (the bare
    /// numeric engine ID). Values are trimmed; blank counts as missing.
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
        let agent_engine_id = read(ENV_GOOGLE_CLOUD_AGENT_ENGINE_ID);

        match (project_id, location, agent_engine_id) {
            (Some(project_id), Some(location), Some(agent_engine_id)) => {
                Ok(Self::new(project_id, location).with_reasoning_engine(agent_engine_id))
            }
            (project_id, location, agent_engine_id) => {
                let missing = [
                    (ENV_GOOGLE_CLOUD_PROJECT, project_id.is_none()),
                    (ENV_GOOGLE_CLOUD_LOCATION, location.is_none()),
                    (ENV_GOOGLE_CLOUD_AGENT_ENGINE_ID, agent_engine_id.is_none()),
                ]
                .into_iter()
                .filter_map(|(key, is_missing)| is_missing.then_some(key))
                .collect::<Vec<_>>()
                .join(", ");
                Err(AdkError::new(
                    ErrorComponent::Memory,
                    ErrorCategory::InvalidInput,
                    "memory.vertex.missing_env",
                    format!(
                        "missing or blank environment variable(s): {missing}. The Agent Engine platform sets these inside deployed containers; set them explicitly elsewhere, or construct the config with VertexAiMemoryConfig::new",
                    ),
                )
                .with_provider("vertex_ai"))
            }
        }
    }

    /// Sets the reasoning engine numeric ID or full resource name.
    #[must_use]
    pub fn with_reasoning_engine(mut self, reasoning_engine: impl Into<String>) -> Self {
        self.reasoning_engine = Some(reasoning_engine.into());
        self
    }

    /// Sets a custom API origin.
    ///
    /// The origin receives Google authorization headers plus memory content.
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

/// Vertex AI Agent Engine Memory Bank service.
///
/// Implements [`MemoryService`] over the platform's `memories:generate` and
/// `memories:retrieve` endpoints. Memories are scoped to
/// `{"app_name": ..., "user_id": ...}`, matching adk-python's
/// `VertexAiMemoryBankService` so both runtimes share one Memory Bank.
pub struct VertexAiMemoryBankService {
    http_client: Client,
    endpoint: String,
    project_id: String,
    location: String,
    reasoning_engine: Option<String>,
    credentials: Credentials,
    auth_headers: Arc<RwLock<Option<reqwest::header::HeaderMap>>>,
}

impl VertexAiMemoryBankService {
    /// Creates a new service using Application Default Credentials (ADC).
    ///
    /// # Errors
    ///
    /// Returns an error when ADC cannot be constructed, the endpoint is not
    /// a valid secure origin, or the HTTP client cannot be built.
    pub fn new_with_adc(config: VertexAiMemoryConfig) -> Result<Self> {
        let credentials = credentials::Builder::default()
            .with_scopes([CLOUD_PLATFORM_SCOPE])
            .build()
            .map_err(|error| {
                let error = truncate_for_error(&error.to_string());
                Self::auth_error(format!("failed to build vertex memory ADC credentials: {error}"))
            })?;
        Self::with_credentials(config, credentials)
    }

    /// Creates a new service with explicit credentials.
    ///
    /// # Errors
    ///
    /// Returns an error when the endpoint is not a valid secure origin or
    /// the redirect-disabled HTTP client cannot be built.
    pub fn with_credentials(
        config: VertexAiMemoryConfig,
        credentials: Credentials,
    ) -> Result<Self> {
        let http_client = Client::builder()
            .connect_timeout(HTTP_CONNECT_TIMEOUT)
            .timeout(HTTP_REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| {
                let error = truncate_for_error(&error.without_url().to_string());
                Self::memory_error(format!(
                    "failed to build bounded vertex memory HTTP client: {error}"
                ))
            })?;
        let service = Self {
            http_client,
            endpoint: config.endpoint(),
            project_id: config.project_id,
            location: config.location,
            reasoning_engine: config.reasoning_engine,
            credentials,
            auth_headers: Arc::new(RwLock::new(None)),
        };
        service.build_url("")?;
        Ok(service)
    }

    /// Generates memories from an explicit slice of events.
    ///
    /// Mirrors adk-python's `VertexAiMemoryBankService.add_events_to_memory`
    /// so callers can persist only recent turns instead of a whole session.
    /// Events without content are skipped; an all-skipped or empty slice is
    /// a no-op.
    ///
    /// This is an inherent method, not part of [`MemoryService`] — the trait
    /// has no subset-of-events operation.
    ///
    /// # Errors
    ///
    /// Returns an error when the request fails or the returned long-running
    /// operation reports failure or does not finish within the poll deadline.
    pub async fn add_events_to_memory(
        &self,
        app_name: &str,
        user_id: &str,
        events: &[Event],
    ) -> Result<()> {
        let contents: Vec<&Content> =
            events.iter().filter_map(|event| event.llm_response.content.as_ref()).collect();
        self.generate_memories(app_name, user_id, &contents).await
    }

    async fn generate_memories(
        &self,
        app_name: &str,
        user_id: &str,
        contents: &[&Content],
    ) -> Result<()> {
        if contents.is_empty() {
            debug!("no content-bearing entries; skipping memories:generate");
            return Ok(());
        }
        let parent = self.memories_parent(app_name)?;
        let url = self.build_url(&format!("{MEMORY_API_VERSION}/{parent}/memories:generate"))?;
        let events: Vec<Value> =
            contents.iter().map(|content| json!({ "content": content })).collect();
        let body = json!({
            "directContentsSource": { "events": events },
            "scope": memory_scope(app_name, user_id),
        });
        let request = self.apply_auth(self.http_client.post(url).json(&body)).await?;
        let operation = self.send_value(request).await?;
        // :generate's LRO response payload is not needed — only completion.
        self.wait_for_operation(operation, "memories generate").await?;
        Ok(())
    }

    async fn retrieve_page(
        &self,
        parent: &str,
        scope: &Value,
        retrieval_params_key: &str,
        retrieval_params: Value,
    ) -> Result<RetrieveMemoriesResponse> {
        let url = self.build_url(&format!("{MEMORY_API_VERSION}/{parent}/memories:retrieve"))?;
        let body = json!({
            "scope": scope,
            retrieval_params_key: retrieval_params,
        });
        let request = self.apply_auth(self.http_client.post(url).json(&body)).await?;
        let value = self.send_value(request).await?;
        serde_json::from_value(value).map_err(|error| {
            let error = truncate_for_error(&error.to_string());
            Self::memory_error(format!("failed to parse memories:retrieve response: {error}"))
        })
    }

    async fn delete_memory(&self, memory_name: &str) -> Result<()> {
        validate_memory_resource_name(memory_name, &self.project_id, &self.location)?;
        let url = self.build_url(&format!("{MEMORY_API_VERSION}/{memory_name}"))?;
        let request = self.apply_auth(self.http_client.delete(url)).await?;
        let operation = self.send_value(request).await?;
        self.wait_for_operation(operation, "memory delete").await?;
        Ok(())
    }

    fn memories_parent(&self, app_name: &str) -> Result<String> {
        let reasoning_engine = self.resolve_reasoning_engine_id(app_name)?;
        Ok(format!(
            "projects/{}/locations/{}/reasoningEngines/{reasoning_engine}",
            self.project_id, self.location,
        ))
    }

    fn resolve_reasoning_engine_id(&self, app_name: &str) -> Result<String> {
        let Some(candidate) = self.reasoning_engine.as_deref() else {
            if is_canonical_reasoning_engine_id(app_name) {
                return Ok(app_name.to_string());
            }
            return Err(Self::invalid_input(
                "when no fixed reasoning engine is configured, app_name must be its canonical numeric reasoning-engine ID without leading zeros; configure with_reasoning_engine for logical app names or full resource names",
            ));
        };
        if is_canonical_reasoning_engine_id(candidate) {
            return Ok(candidate.to_string());
        }
        let prefix =
            format!("projects/{}/locations/{}/reasoningEngines/", self.project_id, self.location);
        if let Some(id) = candidate.strip_prefix(&prefix)
            && is_canonical_reasoning_engine_id(id)
        {
            return Ok(id.to_string());
        }
        let candidate = truncate_for_error(candidate);
        Err(Self::invalid_input(format!(
            "reasoning engine '{candidate}' is invalid. Provide a numeric ID or the exact resource name '{prefix}<numeric-id>'",
        )))
    }

    /// Builds a URL from the endpoint base, requiring HTTPS for
    /// non-loopback endpoints so memory content is never sent in cleartext.
    fn build_url(&self, path: &str) -> Result<String> {
        let mut url = reqwest::Url::parse(&self.endpoint).map_err(|error| {
            let error = truncate_for_error(&error.to_string());
            Self::invalid_input(format!("invalid Vertex AI endpoint URL: {error}"))
        })?;
        if url.scheme() != "https" && !(url.scheme() == "http" && is_loopback_host(&url)) {
            return Err(Self::invalid_input(
                "Vertex AI endpoint must use HTTPS for secure transmission of memory content",
            ));
        }
        if !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
        {
            return Err(Self::invalid_input(
                "Vertex AI endpoint must be an origin without userinfo, path, query, or fragment",
            ));
        }
        url.set_path(&format!("/{}", path.trim_start_matches('/')));
        Ok(url.to_string())
    }

    async fn auth_headers(&self) -> Result<reqwest::header::HeaderMap> {
        let cacheable_headers = tokio::time::timeout(
            AUTH_HEADERS_TIMEOUT,
            self.credentials.headers(Default::default()),
        )
        .await
        .map_err(|_| {
            Self::timeout_error(format!(
                "vertex memory credential header acquisition timed out after {} seconds",
                AUTH_HEADERS_TIMEOUT.as_secs_f64(),
            ))
        })?
        .map_err(Self::credentials_error)?;

        match cacheable_headers {
            CacheableResource::New { data, .. } => {
                *self.auth_headers.write().await = Some(data.clone());
                Ok(data)
            }
            CacheableResource::NotModified => {
                self.auth_headers.read().await.clone().ok_or_else(|| {
                    Self::auth_error(
                        "google cloud credentials returned NotModified before any cached auth headers were available",
                    )
                })
            }
        }
    }

    async fn apply_auth(&self, request: RequestBuilder) -> Result<RequestBuilder> {
        let headers = self.auth_headers().await?;
        Ok(request.headers(headers))
    }

    async fn send_value(&self, request: RequestBuilder) -> Result<Value> {
        let (status, body) = tokio::time::timeout(HTTP_REQUEST_TIMEOUT, async {
            let response = request.send().await.map_err(Self::transport_error)?;
            let status = response.status();
            if let Some(declared) = response.content_length()
                && declared > MAX_RESPONSE_BYTES as u64
            {
                return Err(Self::memory_error(format!(
                    "vertex memory response Content-Length {declared} exceeds the {MAX_RESPONSE_BYTES}-byte limit",
                ))
                .with_upstream_status(status.as_u16()));
            }
            let body = response.bytes().await.map_err(|error| {
                let error = truncate_for_error(&error.without_url().to_string());
                AdkError::unavailable(
                    ErrorComponent::Memory,
                    "memory.vertex.unavailable",
                    format!("failed to read vertex memory response body: {error}"),
                )
                .with_provider("vertex_ai")
                .with_upstream_status(status.as_u16())
            })?;
            if body.len() > MAX_RESPONSE_BYTES {
                return Err(Self::memory_error(format!(
                    "vertex memory response body of {} bytes exceeds the {MAX_RESPONSE_BYTES}-byte limit",
                    body.len(),
                ))
                .with_upstream_status(status.as_u16()));
            }
            Ok::<_, AdkError>((status, body))
        })
        .await
        .map_err(|_| {
            Self::timeout_error(format!(
                "vertex memory request timed out after {} seconds",
                HTTP_REQUEST_TIMEOUT.as_secs(),
            ))
        })??;

        if !status.is_success() {
            let body = String::from_utf8_lossy(&body);
            let body = if body.trim().is_empty() { "<empty body>" } else { body.as_ref() };
            return Err(Self::status_error(status, body));
        }
        if body.iter().all(u8::is_ascii_whitespace) {
            return Ok(Value::Object(Map::new()));
        }
        serde_json::from_slice(&body).map_err(|error| {
            let error = truncate_for_error(&error.to_string());
            Self::memory_error(format!("failed to parse vertex memory response JSON: {error}"))
                .with_upstream_status(status.as_u16())
        })
    }

    /// Polls a long-running operation to completion with the session
    /// backend's backoff policy, pinning the operation identity so a poll
    /// can never silently follow a different operation.
    async fn wait_for_operation(
        &self,
        initial: Value,
        operation_kind: &str,
    ) -> Result<Option<Value>> {
        let mut operation = parse_operation(initial, operation_kind)?;
        validate_operation_name(&operation.name, &self.project_id, &self.location)?;
        let operation_name = operation.name.clone();
        let deadline = Instant::now() + OPERATION_POLL_TIMEOUT;
        let mut delay = OPERATION_POLL_INITIAL_DELAY;

        loop {
            if operation.done {
                if let Some(error) = operation.error {
                    return Err(Self::operation_error(operation_kind, &operation_name, error));
                }
                return Ok(operation.response);
            }
            if Instant::now() >= deadline {
                return Err(Self::timeout_error(format!(
                    "vertex {operation_kind} operation '{operation_name}' did not complete within {} seconds; inspect the operation in Google Cloud before retrying",
                    OPERATION_POLL_TIMEOUT.as_secs_f64(),
                )));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            let poll = async {
                let url = self.build_url(&format!("{MEMORY_API_VERSION}/{operation_name}"))?;
                let request = self.apply_auth(self.http_client.get(url)).await?;
                self.send_value(request).await
            };
            let value = tokio::time::timeout(remaining, poll).await.map_err(|_| {
                Self::timeout_error(format!(
                    "vertex {operation_kind} operation '{operation_name}' did not complete within {} seconds; inspect the operation in Google Cloud before retrying",
                    OPERATION_POLL_TIMEOUT.as_secs_f64(),
                ))
            })??;
            let next = parse_operation(value, operation_kind)?;
            if next.name != operation_name {
                return Err(Self::memory_error(format!(
                    "vertex {operation_kind} poll changed operation identity from '{operation_name}' to '{}'; refusing to follow a different operation",
                    next.name,
                )));
            }
            operation = next;
            if !operation.done {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    continue;
                }
                tokio::time::sleep(delay.min(remaining)).await;
                delay = delay.saturating_mul(2).min(OPERATION_POLL_MAX_DELAY);
            }
        }
    }

    fn memory_error(message: impl Into<String>) -> AdkError {
        AdkError::internal(ErrorComponent::Memory, "memory.vertex.invalid_response", message)
            .with_provider("vertex_ai")
    }

    fn invalid_input(message: impl Into<String>) -> AdkError {
        AdkError::new(
            ErrorComponent::Memory,
            ErrorCategory::InvalidInput,
            "memory.vertex.invalid_input",
            message,
        )
        .with_provider("vertex_ai")
    }

    fn auth_error(message: impl Into<String>) -> AdkError {
        AdkError::unauthorized(ErrorComponent::Memory, "memory.vertex.unauthorized", message)
            .with_provider("vertex_ai")
    }

    fn credentials_error(error: google_cloud_auth::errors::CredentialsError) -> AdkError {
        let message = format!(
            "failed to obtain google cloud auth headers: {}",
            truncate_for_error(&error.to_string()),
        );
        if error.is_transient() {
            AdkError::unavailable(
                ErrorComponent::Memory,
                "memory.vertex.credentials_unavailable",
                message,
            )
            .with_provider("vertex_ai")
        } else {
            Self::auth_error(message)
        }
    }

    fn timeout_error(message: impl Into<String>) -> AdkError {
        AdkError::timeout(ErrorComponent::Memory, "memory.vertex.timeout", message)
            .with_provider("vertex_ai")
    }

    fn transport_error(error: reqwest::Error) -> AdkError {
        let timeout = error.is_timeout();
        let error = truncate_for_error(&error.without_url().to_string());
        if timeout {
            return Self::timeout_error(format!("vertex memory HTTP request timed out: {error}"));
        }
        AdkError::unavailable(
            ErrorComponent::Memory,
            "memory.vertex.unavailable",
            format!("failed to send vertex memory request: {error}"),
        )
        .with_provider("vertex_ai")
    }

    fn status_error(status: StatusCode, body: &str) -> AdkError {
        let message = format!(
            "vertex memory request failed with status {}: {}",
            status.as_u16(),
            truncate_for_error(body),
        );
        let (category, code) = match status.as_u16() {
            400 | 409 | 422 => (ErrorCategory::InvalidInput, "memory.vertex.invalid_request"),
            401 => (ErrorCategory::Unauthorized, "memory.vertex.unauthorized"),
            403 => (ErrorCategory::Forbidden, "memory.vertex.forbidden"),
            404 => (ErrorCategory::NotFound, "memory.vertex.not_found"),
            408 | 504 => (ErrorCategory::Timeout, "memory.vertex.timeout"),
            429 => (ErrorCategory::RateLimited, "memory.vertex.rate_limited"),
            500 | 502 | 503 => (ErrorCategory::Unavailable, "memory.vertex.unavailable"),
            _ => (ErrorCategory::Internal, "memory.vertex.upstream_error"),
        };
        AdkError::new(ErrorComponent::Memory, category, code, message)
            .with_provider("vertex_ai")
            .with_upstream_status(status.as_u16())
    }

    fn operation_error(
        operation_kind: &str,
        operation_name: &str,
        error: VertexOperationError,
    ) -> AdkError {
        let operation_name = truncate_for_error(operation_name);
        let operation_message =
            if error.message.trim().is_empty() { "<no error message>" } else { &error.message };
        let message = format!(
            "vertex {operation_kind} operation '{operation_name}' failed with code {}: {}",
            error.code,
            truncate_for_error(operation_message),
        );
        let category = match error.code {
            1 => ErrorCategory::Cancelled,
            3 | 6 | 9 | 11 => ErrorCategory::InvalidInput,
            4 => ErrorCategory::Timeout,
            5 => ErrorCategory::NotFound,
            7 => ErrorCategory::Forbidden,
            8 => ErrorCategory::RateLimited,
            10 | 14 => ErrorCategory::Unavailable,
            12 => ErrorCategory::Unsupported,
            16 => ErrorCategory::Unauthorized,
            _ => ErrorCategory::Internal,
        };
        AdkError::new(ErrorComponent::Memory, category, "memory.vertex.operation_failed", message)
            .with_provider("vertex_ai")
    }
}

#[async_trait]
impl MemoryService for VertexAiMemoryBankService {
    async fn add_session(
        &self,
        app_name: &str,
        user_id: &str,
        _session_id: &str,
        entries: Vec<MemoryEntry>,
    ) -> Result<()> {
        let contents: Vec<&Content> = entries.iter().map(|entry| &entry.content).collect();
        self.generate_memories(app_name, user_id, &contents).await
    }

    async fn search(&self, req: SearchRequest) -> Result<SearchResponse> {
        if let Some(project_id) = &req.project_id {
            // Memory Bank scopes are exact-match maps; a project dimension
            // would need its own scope key, which no other runtime writes.
            let project_id = truncate_for_error(project_id);
            return Err(Self::invalid_input(format!(
                "project-scoped search (project_id '{project_id}') is not supported by the Memory Bank backend; memories are scoped by app_name and user_id only",
            )));
        }
        let parent = self.memories_parent(&req.app_name)?;
        let scope = memory_scope(&req.app_name, &req.user_id);
        let top_k = req.limit.unwrap_or(10);
        let response = self
            .retrieve_page(
                &parent,
                &scope,
                "similaritySearchParams",
                json!({ "searchQuery": req.query, "topK": top_k }),
            )
            .await?;
        let memories = response.retrieved_memories.into_iter().map(retrieved_to_entry).collect();
        Ok(SearchResponse { memories })
    }

    async fn delete_user(&self, app_name: &str, user_id: &str) -> Result<()> {
        let parent = self.memories_parent(app_name)?;
        let scope = memory_scope(app_name, user_id);
        let mut page_token: Option<String> = None;
        for _ in 0..DELETE_MAX_PAGES {
            let mut params = json!({ "pageSize": DELETE_PAGE_SIZE });
            if let Some(token) = &page_token {
                params["pageToken"] = json!(token);
            }
            let response =
                self.retrieve_page(&parent, &scope, "simpleRetrievalParams", params).await?;
            for retrieved in &response.retrieved_memories {
                self.delete_memory(&retrieved.memory.name).await?;
            }
            match response.next_page_token {
                Some(token) if !token.is_empty() => page_token = Some(token),
                _ => return Ok(()),
            }
        }
        Err(Self::memory_error(format!(
            "vertex memory deletion did not converge after {DELETE_MAX_PAGES} pages; remaining memories for this scope must be deleted in Google Cloud",
        )))
    }

    async fn health_check(&self) -> Result<()> {
        // Credential acquisition exercises ADC and token refresh — the
        // failure mode a readiness probe needs to catch.
        self.auth_headers().await.map(|_| ())
    }
}

/// The exact scope map both adk-python and adk-rust write, so one Memory
/// Bank serves both runtimes.
fn memory_scope(app_name: &str, user_id: &str) -> Value {
    json!({ "app_name": app_name, "user_id": user_id })
}

fn retrieved_to_entry(retrieved: RetrievedMemory) -> MemoryEntry {
    // MemoryEntry has no metadata slot, so the memory resource name cannot
    // be preserved; updateTime survives as the entry timestamp.
    let timestamp = retrieved
        .memory
        .update_time
        .as_deref()
        .or(retrieved.memory.create_time.as_deref())
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map_or_else(Utc::now, |parsed| parsed.with_timezone(&Utc));
    MemoryEntry {
        content: Content::new("model").with_text(retrieved.memory.fact),
        author: "memory_bank".to_string(),
        timestamp,
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RetrieveMemoriesResponse {
    #[serde(default)]
    retrieved_memories: Vec<RetrievedMemory>,
    #[serde(default)]
    next_page_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RetrievedMemory {
    memory: VertexMemory,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VertexMemory {
    name: String,
    #[serde(default)]
    fact: String,
    #[serde(default)]
    create_time: Option<String>,
    #[serde(default)]
    update_time: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct VertexOperation {
    name: String,
    #[serde(default)]
    done: bool,
    #[serde(default)]
    error: Option<VertexOperationError>,
    #[serde(default)]
    response: Option<Value>,
}

#[derive(Debug, Deserialize, Serialize)]
struct VertexOperationError {
    #[serde(default)]
    code: i64,
    #[serde(default)]
    message: String,
}

fn parse_operation(value: Value, operation_kind: &str) -> Result<VertexOperation> {
    let operation: VertexOperation = serde_json::from_value(value).map_err(|error| {
        let error = truncate_for_error(&error.to_string());
        VertexAiMemoryBankService::memory_error(format!(
            "failed to parse vertex {operation_kind} operation: {error}"
        ))
    })?;
    if operation.name.trim().is_empty() {
        return Err(VertexAiMemoryBankService::memory_error(format!(
            "vertex {operation_kind} response did not contain an operation name"
        )));
    }
    if operation.error.is_some() && operation.response.is_some() {
        return Err(VertexAiMemoryBankService::memory_error(format!(
            "vertex {operation_kind} operation '{}' contains both error and response results",
            operation.name,
        )));
    }
    if !operation.done && (operation.error.is_some() || operation.response.is_some()) {
        return Err(VertexAiMemoryBankService::memory_error(format!(
            "vertex {operation_kind} operation '{}' contains a terminal result while done is false",
            operation.name,
        )));
    }
    Ok(operation)
}

/// Rejects operation names outside this service's project and location, so
/// a compromised or buggy server cannot redirect polling elsewhere.
fn validate_operation_name(name: &str, project_id: &str, location: &str) -> Result<()> {
    let prefix = format!("projects/{project_id}/locations/{location}/");
    if name.starts_with(&prefix) && !name.contains("://") && !name.contains("..") {
        return Ok(());
    }
    let name = truncate_for_error(name);
    Err(VertexAiMemoryBankService::memory_error(format!(
        "vertex operation name '{name}' does not belong to projects/{project_id}/locations/{location}",
    )))
}

/// Rejects memory resource names outside this service's project and
/// location before issuing a DELETE.
fn validate_memory_resource_name(name: &str, project_id: &str, location: &str) -> Result<()> {
    let prefix = format!("projects/{project_id}/locations/{location}/reasoningEngines/");
    if name.starts_with(&prefix)
        && name.contains("/memories/")
        && !name.contains("://")
        && !name.contains("..")
    {
        return Ok(());
    }
    let name = truncate_for_error(name);
    Err(VertexAiMemoryBankService::memory_error(format!(
        "memory resource name '{name}' does not belong to projects/{project_id}/locations/{location}; refusing to delete it",
    )))
}

fn is_canonical_reasoning_engine_id(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && (value == "0" || !value.starts_with('0'))
}

fn is_loopback_host(url: &reqwest::Url) -> bool {
    matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "::1"))
}

fn truncate_for_error(value: &str) -> String {
    const MAX_LEN: usize = 512;
    let mut sanitized = String::with_capacity(value.len().min(MAX_LEN));
    let mut truncated = false;
    for character in value.chars() {
        let character =
            if character.is_control() { char::REPLACEMENT_CHARACTER } else { character };
        if sanitized.len() + character.len_utf8() > MAX_LEN {
            truncated = true;
            break;
        }
        sanitized.push(character);
    }
    if truncated {
        sanitized.push_str("...");
    }
    sanitized
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_engine_ids() {
        assert!(is_canonical_reasoning_engine_id("123"));
        assert!(is_canonical_reasoning_engine_id("0"));
        assert!(!is_canonical_reasoning_engine_id("0123"));
        assert!(!is_canonical_reasoning_engine_id(""));
        assert!(!is_canonical_reasoning_engine_id("my-app"));
    }

    #[test]
    fn scope_matches_python_shape() {
        assert_eq!(
            memory_scope("weather-app", "u-1"),
            json!({"app_name": "weather-app", "user_id": "u-1"}),
        );
    }

    #[test]
    fn operation_names_outside_the_scope_are_rejected() {
        assert!(validate_operation_name("projects/p/locations/l/operations/1", "p", "l").is_ok());
        assert!(
            validate_operation_name("projects/other/locations/l/operations/1", "p", "l").is_err()
        );
        assert!(validate_operation_name("https://evil.example/op", "p", "l").is_err());
    }

    #[test]
    fn memory_names_outside_the_scope_are_rejected() {
        assert!(
            validate_memory_resource_name(
                "projects/p/locations/l/reasoningEngines/1/memories/m",
                "p",
                "l",
            )
            .is_ok()
        );
        assert!(
            validate_memory_resource_name(
                "projects/p/locations/l/reasoningEngines/1/sessions/s",
                "p",
                "l",
            )
            .is_err()
        );
        assert!(
            validate_memory_resource_name(
                "projects/other/locations/l/reasoningEngines/1/memories/m",
                "p",
                "l",
            )
            .is_err()
        );
    }

    #[test]
    fn endpoint_defaults_to_regional_origin() {
        let config = VertexAiMemoryConfig::new("p", "us-central1");
        assert_eq!(config.endpoint(), "https://us-central1-aiplatform.googleapis.com");
    }
}
