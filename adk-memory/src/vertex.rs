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
//! Transport, credential caching, and operation polling come from
//! [`adk_gcp`]: [`GcpHttpClient`] carries the ADC/auth-header/bounded-read
//! plumbing and [`LroPoller`] the identity-pinned operation polling, both
//! branded with this backend's error identity via [`GcpErrorContext`].
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

use crate::{MemoryEntry, MemoryService, SearchRequest, SearchResponse};
use adk_core::{AdkError, Content, ErrorCategory, ErrorComponent, Event, Result};
use adk_gcp::{
    GcpErrorCodes, GcpErrorContext, GcpHttpClient, LroPoller, VertexResourceName,
    is_canonical_reasoning_engine_id, truncate_for_error,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use google_cloud_auth::credentials::Credentials;
use reqwest::Method;
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;
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

/// The machine-readable codes this backend stamps on shared-plumbing errors.
const ERROR_CODES: GcpErrorCodes = GcpErrorCodes {
    invalid_input: "memory.vertex.invalid_input",
    unauthorized: "memory.vertex.unauthorized",
    forbidden: "memory.vertex.forbidden",
    not_found: "memory.vertex.not_found",
    rate_limited: "memory.vertex.rate_limited",
    timeout: "memory.vertex.timeout",
    unavailable: "memory.vertex.unavailable",
    credentials_unavailable: "memory.vertex.credentials_unavailable",
    invalid_response: "memory.vertex.invalid_response",
    invalid_request: "memory.vertex.invalid_request",
    upstream_error: "memory.vertex.upstream_error",
    operation_failed: "memory.vertex.operation_failed",
};

/// This backend's error identity: component Memory, provider `vertex_ai`.
fn error_context() -> GcpErrorContext {
    GcpErrorContext::new(ErrorComponent::Memory, ERROR_CODES, "vertex memory")
}

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
    client: GcpHttpClient,
    poller: LroPoller,
    project_id: String,
    location: String,
    reasoning_engine: Option<String>,
}

impl VertexAiMemoryBankService {
    /// Creates a new service using Application Default Credentials (ADC).
    ///
    /// # Errors
    ///
    /// Returns an error when ADC cannot be constructed, the endpoint is not
    /// a valid secure origin, or the HTTP client cannot be built.
    pub fn new_with_adc(config: VertexAiMemoryConfig) -> Result<Self> {
        Self::build(config, None)
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
        Self::build(config, Some(credentials))
    }

    fn build(config: VertexAiMemoryConfig, credentials: Option<Credentials>) -> Result<Self> {
        let mut builder = GcpHttpClient::builder(error_context(), config.endpoint())
            .api_version(MEMORY_API_VERSION)
            .scopes([CLOUD_PLATFORM_SCOPE])
            .connect_timeout(HTTP_CONNECT_TIMEOUT)
            .request_timeout(HTTP_REQUEST_TIMEOUT)
            .auth_timeout(AUTH_HEADERS_TIMEOUT)
            .max_response_bytes(MAX_RESPONSE_BYTES);
        if let Some(credentials) = credentials {
            builder = builder.credentials(credentials);
        }
        let poller = LroPoller::new()
            .with_poll_timeout(OPERATION_POLL_TIMEOUT)
            .with_initial_delay(OPERATION_POLL_INITIAL_DELAY)
            .with_max_delay(OPERATION_POLL_MAX_DELAY);
        Ok(Self {
            client: builder.build()?,
            poller,
            project_id: config.project_id,
            location: config.location,
            reasoning_engine: config.reasoning_engine,
        })
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
        let events: Vec<Value> =
            contents.iter().map(|content| json!({ "content": content })).collect();
        let body = json!({
            "directContentsSource": { "events": events },
            "scope": memory_scope(app_name, user_id),
        });
        let request = self
            .client
            .request(Method::POST, &format!("{parent}/memories:generate"))
            .await?
            .json(&body);
        let operation = self.client.send_value(request).await?;
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
        let body = json!({
            "scope": scope,
            retrieval_params_key: retrieval_params,
        });
        let request = self
            .client
            .request(Method::POST, &format!("{parent}/memories:retrieve"))
            .await?
            .json(&body);
        let value = self.client.send_value(request).await?;
        serde_json::from_value(value).map_err(|error| {
            let error = truncate_for_error(&error.to_string());
            self.client
                .errors()
                .invalid_response(format!("failed to parse memories:retrieve response: {error}"))
        })
    }

    async fn delete_memory(&self, memory_name: &str) -> Result<()> {
        validate_memory_resource_name(memory_name, &self.project_id, &self.location)?;
        let request = self.client.request(Method::DELETE, memory_name).await?;
        let operation = self.client.send_value(request).await?;
        self.wait_for_operation(operation, "memory delete").await?;
        Ok(())
    }

    /// Polls a long-running operation to completion, scoped to this
    /// service's project and location so a compromised or buggy server
    /// cannot redirect polling elsewhere.
    async fn wait_for_operation(
        &self,
        initial: Value,
        operation_kind: &str,
    ) -> Result<Option<Value>> {
        self.poller
            .wait_for_operation(
                &self.client,
                initial,
                operation_kind,
                false,
                &self.project_id,
                &self.location,
            )
            .await
    }

    fn memories_parent(&self, app_name: &str) -> Result<String> {
        let reasoning_engine = self.resolve_reasoning_engine_id(app_name)?;
        Ok(VertexResourceName::new(&self.project_id, &self.location, reasoning_engine).to_string())
    }

    fn resolve_reasoning_engine_id(&self, app_name: &str) -> Result<String> {
        let Some(candidate) = self.reasoning_engine.as_deref() else {
            if is_canonical_reasoning_engine_id(app_name) {
                return Ok(app_name.to_string());
            }
            return Err(self.client.errors().invalid_input(
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
        Err(self.client.errors().invalid_input(format!(
            "reasoning engine '{candidate}' is invalid. Provide a numeric ID or the exact resource name '{prefix}<numeric-id>'",
        )))
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
            return Err(self.client.errors().invalid_input(format!(
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
        Err(self.client.errors().invalid_response(format!(
            "vertex memory deletion did not converge after {DELETE_MAX_PAGES} pages; remaining memories for this scope must be deleted in Google Cloud",
        )))
    }

    async fn health_check(&self) -> Result<()> {
        // Credential acquisition exercises ADC and token refresh — the
        // failure mode a readiness probe needs to catch.
        self.client.auth_headers().await.map(|_| ())
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

/// Rejects memory resource names outside this service's project and
/// location before issuing a DELETE. Crate-specific: the shared scope check
/// does not know Memory Bank names must carry a `/memories/` segment.
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
    Err(error_context().invalid_response(format!(
        "memory resource name '{name}' does not belong to projects/{project_id}/locations/{location}; refusing to delete it",
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_matches_python_shape() {
        assert_eq!(
            memory_scope("weather-app", "u-1"),
            json!({"app_name": "weather-app", "user_id": "u-1"}),
        );
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
