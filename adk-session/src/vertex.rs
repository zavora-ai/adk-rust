use crate::{
    AppendEventRequest, CreateRequest, DeleteRequest, Event, Events, GetRequest, KEY_PREFIX_TEMP,
    ListRequest, Session, SessionService, State,
};
use adk_core::{
    AdkError, AppName, CitationMetadata, Content, EmbeddedResource, ErrorCategory, ErrorComponent,
    FileDataPart, FinishReason, FunctionResponseData, InlineDataPart, Part, Result, RetryHint,
    SessionId, UsageMetadata, UserId,
};
use adk_gcp::{GcpErrorCodes, GcpErrorContext, GcpHttpClient, LroPoller, truncate_for_error};
use async_trait::async_trait;
use base64::{
    Engine,
    engine::general_purpose::{
        STANDARD as BASE64_STANDARD, STANDARD_NO_PAD as BASE64_STANDARD_NO_PAD,
        URL_SAFE as BASE64_URL_SAFE, URL_SAFE_NO_PAD as BASE64_URL_SAFE_NO_PAD,
    },
};
use chrono::{DateTime, Utc};
use google_cloud_auth::credentials::Credentials;
use reqwest::{Method, header::CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

const SESSION_API_VERSION: &str = "v1";
const OPERATION_POLL_TIMEOUT: Duration = Duration::from_secs(120);
const PAGINATION_TIMEOUT: Duration = Duration::from_secs(120);
const OPERATION_POLL_INITIAL_DELAY: Duration = Duration::from_millis(100);
const OPERATION_POLL_MAX_DELAY: Duration = Duration::from_secs(2);
const DEFAULT_MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;
const DEFAULT_MAX_REQUEST_BYTES: usize = 64 * 1024 * 1024;
const VERTEX_RAW_EVENT_METADATA_KEY: &str = "adk.vertex.session.raw_event_json";
const VERTEX_CUSTOM_METADATA_KEY: &str = "adk.vertex.session.custom_metadata_json";
const VERTEX_CANONICAL_EXTENSIONS_KEY: &str = "adk.vertex.session.canonical_extensions_json";
const VERTEX_CANONICAL_CONTENT_KEY: &str = "adk.vertex.session.canonical_content_json";
const VERTEX_PAGE_SIZE: usize = 100;
const VERTEX_MAX_USER_ID_CHARS: usize = 128;
const VERTEX_MAX_RESOURCE_SEGMENT_BYTES: usize = 512;
const VERTEX_VALUE_MAX_DEPTH: usize = 64;
const VERTEX_MAX_EXACT_INTEGER: u64 = (1_u64 << 53) - 1;
const VERTEX_MAX_PAGE_TOKEN_BYTES: usize = 64 * 1024;
const VERTEX_SESSION_SCOPE_CACHE_CAPACITY: usize = 1024;
const RUST_RAW_EVENT_ENVELOPE_KEY: &str = "_adkRust";
const VERTEX_IDENTITY_STATE_KEY: &str = "__adk_vertex_identity_v1";
const ENV_GOOGLE_CLOUD_PROJECT: &str = "GOOGLE_CLOUD_PROJECT";
const ENV_GOOGLE_CLOUD_LOCATION: &str = "GOOGLE_CLOUD_LOCATION";
const ENV_GOOGLE_CLOUD_AGENT_ENGINE_ID: &str = "GOOGLE_CLOUD_AGENT_ENGINE_ID";
// google/cloud/aiplatform/v1beta1/session.proto documents a 24-hour minimum
// for the input-only `ttl` member of the Session `expiration` oneof.
const MIN_SESSION_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Configuration for the Vertex AI Session API backend.
#[derive(Debug, Clone)]
pub struct VertexAiSessionConfig {
    /// Google Cloud project ID.
    pub project_id: String,
    /// GCP region (e.g. `us-central1`).
    pub location: String,
    /// Optional reasoning engine resource name.
    pub reasoning_engine: Option<String>,
    /// Optional custom API origin.
    ///
    /// The origin receives Google authorization headers plus session and event
    /// data. It must not contain userinfo, a path, a query, or a fragment.
    pub endpoint: Option<String>,
    /// Optional session time-to-live sent on session create.
    ///
    /// `ttl` is a member of the `Session` `expiration` oneof in
    /// `google/cloud/aiplatform/v1beta1/session.proto`: input-only, serialized
    /// as a JSON duration string (e.g. `"86400s"`), minimum 24 hours. Mutually
    /// exclusive with [`expire_time`](Self::expire_time).
    pub ttl: Option<Duration>,
    /// Optional absolute session expiration timestamp sent on session create.
    ///
    /// `expireTime` is the other `Session` `expiration` oneof member,
    /// serialized as an RFC 3339 timestamp. Mutually exclusive with
    /// [`ttl`](Self::ttl).
    pub expire_time: Option<DateTime<Utc>>,
}

impl VertexAiSessionConfig {
    /// Creates a new config with the given project ID and location.
    pub fn new(project_id: impl Into<String>, location: impl Into<String>) -> Self {
        Self {
            project_id: project_id.into(),
            location: location.into(),
            reasoning_engine: None,
            endpoint: None,
            ttl: None,
            expire_time: None,
        }
    }

    /// Builds a config from the environment variables the Vertex AI Agent
    /// Engine platform sets inside deployed containers.
    ///
    /// Reads `GOOGLE_CLOUD_PROJECT`, `GOOGLE_CLOUD_LOCATION`, and
    /// `GOOGLE_CLOUD_AGENT_ENGINE_ID` (the bare numeric agent engine ID, not a
    /// full resource name). Values are trimmed; blank values count as missing.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use adk_session::{VertexAiSessionConfig, VertexAiSessionService};
    ///
    /// # fn main() -> adk_core::Result<()> {
    /// let config = VertexAiSessionConfig::from_env()?;
    /// let service = VertexAiSessionService::new_with_adc(config)?;
    /// # Ok(())
    /// # }
    /// ```
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
                    ErrorComponent::Session,
                    ErrorCategory::InvalidInput,
                    "session.vertex.missing_env",
                    format!(
                        "missing or blank environment variable(s): {missing}. The Vertex AI Agent Engine platform sets these inside deployed containers (GOOGLE_CLOUD_AGENT_ENGINE_ID is the bare numeric engine ID); set them explicitly elsewhere, or construct the config with VertexAiSessionConfig::new",
                    ),
                )
                .with_provider("vertex_ai"))
            }
        }
    }

    /// Sets the reasoning engine numeric ID or full resource name.
    pub fn with_reasoning_engine(mut self, reasoning_engine: impl Into<String>) -> Self {
        self.reasoning_engine = Some(reasoning_engine.into());
        self
    }

    /// Sets a custom API origin.
    ///
    /// The origin receives Google authorization headers plus session and event
    /// data. Use only a trusted HTTPS origin, or loopback HTTP for local tests.
    /// Userinfo, paths, queries, and fragments are rejected before transport.
    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = Some(endpoint.into());
        self
    }

    /// Sets the session time-to-live sent on session create.
    ///
    /// `ttl` is input-only with a 24-hour minimum and is mutually exclusive
    /// with [`with_expire_time`](Self::with_expire_time); both constraints are
    /// enforced at service construction.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_session::VertexAiSessionConfig;
    /// use std::time::Duration;
    ///
    /// let config = VertexAiSessionConfig::new("my-project", "us-central1")
    ///     .with_ttl(Duration::from_secs(86_400)); // sent as "86400s"
    /// ```
    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = Some(ttl);
        self
    }

    /// Sets the absolute session expiration timestamp sent on session create.
    ///
    /// Mutually exclusive with [`with_ttl`](Self::with_ttl); the constraint is
    /// enforced at service construction.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_session::VertexAiSessionConfig;
    /// use chrono::{Duration, Utc};
    ///
    /// let config = VertexAiSessionConfig::new("my-project", "us-central1")
    ///     .with_expire_time(Utc::now() + Duration::days(7));
    /// ```
    pub fn with_expire_time(mut self, expire_time: DateTime<Utc>) -> Self {
        self.expire_time = Some(expire_time);
        self
    }

    fn endpoint(&self) -> String {
        let endpoint = self.endpoint.clone().unwrap_or_else(|| match self.location.as_str() {
            "global" => "https://aiplatform.googleapis.com".to_string(),
            "us" => "https://aiplatform.us.rep.googleapis.com".to_string(),
            "eu" => "https://aiplatform.eu.rep.googleapis.com".to_string(),
            location => format!("https://{location}-aiplatform.googleapis.com"),
        });
        let endpoint =
            if endpoint.contains("://") { endpoint } else { format!("https://{endpoint}") };

        let Ok(url) = reqwest::Url::parse(&endpoint) else {
            return endpoint;
        };
        url.to_string()
    }
}

/// The session backend's error identity, stamped by `adk-gcp` plumbing and
/// local constructors alike so every failure carries the same codes the
/// backend used before the Wave 3 consolidation.
const GCP_ERROR_CODES: GcpErrorCodes = GcpErrorCodes {
    invalid_input: "session.vertex.invalid_input",
    unauthorized: "session.vertex.unauthorized",
    forbidden: "session.vertex.forbidden",
    not_found: "session.vertex.not_found",
    rate_limited: "session.vertex.rate_limited",
    timeout: "session.vertex.timeout",
    unavailable: "session.vertex.unavailable",
    credentials_unavailable: "session.vertex.credentials_unavailable",
    invalid_response: "session.vertex.invalid_response",
    invalid_request: "session.vertex.invalid_request",
    upstream_error: "session.vertex.upstream_error",
    operation_failed: "session.vertex.operation_failed",
};

fn gcp_error_context() -> GcpErrorContext {
    GcpErrorContext::new(ErrorComponent::Session, GCP_ERROR_CODES, "vertex session")
        .with_response_too_large_code("session.vertex.response_too_large")
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SessionScope {
    app_name: String,
    user_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VertexSessionIdentity {
    schema_version: u8,
    app_name: String,
    user_id: String,
    session_id: String,
}

struct BoundedJsonWriter {
    bytes: Vec<u8>,
    limit: usize,
    exceeded: bool,
    observed_at_least: u64,
}

impl BoundedJsonWriter {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(limit.min(8 * 1024)),
            limit,
            exceeded: false,
            observed_at_least: 0,
        }
    }
}

impl Write for BoundedJsonWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let observed = self.bytes.len().checked_add(buffer.len());
        if observed.is_none_or(|observed| observed > self.limit) {
            self.exceeded = true;
            self.observed_at_least =
                observed.and_then(|value| u64::try_from(value).ok()).unwrap_or(u64::MAX);
            return Err(std::io::Error::other("encoded JSON body exceeds configured limit"));
        }
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct BoundedJsonCounter {
    written: usize,
    limit: usize,
    exceeded: bool,
    observed_at_least: u64,
}

impl BoundedJsonCounter {
    fn new(limit: usize) -> Self {
        Self { written: 0, limit, exceeded: false, observed_at_least: 0 }
    }
}

impl Write for BoundedJsonCounter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let Some(observed) = self.written.checked_add(buffer.len()) else {
            self.exceeded = true;
            self.observed_at_least = u64::MAX;
            return Err(std::io::Error::other("encoded JSON value exceeds configured limit"));
        };
        if observed > self.limit {
            self.exceeded = true;
            self.observed_at_least = u64::try_from(observed).unwrap_or(u64::MAX);
            return Err(std::io::Error::other("encoded JSON value exceeds configured limit"));
        }
        self.written = observed;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Vertex AI Session API service implementation.
pub struct VertexAiSessionService {
    client: GcpHttpClient,
    project_id: String,
    location: String,
    reasoning_engine: Option<String>,
    unmarked_session_app: Option<String>,
    max_response_bytes: usize,
    max_request_bytes: usize,
    pagination_timeout: Duration,
    session_ttl: Option<String>,
    session_expire_time: Option<String>,
    session_scopes: Arc<RwLock<VecDeque<(String, SessionScope)>>>,
}

impl VertexAiSessionService {
    /// Creates a new service using Application Default Credentials (ADC).
    ///
    /// # Errors
    ///
    /// Returns an error when ADC cannot be constructed, the endpoint is not a
    /// valid secure origin, or the bounded, redirect-disabled HTTP client
    /// cannot be constructed.
    pub fn new_with_adc(config: VertexAiSessionConfig) -> Result<Self> {
        Self::build(config, None)
    }

    /// Creates a new service with explicit credentials.
    ///
    /// # Errors
    ///
    /// Returns an error when the endpoint is not a valid secure origin, the
    /// session expiration config is invalid (both `ttl` and `expire_time`
    /// set, or a TTL below the 24-hour minimum), or the bounded,
    /// redirect-disabled HTTP client cannot be constructed.
    pub fn with_credentials(
        config: VertexAiSessionConfig,
        credentials: Credentials,
    ) -> Result<Self> {
        Self::build(config, Some(credentials))
    }

    fn build(config: VertexAiSessionConfig, credentials: Option<Credentials>) -> Result<Self> {
        // `ttl` and `expireTime` form the Session `expiration` oneof in
        // google/cloud/aiplatform/v1beta1/session.proto, so at most one may
        // be sent; `ttl` is input-only with a documented 24-hour minimum.
        if config.ttl.is_some() && config.expire_time.is_some() {
            return Err(Self::invalid_input(
                "session ttl and expire_time are mutually exclusive Session.expiration oneof members; configure at most one of with_ttl and with_expire_time",
            ));
        }
        if let Some(ttl) = config.ttl
            && ttl < MIN_SESSION_TTL
        {
            return Err(Self::invalid_input(format!(
                "session ttl of {}s is below the Vertex AI minimum of 24 hours (86400s)",
                ttl.as_secs(),
            )));
        }
        // Endpoint validation (HTTPS-or-loopback bare origin), the
        // redirect-disabled bounded HTTP client, ADC construction, and
        // cached auth headers all live in adk-gcp; the builder defaults
        // match this backend's original constants (10 s connect, 120 s
        // request, 30 s auth, 64 MiB responses).
        let mut builder = GcpHttpClient::builder(gcp_error_context(), config.endpoint())
            .api_version(SESSION_API_VERSION)
            .max_response_bytes(DEFAULT_MAX_RESPONSE_BYTES);
        if let Some(credentials) = credentials {
            builder = builder.credentials(credentials);
        }
        let client = builder.build()?;
        Ok(Self {
            client,
            project_id: config.project_id,
            location: config.location,
            reasoning_engine: config.reasoning_engine,
            unmarked_session_app: None,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            max_request_bytes: DEFAULT_MAX_REQUEST_BYTES,
            pagination_timeout: PAGINATION_TIMEOUT,
            session_ttl: config.ttl.map(proto_duration_string),
            session_expire_time: config.expire_time.map(|expire_time| {
                expire_time.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true)
            }),
            session_scopes: Arc::new(RwLock::new(VecDeque::new())),
        })
    }

    /// Allows one logical app to access unmarked Python ADK or pre-v2 sessions.
    ///
    /// This opt-in is only needed when a fixed reasoning engine is configured.
    /// Without a fixed engine, a canonical nonzero numeric `app_name` selects
    /// the matching reasoning-engine parent.
    #[must_use]
    pub fn allow_unmarked_sessions_for_app(mut self, app_name: impl Into<String>) -> Self {
        self.unmarked_session_app = Some(app_name.into());
        self
    }

    /// Sets the maximum decoded response bytes retained per response and across
    /// one paginated list operation.
    ///
    /// Values above the 64 MiB default weaken that protection. The caller is
    /// responsible for selecting a bound appropriate for the deployment.
    #[must_use]
    pub fn with_max_response_bytes(mut self, max_response_bytes: usize) -> Self {
        self.max_response_bytes = max_response_bytes;
        self.client = self.client.with_max_response_bytes(max_response_bytes);
        self
    }

    /// Sets the maximum encoded JSON request-body bytes.
    ///
    /// Values above the 64 MiB default weaken that protection. The caller is
    /// responsible for selecting a bound appropriate for the deployment.
    #[must_use]
    pub fn with_max_request_bytes(mut self, max_request_bytes: usize) -> Self {
        self.max_request_bytes = max_request_bytes;
        self
    }

    /// Sets the total elapsed deadline for one paginated list operation.
    #[must_use]
    pub fn with_pagination_timeout(mut self, pagination_timeout: Duration) -> Self {
        self.pagination_timeout = pagination_timeout;
        self
    }

    fn session_error(message: impl Into<String>) -> AdkError {
        gcp_error_context().invalid_response(message)
    }

    fn invalid_input(message: impl Into<String>) -> AdkError {
        gcp_error_context().invalid_input(message)
    }

    fn timeout_error(message: impl Into<String>) -> AdkError {
        gcp_error_context().timeout(message)
    }

    fn response_too_large(context: &str, limit: usize, observed: u64) -> AdkError {
        AdkError::new(
            ErrorComponent::Session,
            ErrorCategory::Internal,
            "session.vertex.response_too_large",
            format!(
                "vertex {context} exceeded the configured decoded response limit of {limit} bytes (observed at least {observed} bytes)",
            ),
        )
        .with_provider("vertex_ai")
    }

    fn request_too_large(context: &str, limit: usize, observed: u64) -> AdkError {
        AdkError::new(
            ErrorComponent::Session,
            ErrorCategory::InvalidInput,
            "session.vertex.request_too_large",
            format!(
                "vertex {context} exceeded the configured encoded request-body limit of {limit} bytes (observed at least {observed} bytes)",
            ),
        )
        .with_provider("vertex_ai")
    }

    fn encode_request_body<T: Serialize + ?Sized>(
        &self,
        value: &T,
        context: &str,
    ) -> Result<Vec<u8>> {
        serialize_json_bounded(value, self.max_request_bytes, context)
    }

    fn add_paginated_response_bytes(
        &self,
        retained: usize,
        page: usize,
        context: &str,
    ) -> Result<usize> {
        let observed = retained
            .checked_add(page)
            .ok_or_else(|| Self::response_too_large(context, self.max_response_bytes, u64::MAX))?;
        if observed > self.max_response_bytes {
            return Err(Self::response_too_large(
                context,
                self.max_response_bytes,
                observed as u64,
            ));
        }
        Ok(observed)
    }

    fn pagination_deadline(&self) -> Result<Instant> {
        Instant::now().checked_add(self.pagination_timeout).ok_or_else(|| {
            Self::invalid_input("Vertex pagination timeout is too large for the system clock")
        })
    }

    fn pagination_timeout_error(&self, context: &str) -> AdkError {
        Self::timeout_error(format!(
            "vertex {context} pagination did not complete within {} seconds",
            self.pagination_timeout.as_secs_f64(),
        ))
    }

    fn ensure_pagination_deadline(&self, deadline: Instant, context: &str) -> Result<()> {
        if Instant::now() >= deadline {
            return Err(self.pagination_timeout_error(context));
        }
        Ok(())
    }

    fn mutation_outcome_error(
        mut error: AdkError,
        operation: &str,
        code: &'static str,
        reconciliation: &str,
        operation_name: Option<&str>,
        force_ambiguous: bool,
    ) -> AdkError {
        let ambiguous = force_ambiguous
            || match error.details.upstream_status_code {
                Some(200..=299 | 408 | 500..=599) | None => true,
                Some(_) => false,
            };
        if ambiguous {
            let cause = truncate_for_error(&error.message);
            error.code = code;
            let operation_reference = operation_name
                .map(|name| format!(" Google Cloud operation '{}'. ", truncate_for_error(name)));
            error.message = format!(
                "vertex {operation} outcome is ambiguous after request transmission or operation polling: {cause}.{}{reconciliation}",
                operation_reference.as_deref().unwrap_or(" "),
            );
            error.retry = RetryHint { should_retry: false, ..Default::default() };
        }
        error
    }

    fn lro_mutation_outcome_error(
        error: AdkError,
        operation: &str,
        code: &'static str,
        reconciliation: &str,
        operation_name: &str,
    ) -> AdkError {
        if error.code == "session.vertex.operation_failed" {
            return error;
        }
        Self::mutation_outcome_error(
            error,
            operation,
            code,
            reconciliation,
            Some(operation_name),
            true,
        )
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
        if candidate.trim().is_empty() {
            return Err(Self::invalid_input(
                "a reasoning engine numeric ID or full resource name is required",
            ));
        }

        if is_canonical_reasoning_engine_id(candidate) {
            validate_vertex_resource_segment(candidate, "reasoning_engine")?;
            return Ok(candidate.to_string());
        }

        let prefix =
            format!("projects/{}/locations/{}/reasoningEngines/", self.project_id, self.location,);
        if candidate.starts_with(&prefix)
            && let Some(reasoning_engine) =
                extract_reasoning_engine_id_from_resource_name(candidate)
        {
            return Ok(reasoning_engine);
        }

        let candidate = truncate_for_error(candidate);
        Err(Self::invalid_input(format!(
            "reasoning engine '{candidate}' is invalid. Provide a numeric ID or the exact resource name '{prefix}<numeric-id>'",
        )))
    }

    fn session_parent(&self, app_name: &str) -> Result<String> {
        validate_vertex_resource_segment(&self.project_id, "project_id")?;
        validate_vertex_resource_segment(&self.location, "location")?;
        let reasoning_engine = self.resolve_reasoning_engine_id(app_name)?;
        Ok(format!(
            "projects/{}/locations/{}/reasoningEngines/{reasoning_engine}",
            self.project_id, self.location,
        ))
    }

    fn session_name_from_app(&self, app_name: &str, session_id: &str) -> Result<String> {
        validate_vertex_session_resource_id(session_id)?;

        Ok(format!("{}/sessions/{session_id}", self.session_parent(app_name)?))
    }

    fn allows_unmarked_sessions_for_app(&self, app_name: &str) -> bool {
        self.reasoning_engine.is_none() || self.unmarked_session_app.as_deref() == Some(app_name)
    }

    async fn remember_session_scope(&self, session_id: &str, app_name: &str, user_id: &str) {
        let mut scopes = self.session_scopes.write().await;
        let scope = SessionScope { app_name: app_name.to_string(), user_id: user_id.to_string() };
        if let Some(index) = scopes
            .iter()
            .position(|(cached_id, cached_scope)| cached_id == session_id && *cached_scope == scope)
        {
            scopes.remove(index);
        }
        while scopes.len() >= VERTEX_SESSION_SCOPE_CACHE_CAPACITY {
            scopes.pop_front();
        }
        scopes.push_back((session_id.to_string(), scope));
    }

    async fn forget_session_scope(&self, session_id: &str, app_name: &str, user_id: &str) {
        let mut scopes = self.session_scopes.write().await;
        scopes.retain(|(cached_id, scope)| {
            cached_id != session_id || scope.app_name != app_name || scope.user_id != user_id
        });
    }

    async fn resolve_session_scope_for_append(&self, session_id: &str) -> Result<SessionScope> {
        let scope = {
            let scopes = self.session_scopes.read().await;
            let mut candidates = scopes
                .iter()
                .filter(|(cached_id, _)| cached_id == session_id)
                .map(|(_, scope)| scope.clone())
                .collect::<Vec<_>>();
            if candidates.is_empty() {
                let session_id = truncate_for_error(session_id);
                return Err(Self::invalid_input(format!(
                    "session '{session_id}' is not in the vertex session scope cache. Call create/get/list first.",
                )));
            }

            candidates.sort_by(|left, right| {
                (&left.app_name, &left.user_id).cmp(&(&right.app_name, &right.user_id))
            });
            candidates.dedup();

            if candidates.len() != 1 {
                let session_id = truncate_for_error(session_id);
                return Err(Self::invalid_input(format!(
                    "session_id '{session_id}' is ambiguous across Vertex app/user scopes; use append_event_for_identity",
                )));
            }
            candidates.remove(0)
        };

        Ok(scope)
    }

    async fn wait_for_operation(
        &self,
        initial: Value,
        operation_kind: &str,
        require_response: bool,
    ) -> Result<Option<Value>> {
        self.wait_for_operation_with_timeout(
            initial,
            operation_kind,
            require_response,
            OPERATION_POLL_TIMEOUT,
        )
        .await
    }

    async fn wait_for_operation_with_timeout(
        &self,
        initial: Value,
        operation_kind: &str,
        require_response: bool,
        timeout: Duration,
    ) -> Result<Option<Value>> {
        // The session backend requires exact-shape operation names
        // (`projects/*/locations/*/operations/*` with validated segments) —
        // stricter than the shared poller's prefix scope check, so it runs
        // first. Identity pinning inside the poller extends the guarantee
        // to every subsequent poll.
        if let Some(name) = initial.get("name").and_then(Value::as_str) {
            validate_vertex_operation_name(name, &self.project_id, &self.location)?;
        }
        LroPoller::new()
            .with_poll_timeout(timeout)
            .with_initial_delay(OPERATION_POLL_INITIAL_DELAY)
            .with_max_delay(OPERATION_POLL_MAX_DELAY)
            .wait_for_operation(
                &self.client,
                initial,
                operation_kind,
                require_response,
                &self.project_id,
                &self.location,
            )
            .await
    }

    async fn fetch_session(&self, session_name: &str) -> Result<Option<VertexSessionPayload>> {
        let request = self.client.request(Method::GET, session_name).await?;
        let value = match self.client.send_value_allow_not_found(request).await? {
            Some(value) => value,
            None => return Ok(None),
        };

        let session: VertexSessionPayload = serde_json::from_value(value).map_err(|error| {
            let error = truncate_for_error(&error.to_string());
            Self::session_error(format!("failed to parse vertex session payload: {error}"))
        })?;
        validate_vertex_session_payload(&session, "get session response")?;
        if session.name != session_name {
            let returned_name = truncate_for_error(&session.name);
            return Err(Self::session_error(format!(
                "vertex get session returned resource '{returned_name}', expected '{session_name}'",
            )));
        }

        Ok(Some(session))
    }

    async fn fetch_session_for_identity(
        &self,
        app_name: &str,
        user_id: &str,
        session_id: &str,
    ) -> Result<Option<(String, VertexSessionPayload)>> {
        let remote_session_id = vertex_remote_session_id(app_name, user_id, session_id);
        let session_name = self.session_name_from_app(app_name, &remote_session_id)?;
        let expected = vertex_session_identity(app_name, user_id, session_id);

        if let Some(payload) = self.fetch_session(&session_name).await? {
            if !verify_vertex_session_identity(
                &payload,
                &remote_session_id,
                &expected,
                "session response",
            )? {
                return Err(Self::session_error(
                    "vertex computed session resource contains a mismatched identity marker",
                ));
            }

            if self.allows_unmarked_sessions_for_app(app_name)
                && is_valid_vertex_session_resource_id(session_id)
            {
                let direct_name = self.session_name_from_app(app_name, session_id)?;
                if direct_name != session_name
                    && let Some(direct) = self.fetch_session(&direct_name).await?
                {
                    if session_id.starts_with("adk1-") {
                        return Err(Self::session_error(
                            "vertex direct session resource uses the reserved computed-ID namespace",
                        ));
                    }
                    if vertex_session_identity_from_state(
                        &direct.session_state,
                        "direct session response",
                    )?
                    .is_some()
                    {
                        return Err(Self::session_error(
                            "vertex direct session resource contains an identity marker; marked sessions must use their computed remote ID",
                        ));
                    }
                    if direct.user_id == user_id {
                        return Err(Self::session_error(format!(
                            "vertex identity is ambiguous between computed resource '{session_name}' and legacy resource '{direct_name}'",
                        )));
                    }
                }
            }

            return Ok(Some((session_name, payload)));
        }

        if !self.allows_unmarked_sessions_for_app(app_name)
            || !is_valid_vertex_session_resource_id(session_id)
        {
            return Ok(None);
        }

        let direct_name = self.session_name_from_app(app_name, session_id)?;
        let Some(payload) = self.fetch_session(&direct_name).await? else {
            return Ok(None);
        };
        if session_id.starts_with("adk1-") {
            return Err(Self::session_error(
                "vertex direct session resource uses the reserved computed-ID namespace",
            ));
        }
        if vertex_session_identity_from_state(&payload.session_state, "direct session response")?
            .is_some()
        {
            return Err(Self::session_error(
                "vertex direct session resource contains an identity marker; marked sessions must use their computed remote ID",
            ));
        }
        if payload.user_id != user_id {
            return Ok(None);
        }
        Ok(Some((direct_name, payload)))
    }

    async fn list_session_events(
        &self,
        session_name: &str,
        num_recent_events: Option<usize>,
        after: Option<DateTime<Utc>>,
    ) -> Result<Vec<Event>> {
        if num_recent_events == Some(0) {
            return Ok(Vec::new());
        }

        let mut events = Vec::new();
        let mut page_token: Option<String> = None;
        let mut seen_page_tokens = HashSet::new();
        let mut seen_event_ids = HashSet::new();
        let deadline = self.pagination_deadline()?;
        let mut retained_response_bytes = 0;
        let descending = num_recent_events.is_some();
        let after_filter = after
            .as_ref()
            .map(|timestamp| serde_json::to_string(&timestamp.to_rfc3339()))
            .transpose()
            .map_err(|error| {
                Self::session_error(format!(
                    "failed to encode Vertex event timestamp filter: {error}"
                ))
            })?
            .map(|timestamp| format!("timestamp>={timestamp}"));
        let mut previous_timestamp: Option<DateTime<Utc>> = None;

        'pages: loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(self.pagination_timeout_error("list events"));
            }
            let page_size = num_recent_events
                .map(|limit| limit.saturating_sub(events.len()).min(VERTEX_PAGE_SIZE))
                .unwrap_or(VERTEX_PAGE_SIZE);
            let (value, page_bytes) = tokio::time::timeout(remaining, async {
                let mut request = self
                    .client
                    .request(Method::GET, &format!("{session_name}/events"))
                    .await?
                    .query(&[("pageSize", page_size)]);
                if let Some(filter) = after_filter.as_ref() {
                    request = request.query(&[("filter", filter)]);
                }
                if descending {
                    request = request.query(&[("orderBy", "timestamp desc")]);
                }
                if let Some(token) = page_token.as_ref().filter(|token| !token.is_empty()) {
                    request = request.query(&[("pageToken", token)]);
                }
                self.client.send_value_counted(request).await
            })
            .await
            .map_err(|_| self.pagination_timeout_error("list events"))??;
            retained_response_bytes = self.add_paginated_response_bytes(
                retained_response_bytes,
                page_bytes,
                "list-events pagination",
            )?;
            let response: VertexListEventsResponse =
                serde_json::from_value(value).map_err(|error| {
                    let error = truncate_for_error(&error.to_string());
                    Self::session_error(format!(
                        "failed to parse vertex list-events response: {error}"
                    ))
                })?;
            self.ensure_pagination_deadline(deadline, "list events")?;
            validate_vertex_page_token(&response.next_page_token, "list events")?;
            if !response.next_page_token.is_empty()
                && seen_page_tokens.contains(&response.next_page_token)
            {
                let token = truncate_for_error(&response.next_page_token);
                return Err(Self::session_error(format!(
                    "vertex list events repeated page token '{token}'; refusing an infinite pagination loop",
                )));
            }

            for event in response.session_events {
                self.ensure_pagination_deadline(deadline, "list events")?;
                if event.name.is_empty() {
                    return Err(Self::session_error(
                        "vertex list events returned a SessionEvent without the required resource name",
                    ));
                }
                let prefix = format!("{session_name}/events/");
                let event_id = event.name.strip_prefix(&prefix).ok_or_else(|| {
                    let event_name = truncate_for_error(&event.name);
                    Self::session_error(format!(
                        "vertex list events returned resource '{event_name}' outside requested session '{session_name}'",
                    ))
                })?;
                validate_vertex_upstream_resource_segment(event_id, "event_id")?;
                if !seen_event_ids.insert(event_id.to_string()) {
                    let event_id = truncate_for_error(event_id);
                    return Err(Self::session_error(format!(
                        "vertex list events returned duplicate event ID '{event_id}' across pages",
                    )));
                }
                let event = event.try_into_event()?;
                if let Some(after) = after.as_ref()
                    && event.timestamp < *after
                {
                    return Err(Self::session_error(format!(
                        "vertex list events returned timestamp '{}' before requested lower bound '{}'",
                        event.timestamp.to_rfc3339(),
                        after.to_rfc3339(),
                    )));
                }
                if let Some(previous) = previous_timestamp.as_ref() {
                    let out_of_order = if descending {
                        event.timestamp > *previous
                    } else {
                        event.timestamp < *previous
                    };
                    if out_of_order {
                        let order = if descending { "descending" } else { "ascending" };
                        return Err(Self::session_error(format!(
                            "vertex list events violated the requested {order} timestamp order",
                        )));
                    }
                }
                previous_timestamp = Some(event.timestamp);
                events.push(event);

                if num_recent_events.is_some_and(|limit| events.len() >= limit) {
                    break 'pages;
                }
            }

            if response.next_page_token.is_empty() {
                break;
            }
            if !seen_page_tokens.insert(response.next_page_token.clone()) {
                let token = truncate_for_error(&response.next_page_token);
                return Err(Self::session_error(format!(
                    "vertex list events repeated page token '{token}'; refusing an infinite pagination loop",
                )));
            }
            page_token = Some(response.next_page_token);
        }

        if descending {
            events.reverse();
        }
        self.ensure_pagination_deadline(deadline, "list events")?;
        Ok(events)
    }
}

#[async_trait]
impl SessionService for VertexAiSessionService {
    async fn create(&self, req: CreateRequest) -> Result<Box<dyn Session>> {
        if req.app_name.trim().is_empty() || req.user_id.trim().is_empty() {
            let app_name = truncate_for_error(&req.app_name);
            let user_id = truncate_for_error(&req.user_id);
            return Err(Self::invalid_input(format!(
                "app_name and user_id are required, got app_name: '{app_name}' user_id: '{user_id}'",
            )));
        }
        req.try_app_name()?;
        req.try_user_id()?;
        req.try_session_id()?;
        validate_vertex_user_id(&req.user_id)?;

        let session_id =
            req.session_id.clone().unwrap_or_else(|| SessionId::generate().to_string());
        let remote_session_id = vertex_remote_session_id(&req.app_name, &req.user_id, &session_id);
        validate_vertex_create_session_id(&remote_session_id)?;
        let identity = vertex_session_identity(&req.app_name, &req.user_id, &session_id);
        let mut stored_state = sanitize_state_map(req.state);
        validate_no_vertex_identity_state_key(&stored_state)?;
        insert_vertex_session_identity(&mut stored_state, &identity)?;
        validate_vertex_struct_map(&stored_state, "sessionState")?;
        let parent = self.session_parent(&req.app_name)?;
        let body = VertexCreateSession {
            user_id: req.user_id.clone(),
            session_state: Some(stored_state),
            ttl: self.session_ttl.clone(),
            expire_time: self.session_expire_time.clone(),
        };
        let body = self.encode_request_body(&body, "create-session request")?;

        if self.allows_unmarked_sessions_for_app(&req.app_name)
            && is_valid_vertex_session_resource_id(&session_id)
        {
            let direct_name = self.session_name_from_app(&req.app_name, &session_id)?;
            let computed_name = self.session_name_from_app(&req.app_name, &remote_session_id)?;
            if direct_name != computed_name
                && let Some(direct) = self.fetch_session(&direct_name).await?
            {
                if vertex_session_identity_from_state(
                    &direct.session_state,
                    "direct create collision response",
                )?
                .is_some()
                    || session_id.starts_with("adk1-")
                {
                    return Err(Self::session_error(
                        "vertex direct create collision is marked or uses the reserved computed-ID namespace",
                    ));
                }
                if direct.user_id == req.user_id {
                    return Err(Self::invalid_input(format!(
                        "legacy vertex session '{session_id}' already exists for the requested app and user",
                    )));
                }
            }
        }

        let request = self
            .client
            .request(Method::POST, &format!("{parent}/sessions"))
            .await?
            .header(CONTENT_TYPE, "application/json")
            .body(body)
            .query(&[("sessionId", &remote_session_id)]);
        let operation = match self.client.send_value(request).await {
            Ok(operation) => operation,
            Err(error) if error.details.upstream_status_code == Some(409) => {
                let session_name = self.session_name_from_app(&req.app_name, &remote_session_id)?;
                let Some(existing) = self.fetch_session(&session_name).await? else {
                    return Err(Self::session_error(
                        "vertex create returned AlreadyExists but the computed resource could not be verified",
                    ));
                };
                if verify_vertex_session_identity(
                    &existing,
                    &remote_session_id,
                    &identity,
                    "existing create resource",
                )? {
                    return Err(error);
                }
                return Err(Self::session_error(
                    "vertex create remote ID collides with a foreign or mismatched session resource",
                ));
            }
            Err(error) => {
                return Err(Self::mutation_outcome_error(
                    error,
                    "create session",
                    "session.vertex.create_outcome_ambiguous",
                    "Inspect the target session and Google Cloud operation before any manual retry",
                    None,
                    false,
                ));
            }
        };
        let operation_name = operation
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| "<unknown operation>".to_string());
        let response = self
            .wait_for_operation(operation, "create session", true)
            .await
            .map_err(|error| {
                Self::lro_mutation_outcome_error(
                    error,
                    "create session",
                    "session.vertex.create_outcome_ambiguous",
                    "Inspect the target session and Google Cloud operation before any manual retry",
                    &operation_name,
                )
            })?
            .ok_or_else(|| {
                Self::session_error(
                    "vertex create session operation completed without the required Session response",
                )
            })
            .map_err(|error| {
                Self::lro_mutation_outcome_error(
                    error,
                    "create session",
                    "session.vertex.create_outcome_ambiguous",
                    "Inspect the target session and Google Cloud operation before any manual retry",
                    &operation_name,
                )
            })?;
        let (payload, updated_at) = (|| {
            let payload = parse_create_session_operation_response(response)?;
            validate_vertex_session_payload(&payload, "create session response")?;
            let returned_remote_id =
                session_id_from_session_name(&payload.name).ok_or_else(|| {
                    let payload_name = truncate_for_error(&payload.name);
                    Self::session_error(format!(
                        "vertex create session operation returned an invalid session resource name '{payload_name}'",
                    ))
                })?;
            validate_vertex_upstream_session_resource_id(
                &returned_remote_id,
                "create session response",
            )?;
            if payload.name != format!("{parent}/sessions/{returned_remote_id}") {
                let payload_name = truncate_for_error(&payload.name);
                return Err(Self::session_error(format!(
                    "vertex create session operation returned resource '{payload_name}' outside requested parent '{parent}'",
                )));
            }
            if returned_remote_id != remote_session_id {
                return Err(Self::session_error(format!(
                    "vertex create session returned remote session_id '{returned_remote_id}', but '{remote_session_id}' was requested",
                )));
            }
            if payload.user_id != req.user_id {
                let user_id = truncate_for_error(&payload.user_id);
                return Err(Self::session_error(format!(
                    "vertex create session returned user_id '{user_id}', but '{}' was requested",
                    req.user_id,
                )));
            }
            if !verify_vertex_session_identity(
                &payload,
                &returned_remote_id,
                &identity,
                "create session response",
            )? {
                return Err(Self::session_error(
                    "vertex create session response did not preserve the requested identity marker",
                ));
            }
            let updated_at = session_update_timestamp(&payload, "create session response")?;
            Ok((payload, updated_at))
        })()
        .map_err(|error| {
            Self::lro_mutation_outcome_error(
                error,
                "create session",
                "session.vertex.create_outcome_ambiguous",
                "Inspect the target session and Google Cloud operation before any manual retry",
                &operation_name,
            )
        })?;
        let state = public_vertex_session_state(payload.session_state);

        self.remember_session_scope(&session_id, &req.app_name, &req.user_id).await;

        Ok(Box::new(VertexSession {
            app_name: req.app_name,
            user_id: req.user_id,
            session_id,
            state,
            events: Vec::new(),
            updated_at,
        }))
    }

    async fn get(&self, req: GetRequest) -> Result<Box<dyn Session>> {
        req.try_identity()?;
        validate_vertex_user_id(&req.user_id)?;

        let (session_name, payload) = self
            .fetch_session_for_identity(&req.app_name, &req.user_id, &req.session_id)
            .await?
            .ok_or_else(|| crate::service::session_not_found(&req))?;

        self.remember_session_scope(&req.session_id, &req.app_name, &req.user_id).await;

        let events =
            self.list_session_events(&session_name, req.num_recent_events, req.after).await?;

        let updated_at = session_update_timestamp(&payload, "get session response")?;

        Ok(Box::new(VertexSession {
            app_name: req.app_name,
            user_id: req.user_id,
            session_id: req.session_id,
            state: public_vertex_session_state(payload.session_state),
            events,
            updated_at,
        }))
    }

    async fn list(&self, req: ListRequest) -> Result<Vec<Box<dyn Session>>> {
        if req.app_name.trim().is_empty() || req.user_id.trim().is_empty() {
            return Err(Self::invalid_input(
                "app_name and user_id are required and must be non-empty",
            ));
        }
        req.try_app_name()?;
        req.try_user_id()?;
        validate_vertex_user_id(&req.user_id)?;
        let parent = self.session_parent(&req.app_name)?;
        let mut sessions = Vec::new();
        if req.limit == Some(0) {
            return Ok(sessions);
        }
        let limit = req.limit.unwrap_or(usize::MAX);
        let mut remaining_offset = req.offset.unwrap_or(0);
        let mut page_token: Option<String> = None;
        let mut seen_page_tokens = HashSet::new();
        let mut seen_session_ids = HashSet::new();
        let deadline = self.pagination_deadline()?;
        let mut retained_response_bytes = 0;

        'pages: loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(self.pagination_timeout_error("list sessions"));
            }
            let (value, page_bytes) = tokio::time::timeout(remaining, async {
                let mut request = self
                    .client
                    .request(Method::GET, &format!("{parent}/sessions"))
                    .await?
                    .query(&[("pageSize", VERTEX_PAGE_SIZE)]);

                let filter = vertex_user_filter(&req.user_id)?;
                request = request.query(&[("filter", filter)]);
                if let Some(token) = page_token.as_ref().filter(|token| !token.is_empty()) {
                    request = request.query(&[("pageToken", token)]);
                }
                self.client.send_value_counted(request).await
            })
            .await
            .map_err(|_| self.pagination_timeout_error("list sessions"))??;
            retained_response_bytes = self.add_paginated_response_bytes(
                retained_response_bytes,
                page_bytes,
                "list-sessions pagination",
            )?;
            let response: VertexListSessionsResponse =
                serde_json::from_value(value).map_err(|error| {
                    let error = truncate_for_error(&error.to_string());
                    Self::session_error(format!(
                        "failed to parse vertex list-sessions response: {error}"
                    ))
                })?;
            self.ensure_pagination_deadline(deadline, "list sessions")?;
            validate_vertex_page_token(&response.next_page_token, "list sessions")?;
            if !response.next_page_token.is_empty()
                && seen_page_tokens.contains(&response.next_page_token)
            {
                let token = truncate_for_error(&response.next_page_token);
                return Err(Self::session_error(format!(
                    "vertex list sessions repeated page token '{token}'; refusing an infinite pagination loop",
                )));
            }

            for payload in response.sessions {
                self.ensure_pagination_deadline(deadline, "list sessions")?;
                validate_vertex_session_payload(&payload, "list sessions response")?;
                let remote_session_id =
                    session_id_from_session_name(&payload.name).ok_or_else(|| {
                        let payload_name = truncate_for_error(&payload.name);
                        Self::session_error(format!(
                            "failed to parse session id from vertex session resource name '{payload_name}'",
                        ))
                    })?;
                validate_vertex_upstream_session_resource_id(
                    &remote_session_id,
                    "list sessions response",
                )?;
                if payload.name != format!("{parent}/sessions/{remote_session_id}") {
                    let payload_name = truncate_for_error(&payload.name);
                    return Err(Self::session_error(format!(
                        "vertex list sessions returned resource '{payload_name}' outside requested parent '{parent}'",
                    )));
                }
                if payload.user_id != req.user_id {
                    continue;
                }
                let identity = match vertex_session_identity_from_state(
                    &payload.session_state,
                    "list sessions response",
                )? {
                    Some(identity) => {
                        if payload.user_id != identity.user_id {
                            return Err(Self::session_error(
                                "vertex list sessions response userId does not match its identity marker",
                            ));
                        }
                        if vertex_remote_session_id(
                            &identity.app_name,
                            &identity.user_id,
                            &identity.session_id,
                        ) != remote_session_id
                        {
                            return Err(Self::session_error(
                                "vertex list sessions response resource ID does not match its identity marker",
                            ));
                        }
                        if identity.app_name != req.app_name || identity.user_id != req.user_id {
                            continue;
                        }
                        identity
                    }
                    None => {
                        if !self.allows_unmarked_sessions_for_app(&req.app_name) {
                            continue;
                        }
                        if remote_session_id.starts_with("adk1-") {
                            return Err(Self::session_error(
                                "vertex list sessions returned an unmarked resource in the reserved computed-ID namespace",
                            ));
                        }
                        SessionId::try_from(remote_session_id.as_str()).map_err(|error| {
                            Self::session_error(format!(
                                "vertex list sessions returned an invalid unmarked logical session ID: {error}",
                            ))
                        })?;
                        vertex_session_identity(&req.app_name, &req.user_id, &remote_session_id)
                    }
                };
                if !seen_session_ids.insert(identity.session_id.clone()) {
                    return Err(Self::session_error(format!(
                        "vertex list sessions returned duplicate logical session_id '{}'",
                        identity.session_id,
                    )));
                }

                if remaining_offset > 0 {
                    remaining_offset -= 1;
                    continue;
                }
                if sessions.len() < limit {
                    self.remember_session_scope(
                        &identity.session_id,
                        &identity.app_name,
                        &identity.user_id,
                    )
                    .await;

                    let updated_at = session_update_timestamp(&payload, "list sessions response")?;

                    sessions.push(Box::new(VertexSession {
                        app_name: identity.app_name,
                        user_id: identity.user_id,
                        session_id: identity.session_id,
                        state: public_vertex_session_state(payload.session_state),
                        events: Vec::new(),
                        updated_at,
                    }) as Box<dyn Session>);
                    if sessions.len() == limit {
                        break 'pages;
                    }
                }
            }
            if response.next_page_token.is_empty() {
                break;
            }
            if !seen_page_tokens.insert(response.next_page_token.clone()) {
                let token = truncate_for_error(&response.next_page_token);
                return Err(Self::session_error(format!(
                    "vertex list sessions repeated page token '{token}'; refusing an infinite pagination loop",
                )));
            }
            page_token = Some(response.next_page_token);
        }

        self.ensure_pagination_deadline(deadline, "list sessions")?;
        Ok(sessions)
    }

    async fn delete(&self, req: DeleteRequest) -> Result<()> {
        req.try_identity()?;
        validate_vertex_user_id(&req.user_id)?;

        let Some((session_name, _payload)) =
            self.fetch_session_for_identity(&req.app_name, &req.user_id, &req.session_id).await?
        else {
            self.forget_session_scope(&req.session_id, &req.app_name, &req.user_id).await;
            return Err(crate::service::session_not_found(&GetRequest {
                app_name: req.app_name,
                user_id: req.user_id,
                session_id: req.session_id,
                num_recent_events: None,
                after: None,
            }));
        };
        let request = self.client.request(Method::DELETE, &session_name).await?;
        if let Some(operation) =
            self.client.send_value_allow_not_found(request).await.map_err(|error| {
                Self::mutation_outcome_error(
                    error,
                    "delete session",
                    "session.vertex.delete_outcome_ambiguous",
                    "Inspect the target session and Google Cloud operation before any manual retry",
                    None,
                    false,
                )
            })?
        {
            let operation_name = operation
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| "<unknown operation>".to_string());
            let response = self
                .wait_for_operation(operation, "delete session", true)
                .await
                .map_err(|error| {
                    Self::lro_mutation_outcome_error(
                        error,
                        "delete session",
                        "session.vertex.delete_outcome_ambiguous",
                        "Inspect the target session and Google Cloud operation before any manual retry",
                        &operation_name,
                    )
                })?
                .ok_or_else(|| {
                    Self::session_error(
                        "vertex delete session operation completed without the required Empty response",
                    )
                })
                .map_err(|error| {
                    Self::lro_mutation_outcome_error(
                        error,
                        "delete session",
                        "session.vertex.delete_outcome_ambiguous",
                        "Inspect the target session and Google Cloud operation before any manual retry",
                        &operation_name,
                    )
                })?;
            validate_delete_operation_response(&response).map_err(|error| {
                Self::lro_mutation_outcome_error(
                    error,
                    "delete session",
                    "session.vertex.delete_outcome_ambiguous",
                    "Inspect the target session and Google Cloud operation before any manual retry",
                    &operation_name,
                )
            })?;
        }

        self.forget_session_scope(&req.session_id, &req.app_name, &req.user_id).await;

        Ok(())
    }

    async fn append_event(&self, session_id: &str, mut event: Event) -> Result<()> {
        if session_id.trim().is_empty() {
            return Err(Self::invalid_input("session_id is required for append_event"));
        }
        SessionId::try_from(session_id)?;

        event.actions.state_delta = sanitize_state_map(event.actions.state_delta);
        validate_no_vertex_identity_state_key(&event.actions.state_delta)?;
        let body = build_append_event_payload_with_limit(&event, self.max_request_bytes)?;
        let body = self.encode_request_body(&body, "append-event request")?;

        let scope = self.resolve_session_scope_for_append(session_id).await?;
        let (session_name, _) = self
            .fetch_session_for_identity(&scope.app_name, &scope.user_id, session_id)
            .await?
            .ok_or_else(|| {
                crate::service::session_not_found(&GetRequest {
                    app_name: scope.app_name,
                    user_id: scope.user_id,
                    session_id: session_id.to_string(),
                    num_recent_events: None,
                    after: None,
                })
            })?;
        let request = self
            .client
            .request(Method::POST, &format!("{session_name}:appendEvent"))
            .await?
            .header(CONTENT_TYPE, "application/json")
            .body(body);
        let response = self.client.send_value(request).await.map_err(|error| {
            Self::mutation_outcome_error(
                error,
                "append event",
                "session.vertex.append_outcome_ambiguous",
                "Inspect or list the session events before any manual retry to avoid duplicates",
                None,
                false,
            )
        })?;
        validate_empty_response(&response, "append event").map_err(|error| {
            Self::mutation_outcome_error(
                error,
                "append event",
                "session.vertex.append_outcome_ambiguous",
                "Inspect or list the session events before any manual retry to avoid duplicates",
                None,
                true,
            )
        })?;

        Ok(())
    }

    async fn append_event_for_identity(&self, req: AppendEventRequest) -> Result<()> {
        let mut event = req.event;

        let app_name = req.identity.app_name.as_ref();
        let session_id = req.identity.session_id.as_ref();

        if session_id.trim().is_empty() {
            return Err(Self::invalid_input("session_id is required for append_event"));
        }
        validate_vertex_user_id(req.identity.user_id.as_ref())?;

        event.actions.state_delta = sanitize_state_map(event.actions.state_delta);
        validate_no_vertex_identity_state_key(&event.actions.state_delta)?;
        let body = build_append_event_payload_with_limit(&event, self.max_request_bytes)?;
        let body = self.encode_request_body(&body, "append-event request")?;

        let (session_name, _) = self
            .fetch_session_for_identity(app_name, req.identity.user_id.as_ref(), session_id)
            .await?
            .ok_or_else(|| {
                crate::service::session_not_found(&GetRequest {
                    app_name: app_name.to_string(),
                    user_id: req.identity.user_id.to_string(),
                    session_id: session_id.to_string(),
                    num_recent_events: None,
                    after: None,
                })
            })?;

        let request = self
            .client
            .request(Method::POST, &format!("{session_name}:appendEvent"))
            .await?
            .header(CONTENT_TYPE, "application/json")
            .body(body);
        let response = self.client.send_value(request).await.map_err(|error| {
            Self::mutation_outcome_error(
                error,
                "append event",
                "session.vertex.append_outcome_ambiguous",
                "Inspect or list the session events before any manual retry to avoid duplicates",
                None,
                false,
            )
        })?;
        validate_empty_response(&response, "append event").map_err(|error| {
            Self::mutation_outcome_error(
                error,
                "append event",
                "session.vertex.append_outcome_ambiguous",
                "Inspect or list the session events before any manual retry to avoid duplicates",
                None,
                true,
            )
        })?;

        // Remember the scope so that subsequent legacy calls can also resolve.
        self.remember_session_scope(session_id, app_name, req.identity.user_id.as_ref()).await;

        Ok(())
    }
}

struct VertexSession {
    app_name: String,
    user_id: String,
    session_id: String,
    state: HashMap<String, Value>,
    events: Vec<Event>,
    updated_at: DateTime<Utc>,
}

impl Session for VertexSession {
    fn id(&self) -> &str {
        &self.session_id
    }

    fn app_name(&self) -> &str {
        &self.app_name
    }

    fn user_id(&self) -> &str {
        &self.user_id
    }

    fn state(&self) -> &dyn State {
        self
    }

    fn events(&self) -> &dyn Events {
        self
    }

    fn last_update_time(&self) -> DateTime<Utc> {
        self.updated_at
    }
}

impl State for VertexSession {
    fn get(&self, key: &str) -> Option<Value> {
        self.state.get(key).cloned()
    }

    fn set(&mut self, key: String, value: Value) {
        if let Err(msg) = adk_core::validate_state_key(&key) {
            tracing::warn!(key = %key, "rejecting invalid state key: {msg}");
            return;
        }
        self.state.insert(key, value);
    }

    fn all(&self) -> HashMap<String, Value> {
        self.state.clone()
    }
}

impl Events for VertexSession {
    fn all(&self) -> Vec<Event> {
        self.events.clone()
    }

    fn len(&self) -> usize {
        self.events.len()
    }

    fn at(&self, index: usize) -> Option<&Event> {
        self.events.get(index)
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct VertexCreateSession {
    user_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_state: Option<HashMap<String, Value>>,
    // `ttl` and `expireTime` are the Session `expiration` oneof members in
    // google/cloud/aiplatform/v1beta1/session.proto. `ttl` is an input-only
    // JSON duration string (e.g. "86400s") with a 24-hour minimum;
    // `expireTime` is an RFC 3339 timestamp. At most one is ever set.
    #[serde(skip_serializing_if = "Option::is_none")]
    ttl: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expire_time: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VertexSessionPayload {
    name: String,
    #[serde(default)]
    user_id: String,
    #[serde(default)]
    session_state: HashMap<String, Value>,
    #[serde(default)]
    create_time: Option<String>,
    #[serde(default)]
    update_time: Option<String>,
}

fn validate_vertex_session_payload(payload: &VertexSessionPayload, context: &str) -> Result<()> {
    validate_vertex_upstream_user_id(&payload.user_id, context)?;
    validate_vertex_upstream_struct_map(&payload.session_state, &format!("{context}.sessionState"))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VertexListSessionsResponse {
    #[serde(default)]
    sessions: Vec<VertexSessionPayload>,
    #[serde(default)]
    next_page_token: String,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct VertexListEventsResponse {
    #[serde(default, rename = "sessionEvents", alias = "events")]
    session_events: Vec<VertexEventPayload>,
    #[serde(default)]
    next_page_token: String,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct VertexEventPayload {
    #[serde(default)]
    name: String,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    invocation_id: String,
    #[serde(default)]
    author: String,
    #[serde(default)]
    content: Option<VertexContentPayload>,
    #[serde(default)]
    raw_event: Option<Map<String, Value>>,
    #[serde(default)]
    actions: VertexEventActionsPayload,
    #[serde(default)]
    event_metadata: VertexEventMetadataPayload,
    #[serde(default)]
    error_code: Option<String>,
    #[serde(default)]
    error_message: Option<String>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RawContentSource {
    Canonical,
    Raw,
    None,
}

impl VertexEventPayload {
    fn try_into_event(self) -> Result<Event> {
        let event_name = self.error_name();
        reject_vertex_event_fields("SessionEvent", &self.extra)?;
        reject_vertex_event_fields("SessionEvent.actions", &self.actions.extra)?;
        reject_vertex_event_fields("SessionEvent.eventMetadata", &self.event_metadata.extra)?;
        validate_no_vertex_identity_state_key_upstream(
            &self.actions.state_delta,
            "SessionEvent.actions.stateDelta",
        )?;
        validate_vertex_upstream_struct_map(
            &self.actions.state_delta,
            "SessionEvent.actions.stateDelta",
        )?;
        if let Some(content) = self.content.as_ref() {
            validate_vertex_content_payload_json_depth(content, "SessionEvent.content")?;
        }
        if let Some(raw_event) = self.raw_event.as_ref() {
            validate_upstream_json_map_depth(raw_event, "SessionEvent.rawEvent")?;
        }
        validate_upstream_json_map_depth(
            &self.event_metadata.custom_metadata,
            "SessionEvent.eventMetadata.customMetadata",
        )?;
        if let Some(configs) = self.actions.requested_auth_configs.as_ref() {
            validate_upstream_json_map_depth(configs, "SessionEvent.actions.requestedAuthConfigs")?;
        }
        for (field, value) in [
            ("groundingMetadata", self.event_metadata.grounding_metadata.as_ref()),
            ("inputTranscription", self.event_metadata.input_transcription.as_ref()),
            ("outputTranscription", self.event_metadata.output_transcription.as_ref()),
        ] {
            if let Some(value) = value {
                validate_lossless_json_depth_with_trust(
                    value,
                    &format!("SessionEvent.eventMetadata.{field}"),
                    VertexStructTrust::Upstream,
                )?;
            }
        }
        if self.invocation_id.trim().is_empty() {
            return Err(VertexAiSessionService::session_error(format!(
                "vertex session event '{}' is missing required invocationId",
                event_name,
            )));
        }
        if self.author.trim().is_empty() {
            return Err(VertexAiSessionService::session_error(format!(
                "vertex session event '{}' is missing required author",
                event_name,
            )));
        }
        let timestamp = self.timestamp.as_deref().and_then(parse_rfc3339_utc).ok_or_else(|| {
            VertexAiSessionService::session_error(format!(
                "vertex session event '{}' has an invalid or missing required timestamp",
                event_name,
            ))
        })?;
        let rust_envelope = self.rust_raw_envelope()?.is_some();
        let canonical_content =
            self.content.as_ref().map(serde_json::to_value).transpose().map_err(|error| {
                VertexAiSessionService::session_error(format!(
                    "failed to preserve vertex canonical content: {error}",
                ))
            })?;
        let preserved_custom_metadata =
            preserved_custom_metadata(&self.event_metadata.custom_metadata, rust_envelope);
        let canonical_extensions = canonical_extensions(&self.actions, &self.event_metadata)?;
        let direct_raw_event = self.raw_event.as_ref().and_then(|raw_event| {
            (!raw_event.is_empty() && !rust_envelope).then_some(raw_event.clone())
        });
        if let Some(content) = canonical_content.as_ref() {
            validate_lossless_json_depth_with_trust(
                content,
                "preserved canonical content",
                VertexStructTrust::Upstream,
            )?;
        }
        validate_upstream_json_map_depth(&preserved_custom_metadata, "preserved custom metadata")?;
        validate_upstream_json_map_depth(&canonical_extensions, "canonical extensions")?;
        if let Some(raw_event) = direct_raw_event.as_ref() {
            validate_upstream_json_map_depth(raw_event, "preserved raw event")?;
        }

        if let Some(mut event) = self.try_raw_adk_event()? {
            event.actions.state_delta = sanitize_state_map(event.actions.state_delta);
            preserve_vertex_metadata(
                &mut event,
                None,
                preserved_custom_metadata,
                canonical_extensions,
                None,
            )?;
            return Ok(event);
        }
        let invocation_id = self.invocation_id;

        let mut event = Event::new(invocation_id.clone());

        if let Some(event_id) = event_id_from_resource_name(&self.name) {
            event.id = event_id;
        }
        event.timestamp = timestamp;

        event.invocation_id = invocation_id;
        event.author = self.author;
        event.branch = self.event_metadata.branch;
        event.llm_response.content = self
            .content
            .map(content_from_vertex)
            .transpose()
            .map_err(|error| Self::content_error(&self.name, error))?;
        event.actions.state_delta = sanitize_state_map(self.actions.state_delta);
        event.actions.artifact_delta = self
            .actions
            .artifact_delta
            .into_iter()
            .map(|(key, value)| (key, i64::from(value)))
            .collect();
        event.actions.skip_summarization = self.actions.skip_summarization;
        event.actions.escalate = self.actions.escalate;
        event.actions.transfer_to_agent =
            (!self.actions.transfer_agent.is_empty()).then_some(self.actions.transfer_agent);
        event.long_running_tool_ids = self.event_metadata.long_running_tool_ids;
        event.llm_response.partial = self.event_metadata.partial;
        event.llm_response.turn_complete = self.event_metadata.turn_complete;
        event.llm_response.interrupted = self.event_metadata.interrupted;
        event.llm_response.error_code = self.error_code;
        event.llm_response.error_message = self.error_message;
        if let Some(raw_event) = direct_raw_event.as_ref()
            && is_google_adk_raw_event(raw_event)
        {
            let mut projected = event.clone();
            match apply_google_adk_raw_event(&mut projected, raw_event) {
                Ok(()) => event = projected,
                Err(error) => {
                    tracing::debug!(
                        error = %error,
                        "ignoring incompatible google ADK rawEvent projection"
                    );
                }
            }
        }
        validate_upstream_event_json_depth(&event)?;
        validate_vertex_upstream_struct_map(
            &event.actions.state_delta,
            "SessionEvent.actions.stateDelta",
        )?;
        preserve_vertex_metadata(
            &mut event,
            direct_raw_event,
            preserved_custom_metadata,
            canonical_extensions,
            canonical_content,
        )?;
        Ok(event)
    }

    fn try_raw_adk_event(&self) -> Result<Option<Event>> {
        let event_name = self.error_name();
        let Some((raw_event, content_source)) = self.rust_raw_envelope()? else {
            return Ok(None);
        };
        let value =
            raw_event.get("adkEvent").expect("recognized Rust rawEvent envelopes contain adkEvent");

        let event_json = value.as_str().ok_or_else(|| {
            VertexAiSessionService::session_error(format!(
                "vertex session event '{}' rawEvent.adkEvent must be a JSON string for lossless numeric persistence",
                event_name,
            ))
        })?;
        let event: Event = serde_json::from_str(event_json).map_err(|error| {
            let error = truncate_for_error(&error.to_string());
            VertexAiSessionService::session_error(format!(
                "vertex session event '{}' contains an invalid rawEvent.adkEvent payload: {error}",
                event_name,
            ))
        })?;
        let mut event = event;
        validate_upstream_event_json_depth(&event)?;
        validate_vertex_upstream_struct_map(
            &event.actions.state_delta,
            "rawEvent.actions.stateDelta",
        )?;
        match content_source {
            RawContentSource::Canonical => {
                if event.llm_response.content.is_some() {
                    return Err(VertexAiSessionService::session_error(format!(
                        "vertex session event '{}' rawEvent declares canonical content but also embeds content",
                        event_name,
                    )));
                }
                let canonical = self.content.clone().ok_or_else(|| {
                    VertexAiSessionService::session_error(format!(
                        "vertex session event '{}' rawEvent declares canonical content but SessionEvent.content is missing",
                        event_name,
                    ))
                })?;
                let mut content = content_from_vertex(canonical)
                    .map_err(|error| Self::content_error(&self.name, error))?;
                content.role = self
                    .event_metadata
                    .custom_metadata
                    .get("adkContentRole")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        VertexAiSessionService::session_error(format!(
                            "vertex session event '{}' rawEvent canonical content is missing adkContentRole",
                            event_name,
                        ))
                    })?
                    .to_string();
                event.llm_response.content = Some(content);
            }
            RawContentSource::Raw => {
                if event.llm_response.content.is_none() || self.content.is_some() {
                    return Err(VertexAiSessionService::session_error(format!(
                        "vertex session event '{}' has an inconsistent raw content envelope",
                        event_name,
                    )));
                }
            }
            RawContentSource::None => {
                if event.llm_response.content.is_some() || self.content.is_some() {
                    return Err(VertexAiSessionService::session_error(format!(
                        "vertex session event '{}' has an inconsistent empty content envelope",
                        event_name,
                    )));
                }
            }
        }
        if event.invocation_id != self.invocation_id {
            return Err(VertexAiSessionService::session_error(format!(
                "vertex session event '{}' rawEvent invocation_id '{}' does not match canonical invocationId '{}'",
                event_name,
                truncate_for_error(&event.invocation_id),
                truncate_for_error(&self.invocation_id),
            )));
        }
        if event.author != self.author {
            return Err(VertexAiSessionService::session_error(format!(
                "vertex session event '{}' rawEvent author '{}' does not match canonical author '{}'",
                event_name,
                truncate_for_error(&event.author),
                truncate_for_error(&self.author),
            )));
        }
        let timestamp = self.timestamp.as_deref().and_then(parse_rfc3339_utc).ok_or_else(|| {
            VertexAiSessionService::session_error(format!(
                "vertex session event '{}' has an invalid or missing timestamp",
                event_name,
            ))
        })?;
        if event.timestamp != timestamp
            || event.branch != self.event_metadata.branch
            || event.long_running_tool_ids != self.event_metadata.long_running_tool_ids
            || event.llm_response.partial != self.event_metadata.partial
            || event.llm_response.turn_complete != self.event_metadata.turn_complete
            || event.llm_response.interrupted != self.event_metadata.interrupted
            || proto3_optional_string(event.llm_response.error_code.as_deref())
                != proto3_optional_string(self.error_code.as_deref())
            || proto3_optional_string(event.llm_response.error_message.as_deref())
                != proto3_optional_string(self.error_message.as_deref())
        {
            return Err(VertexAiSessionService::session_error(format!(
                "vertex session event '{}' rawEvent metadata does not match canonical fields",
                event_name,
            )));
        }
        let event_id =
            self.event_metadata.custom_metadata.get("adkEventId").and_then(Value::as_str).ok_or_else(
                || {
                    VertexAiSessionService::session_error(format!(
                        "vertex session event '{}' Rust rawEvent envelope is missing canonical adkEventId",
                        event_name,
                    ))
                },
            )?;
        if event.id != event_id {
            return Err(VertexAiSessionService::session_error(format!(
                "vertex session event '{}' rawEvent id '{}' does not match canonical adkEventId '{}'",
                event_name,
                truncate_for_error(&event.id),
                truncate_for_error(event_id),
            )));
        }
        if let Some(content_role) =
            self.event_metadata.custom_metadata.get("adkContentRole").and_then(Value::as_str)
            && event.llm_response.content.as_ref().map(|content| content.role.as_str())
                != Some(content_role)
        {
            return Err(VertexAiSessionService::session_error(format!(
                "vertex session event '{}' rawEvent content role does not match canonical adkContentRole",
                event_name,
            )));
        }
        if content_source == RawContentSource::Canonical {
            let raw_content = event.llm_response.content.as_ref().ok_or_else(|| {
                VertexAiSessionService::session_error(format!(
                    "vertex session event '{}' canonical rawEvent content was not restored",
                    event_name,
                ))
            })?;
            let canonical = self.content.as_ref().ok_or_else(|| {
                VertexAiSessionService::session_error(format!(
                    "vertex session event '{}' omits canonical content required by rawEvent",
                    event_name,
                ))
            })?;
            let mut canonical = content_from_vertex(canonical.clone())
                .map_err(|error| Self::content_error(&self.name, error))?;
            canonical.role = raw_content.role.clone();
            let expected = serde_json::to_value(raw_content).map_err(|error| {
                VertexAiSessionService::session_error(format!(
                    "failed to validate vertex rawEvent content: {error}"
                ))
            })?;
            let canonical = serde_json::to_value(canonical).map_err(|error| {
                VertexAiSessionService::session_error(format!(
                    "failed to validate vertex canonical content: {error}"
                ))
            })?;
            if !vertex_json_semantically_equal(&expected, &canonical) {
                return Err(VertexAiSessionService::session_error(format!(
                    "vertex session event '{}' rawEvent content does not match canonical content",
                    event_name,
                )));
            }
        }
        validate_vertex_upstream_struct_map(
            &event.actions.state_delta,
            "rawEvent.actions.stateDelta",
        )?;
        let raw_state = Value::Object(
            sanitize_state_map(event.actions.state_delta.clone()).into_iter().collect(),
        );
        let canonical_state = Value::Object(
            sanitize_state_map(self.actions.state_delta.clone()).into_iter().collect(),
        );
        if !vertex_json_semantically_equal(&raw_state, &canonical_state) {
            return Err(VertexAiSessionService::session_error(format!(
                "vertex session event '{}' rawEvent state_delta does not match canonical actions.stateDelta",
                event_name,
            )));
        }
        let canonical_artifacts = self
            .actions
            .artifact_delta
            .iter()
            .map(|(key, value)| (key.clone(), i64::from(*value)))
            .collect::<HashMap<_, _>>();
        let raw_artifacts_fit_vertex =
            event.actions.artifact_delta.values().all(|value| i32::try_from(*value).is_ok());
        let artifacts_match = if raw_artifacts_fit_vertex {
            event.actions.artifact_delta == canonical_artifacts
        } else {
            canonical_artifacts.is_empty()
        };
        if !artifacts_match
            || event.actions.skip_summarization != self.actions.skip_summarization
            || event.actions.escalate != self.actions.escalate
            || proto3_optional_string(event.actions.transfer_to_agent.as_deref())
                != (!self.actions.transfer_agent.is_empty())
                    .then_some(self.actions.transfer_agent.as_str())
        {
            return Err(VertexAiSessionService::session_error(format!(
                "vertex session event '{}' rawEvent actions do not match canonical actions",
                event_name,
            )));
        }

        validate_upstream_event_json_depth(&event)?;
        validate_vertex_upstream_struct_map(
            &event.actions.state_delta,
            "rawEvent.actions.stateDelta",
        )?;
        Ok(Some(event))
    }

    fn rust_raw_envelope(&self) -> Result<Option<(&Map<String, Value>, RawContentSource)>> {
        let event_name = self.error_name();
        let Some(raw_event) = self.raw_event.as_ref() else {
            return Ok(None);
        };
        let Some(value) = raw_event.get(RUST_RAW_EVENT_ENVELOPE_KEY) else {
            return Ok(None);
        };
        let envelope = value.as_object().ok_or_else(|| {
            VertexAiSessionService::session_error(format!(
                "vertex session event '{}' contains a non-object {RUST_RAW_EVENT_ENVELOPE_KEY} envelope",
                event_name,
            ))
        })?;
        let schema_version = envelope.get("schemaVersion").and_then(Value::as_number);
        if !schema_version
            .is_some_and(|version| version.as_u64() == Some(1) || version.as_f64() == Some(1.0))
        {
            return Err(VertexAiSessionService::session_error(format!(
                "vertex session event '{}' contains an unsupported or missing Rust rawEvent schemaVersion; expected 1",
                event_name,
            )));
        }
        if !envelope.get("adkEvent").is_some_and(Value::is_string) {
            return Err(VertexAiSessionService::session_error(format!(
                "vertex session event '{}' Rust rawEvent envelope must contain an adkEvent JSON string",
                event_name,
            )));
        }
        let source = match envelope.get("contentSource").and_then(Value::as_str) {
            Some("canonical") => RawContentSource::Canonical,
            Some("raw") => RawContentSource::Raw,
            Some("none") => RawContentSource::None,
            _ => {
                return Err(VertexAiSessionService::session_error(format!(
                    "vertex session event '{}' Rust rawEvent envelope has an unsupported or missing contentSource",
                    event_name,
                )));
            }
        };
        Ok(Some((envelope, source)))
    }

    fn error_name(&self) -> String {
        if self.name.is_empty() { "<unnamed>".to_string() } else { truncate_for_error(&self.name) }
    }

    fn content_error(event_name: &str, error: AdkError) -> AdkError {
        let event_name = if event_name.is_empty() {
            "<unnamed>".to_string()
        } else {
            truncate_for_error(event_name)
        };
        let message = truncate_for_error(&error.message);
        VertexAiSessionService::session_error(format!(
            "failed to restore content for vertex session event '{}': {}",
            event_name, message,
        ))
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct VertexEventActionsPayload {
    #[serde(default)]
    state_delta: HashMap<String, Value>,
    #[serde(default)]
    artifact_delta: HashMap<String, i32>,
    #[serde(default)]
    skip_summarization: bool,
    #[serde(default)]
    escalate: bool,
    #[serde(default)]
    requested_auth_configs: Option<Map<String, Value>>,
    #[serde(default)]
    transfer_agent: String,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct VertexEventMetadataPayload {
    #[serde(default)]
    branch: String,
    #[serde(default)]
    partial: bool,
    #[serde(default)]
    turn_complete: bool,
    #[serde(default)]
    interrupted: bool,
    #[serde(default)]
    long_running_tool_ids: Vec<String>,
    #[serde(default)]
    grounding_metadata: Option<Value>,
    #[serde(default)]
    input_transcription: Option<Value>,
    #[serde(default)]
    output_transcription: Option<Value>,
    #[serde(default)]
    custom_metadata: Map<String, Value>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VertexContentPayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    #[serde(default)]
    parts: Vec<VertexPartPayload>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VertexPartPayload {
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    inline_data: Option<VertexInlineData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    file_data: Option<VertexFileData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    function_call: Option<VertexFunctionCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    function_response: Option<VertexFunctionResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thought: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thought_signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    video_metadata: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    media_resolution: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    executable_code: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    code_execution_result: Option<Value>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VertexInlineData {
    mime_type: String,
    data: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    display_name: Option<String>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VertexFileData {
    mime_type: String,
    file_uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    display_name: Option<String>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct VertexFunctionCall {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    args: Option<Value>,
    #[serde(default, rename = "partialArgs", skip_serializing_if = "Option::is_none")]
    partial_args: Option<Vec<Value>>,
    #[serde(default, rename = "willContinue", skip_serializing_if = "Option::is_none")]
    will_continue: Option<bool>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct VertexFunctionResponse {
    name: String,
    response: Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    parts: Vec<VertexFunctionResponsePart>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VertexFunctionResponsePart {
    #[serde(skip_serializing_if = "Option::is_none")]
    inline_data: Option<VertexInlineData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    file_data: Option<VertexFileData>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

fn validate_vertex_content_payload_json_depth(
    content: &VertexContentPayload,
    path: &str,
) -> Result<()> {
    validate_upstream_json_map_depth(&content.extra, &format!("{path}.extra"))?;
    for (index, part) in content.parts.iter().enumerate() {
        let part_path = format!("{path}.parts[{index}]");
        validate_upstream_json_map_depth(&part.extra, &format!("{part_path}.extra"))?;
        for (field, value) in [
            ("videoMetadata", part.video_metadata.as_ref()),
            ("mediaResolution", part.media_resolution.as_ref()),
            ("executableCode", part.executable_code.as_ref()),
            ("codeExecutionResult", part.code_execution_result.as_ref()),
        ] {
            if let Some(value) = value {
                validate_lossless_json_depth_with_trust(
                    value,
                    &format!("{part_path}.{field}"),
                    VertexStructTrust::Upstream,
                )?;
            }
        }
        if let Some(data) = part.inline_data.as_ref() {
            validate_upstream_json_map_depth(
                &data.extra,
                &format!("{part_path}.inlineData.extra"),
            )?;
        }
        if let Some(data) = part.file_data.as_ref() {
            validate_upstream_json_map_depth(&data.extra, &format!("{part_path}.fileData.extra"))?;
        }
        if let Some(call) = part.function_call.as_ref() {
            validate_upstream_json_map_depth(
                &call.extra,
                &format!("{part_path}.functionCall.extra"),
            )?;
            if let Some(args) = call.args.as_ref() {
                validate_lossless_json_depth_with_trust(
                    args,
                    &format!("{part_path}.functionCall.args"),
                    VertexStructTrust::Upstream,
                )?;
            }
            if let Some(partial_args) = call.partial_args.as_ref() {
                for (partial_index, value) in partial_args.iter().enumerate() {
                    validate_lossless_json_depth_with_trust(
                        value,
                        &format!("{part_path}.functionCall.partialArgs[{partial_index}]"),
                        VertexStructTrust::Upstream,
                    )?;
                }
            }
        }
        if let Some(response) = part.function_response.as_ref() {
            validate_upstream_json_map_depth(
                &response.extra,
                &format!("{part_path}.functionResponse.extra"),
            )?;
            validate_lossless_json_depth_with_trust(
                &response.response,
                &format!("{part_path}.functionResponse.response"),
                VertexStructTrust::Upstream,
            )?;
            for (response_index, response_part) in response.parts.iter().enumerate() {
                let response_path = format!("{part_path}.functionResponse.parts[{response_index}]");
                validate_upstream_json_map_depth(
                    &response_part.extra,
                    &format!("{response_path}.extra"),
                )?;
                if let Some(data) = response_part.inline_data.as_ref() {
                    validate_upstream_json_map_depth(
                        &data.extra,
                        &format!("{response_path}.inlineData.extra"),
                    )?;
                }
                if let Some(data) = response_part.file_data.as_ref() {
                    validate_upstream_json_map_depth(
                        &data.extra,
                        &format!("{response_path}.fileData.extra"),
                    )?;
                }
            }
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct GoogleAdkRawActions {
    #[serde(default)]
    state_delta: Option<HashMap<String, Value>>,
    #[serde(default)]
    artifact_delta: Option<HashMap<String, i64>>,
    #[serde(default)]
    skip_summarization: Option<bool>,
    #[serde(default)]
    transfer_to_agent: Option<String>,
    #[serde(default)]
    escalate: Option<bool>,
    #[serde(flatten)]
    _extra: Map<String, Value>,
}

fn validate_empty_response(value: &Value, operation_kind: &str) -> Result<()> {
    if value.as_object().is_some_and(Map::is_empty) {
        return Ok(());
    }
    Err(VertexAiSessionService::session_error(format!(
        "vertex {operation_kind} returned a non-empty response; expected google.protobuf.Empty",
    )))
}

fn validate_delete_operation_response(value: &Value) -> Result<()> {
    const EMPTY_TYPE: &str = "type.googleapis.com/google.protobuf.Empty";
    let valid = value.as_object().is_some_and(|object| {
        object.len() == 1 && object.get("@type").and_then(Value::as_str) == Some(EMPTY_TYPE)
    });
    if valid {
        return Ok(());
    }
    Err(VertexAiSessionService::session_error(format!(
        "vertex delete session operation returned an invalid response; expected an Any-wrapped google.protobuf.Empty with @type '{EMPTY_TYPE}'",
    )))
}

fn parse_create_session_operation_response(mut value: Value) -> Result<VertexSessionPayload> {
    const SESSION_TYPE: &str = "type.googleapis.com/google.cloud.aiplatform.v1.Session";
    let object = value.as_object_mut().ok_or_else(|| {
        VertexAiSessionService::session_error(format!(
            "vertex create session operation returned an invalid response; expected an Any-wrapped Session with @type '{SESSION_TYPE}'",
        ))
    })?;
    if object.remove("@type").and_then(|value| value.as_str().map(str::to_string)).as_deref()
        != Some(SESSION_TYPE)
    {
        return Err(VertexAiSessionService::session_error(format!(
            "vertex create session operation returned an invalid response; expected an Any-wrapped Session with @type '{SESSION_TYPE}'",
        )));
    }
    serde_json::from_value(value).map_err(|error| {
        let error = truncate_for_error(&error.to_string());
        VertexAiSessionService::session_error(format!(
            "vertex create session operation returned an invalid Session response: {error}",
        ))
    })
}

fn is_valid_vertex_resource_segment(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= VERTEX_MAX_RESOURCE_SEGMENT_BYTES
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~'))
}

fn validate_vertex_resource_segment(value: &str, field: &str) -> Result<()> {
    if is_valid_vertex_resource_segment(value) {
        return Ok(());
    }

    let value = truncate_for_error(value);
    Err(VertexAiSessionService::invalid_input(format!(
        "invalid Vertex {field} path segment '{value}': use at most {VERTEX_MAX_RESOURCE_SEGMENT_BYTES} bytes containing only ASCII letters, digits, hyphens, periods, underscores, or tildes",
    )))
}

fn validate_vertex_upstream_resource_segment(value: &str, field: &str) -> Result<()> {
    if is_valid_vertex_resource_segment(value) {
        return Ok(());
    }
    let value = truncate_for_error(value);
    Err(VertexAiSessionService::session_error(format!(
        "vertex response contains an invalid {field} path segment '{value}'",
    )))
}

fn is_valid_vertex_session_resource_id(session_id: &str) -> bool {
    !session_id.is_empty()
        && session_id.len() <= VERTEX_MAX_RESOURCE_SEGMENT_BYTES
        && session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn validate_vertex_session_resource_id(session_id: &str) -> Result<()> {
    if is_valid_vertex_session_resource_id(session_id) {
        return Ok(());
    }
    let session_id = truncate_for_error(session_id);
    Err(VertexAiSessionService::invalid_input(format!(
        "invalid Vertex session_id path segment '{session_id}': use at most {VERTEX_MAX_RESOURCE_SEGMENT_BYTES} bytes containing only ASCII letters, digits, hyphens, or underscores",
    )))
}

fn validate_vertex_upstream_session_resource_id(session_id: &str, context: &str) -> Result<()> {
    if is_valid_vertex_session_resource_id(session_id) {
        return Ok(());
    }
    let session_id = truncate_for_error(session_id);
    Err(VertexAiSessionService::session_error(format!(
        "vertex {context} contains an invalid session resource ID '{session_id}'",
    )))
}

fn validate_vertex_create_session_id(session_id: &str) -> Result<()> {
    let bytes = session_id.as_bytes();
    let valid = (1..=63).contains(&bytes.len())
        && bytes.first().is_some_and(u8::is_ascii_lowercase)
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-');

    if valid {
        return Ok(());
    }

    Err(VertexAiSessionService::invalid_input(format!(
        "invalid Vertex session_id '{session_id}': use 1-63 lowercase letters, digits, or hyphens; start with a letter and end with a letter or digit",
    )))
}

fn validate_vertex_user_id(user_id: &str) -> Result<()> {
    if user_id.chars().count() <= VERTEX_MAX_USER_ID_CHARS {
        return Ok(());
    }
    Err(VertexAiSessionService::invalid_input(format!(
        "Vertex user_id exceeds the maximum of {VERTEX_MAX_USER_ID_CHARS} Unicode characters",
    )))
}

fn validate_vertex_upstream_user_id(user_id: &str, context: &str) -> Result<()> {
    UserId::try_from(user_id).map_err(|error| {
        VertexAiSessionService::session_error(format!(
            "vertex {context} contains an invalid userId: {error}",
        ))
    })?;
    if user_id.chars().count() > VERTEX_MAX_USER_ID_CHARS {
        return Err(VertexAiSessionService::session_error(format!(
            "vertex {context} userId exceeds the maximum of {VERTEX_MAX_USER_ID_CHARS} Unicode characters",
        )));
    }
    Ok(())
}

fn validate_vertex_page_token(page_token: &str, context: &str) -> Result<()> {
    if page_token.len() <= VERTEX_MAX_PAGE_TOKEN_BYTES {
        return Ok(());
    }
    Err(VertexAiSessionService::session_error(format!(
        "vertex {context} returned a page token larger than the {VERTEX_MAX_PAGE_TOKEN_BYTES}-byte limit",
    )))
}

fn validate_vertex_operation_name(
    operation_name: &str,
    project_id: &str,
    location: &str,
) -> Result<()> {
    validate_vertex_resource_segment(project_id, "project_id")?;
    validate_vertex_resource_segment(location, "location")?;
    let prefix = format!("projects/{project_id}/locations/{location}/operations/");
    let operation_id = operation_name.strip_prefix(&prefix).ok_or_else(|| {
        let operation_name = truncate_for_error(operation_name);
        VertexAiSessionService::session_error(format!(
            "vertex operation resource '{operation_name}' is outside configured project '{project_id}' and location '{location}'",
        ))
    })?;
    validate_vertex_upstream_resource_segment(operation_id, "operation_id")?;
    if operation_name != format!("{prefix}{operation_id}") {
        let operation_name = truncate_for_error(operation_name);
        return Err(VertexAiSessionService::session_error(format!(
            "invalid Vertex operation resource name '{operation_name}'",
        )));
    }
    Ok(())
}

// JSON representation of google.protobuf.Duration: decimal seconds with an
// "s" suffix (e.g. "86400s"), fractional digits only when nonzero.
fn proto_duration_string(duration: Duration) -> String {
    let nanos = duration.subsec_nanos();
    if nanos == 0 {
        format!("{}s", duration.as_secs())
    } else {
        let fraction = format!("{nanos:09}");
        let fraction = fraction.trim_end_matches('0');
        format!("{}.{fraction}s", duration.as_secs())
    }
}

fn proto3_optional_string(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.is_empty())
}

fn content_to_vertex(content: &Content) -> Result<VertexContentPayload> {
    if content.parts.is_empty() {
        return Err(VertexAiSessionService::invalid_input(
            "ADK content has no parts; Vertex AI requires at least one Content part",
        ));
    }
    let mut parts = Vec::with_capacity(content.parts.len());

    for (index, part) in content.parts.iter().enumerate() {
        let payload = match part {
            Part::Thinking { thinking, signature } => {
                if signature.as_deref() == Some("") {
                    return Err(VertexAiSessionService::invalid_input(format!(
                        "ADK Thinking content part {index} has an empty thought signature; the event must use rawEvent persistence because proto3 JSON may omit empty bytes",
                    )));
                }
                let thought_signature = signature
                    .as_deref()
                    .map(|signature| {
                        normalize_outbound_base64(
                            signature,
                            &format!("content part {index} thought signature"),
                        )
                    })
                    .transpose()?;
                VertexPartPayload {
                    text: Some(thinking.clone()),
                    thought: Some(true),
                    thought_signature,
                    ..VertexPartPayload::default()
                }
            }
            Part::Text { text } => {
                VertexPartPayload { text: Some(text.clone()), ..VertexPartPayload::default() }
            }
            Part::InlineData { mime_type, data, uri, annotations } => {
                if uri.is_some() || annotations.is_some() {
                    return Err(VertexAiSessionService::invalid_input(format!(
                        "ADK InlineData content part {index} has metadata that the Vertex AI GA v1 wire type cannot represent; the event must use rawEvent persistence",
                    )));
                }
                if mime_type.trim().is_empty() {
                    return Err(VertexAiSessionService::invalid_input(format!(
                        "ADK InlineData content part {index} has an empty MIME type",
                    )));
                }
                VertexPartPayload {
                    inline_data: Some(VertexInlineData {
                        mime_type: mime_type.clone(),
                        data: BASE64_STANDARD.encode(data),
                        display_name: None,
                        extra: Map::new(),
                    }),
                    ..VertexPartPayload::default()
                }
            }
            Part::FileData { mime_type, file_uri, annotations } => {
                if annotations.is_some() {
                    return Err(VertexAiSessionService::invalid_input(format!(
                        "ADK FileData content part {index} has annotations that the Vertex AI GA v1 wire type cannot represent; the event must use rawEvent persistence",
                    )));
                }
                if mime_type.trim().is_empty() || file_uri.trim().is_empty() {
                    return Err(VertexAiSessionService::invalid_input(format!(
                        "ADK FileData content part {index} requires a non-empty MIME type and URI",
                    )));
                }
                VertexPartPayload {
                    file_data: Some(VertexFileData {
                        mime_type: mime_type.clone(),
                        file_uri: file_uri.clone(),
                        display_name: None,
                        extra: Map::new(),
                    }),
                    ..VertexPartPayload::default()
                }
            }
            Part::FunctionCall { name, args, id, thought_signature } => {
                if id.is_some() {
                    return Err(VertexAiSessionService::invalid_input(format!(
                        "ADK FunctionCall content part {index} has an id, but the Vertex AI GA v1 FunctionCall wire type has no id field; the event must use rawEvent persistence",
                    )));
                }
                if thought_signature.as_deref() == Some("") {
                    return Err(VertexAiSessionService::invalid_input(format!(
                        "ADK FunctionCall content part {index} has an empty thought signature; the event must use rawEvent persistence because proto3 JSON may omit empty bytes",
                    )));
                }
                if !args.is_object() {
                    return Err(VertexAiSessionService::invalid_input(format!(
                        "ADK FunctionCall content part {index} has non-object args; Vertex AI requires google.protobuf.Struct arguments",
                    )));
                }
                validate_vertex_struct_value(
                    args,
                    &format!("content.parts[{index}].functionCall.args"),
                )?;
                let thought_signature = thought_signature
                    .as_deref()
                    .map(|signature| {
                        normalize_outbound_base64(
                            signature,
                            &format!("content part {index} function-call thought signature"),
                        )
                    })
                    .transpose()?;
                VertexPartPayload {
                    function_call: Some(VertexFunctionCall {
                        name: (!name.is_empty()).then(|| name.clone()),
                        args: Some(args.clone()),
                        partial_args: None,
                        will_continue: None,
                        extra: Map::new(),
                    }),
                    thought_signature,
                    ..VertexPartPayload::default()
                }
            }
            Part::FunctionResponse { function_response, id, annotations } => {
                if id.is_some() {
                    return Err(VertexAiSessionService::invalid_input(format!(
                        "ADK FunctionResponse content part {index} has an id, but the Vertex AI GA v1 FunctionResponse wire type has no id field; the event must use rawEvent persistence",
                    )));
                }
                if annotations.is_some()
                    || function_response
                        .inline_data
                        .iter()
                        .any(|part| part.uri.is_some() || part.annotations.is_some())
                    || function_response.file_data.iter().any(|part| part.annotations.is_some())
                {
                    return Err(VertexAiSessionService::invalid_input(format!(
                        "ADK FunctionResponse content part {index} has metadata that the Vertex AI GA v1 wire type cannot represent; the event must use rawEvent persistence",
                    )));
                }
                if function_response.name.trim().is_empty() {
                    return Err(VertexAiSessionService::invalid_input(format!(
                        "ADK FunctionResponse content part {index} has an empty function name",
                    )));
                }
                if !function_response.response.is_object() {
                    return Err(VertexAiSessionService::invalid_input(format!(
                        "ADK FunctionResponse content part {index} has a non-object response; Vertex AI requires a google.protobuf.Struct response",
                    )));
                }
                validate_vertex_struct_value(
                    &function_response.response,
                    &format!("content.parts[{index}].functionResponse.response"),
                )?;
                if function_response.inline_data.iter().any(|part| part.mime_type.trim().is_empty())
                    || function_response.file_data.iter().any(|part| {
                        part.mime_type.trim().is_empty() || part.file_uri.trim().is_empty()
                    })
                {
                    return Err(VertexAiSessionService::invalid_input(format!(
                        "ADK FunctionResponse content part {index} contains media without a required MIME type or file URI",
                    )));
                }
                let mut response_parts = Vec::with_capacity(
                    function_response.inline_data.len() + function_response.file_data.len(),
                );
                response_parts.extend(function_response.inline_data.iter().map(|part| {
                    VertexFunctionResponsePart {
                        inline_data: Some(VertexInlineData {
                            mime_type: part.mime_type.clone(),
                            data: BASE64_STANDARD.encode(&part.data),
                            display_name: None,
                            extra: Map::new(),
                        }),
                        ..VertexFunctionResponsePart::default()
                    }
                }));
                response_parts.extend(function_response.file_data.iter().map(|part| {
                    VertexFunctionResponsePart {
                        file_data: Some(VertexFileData {
                            mime_type: part.mime_type.clone(),
                            file_uri: part.file_uri.clone(),
                            display_name: None,
                            extra: Map::new(),
                        }),
                        ..VertexFunctionResponsePart::default()
                    }
                }));

                VertexPartPayload {
                    function_response: Some(VertexFunctionResponse {
                        name: function_response.name.clone(),
                        response: function_response.response.clone(),
                        parts: response_parts,
                        extra: Map::new(),
                    }),
                    ..VertexPartPayload::default()
                }
            }
            Part::ServerToolCall { .. } => {
                return Err(unsupported_content_part(index, "ServerToolCall"));
            }
            Part::ServerToolResponse { .. } => {
                return Err(unsupported_content_part(index, "ServerToolResponse"));
            }
            Part::EmbeddedResource { resource } => {
                let resource_kind = match resource {
                    EmbeddedResource::Text(_) => "text EmbeddedResource",
                    EmbeddedResource::Blob(_) => "binary EmbeddedResource",
                };
                return Err(unsupported_content_part(index, resource_kind));
            }
        };
        parts.push(payload);
    }

    let role = match content.role.as_str() {
        "" => None,
        "model" | "assistant" | "agent" => Some("model"),
        "user" | "function" | "tool" | "system" => Some("user"),
        role => {
            let role = truncate_for_error(role);
            return Err(VertexAiSessionService::invalid_input(format!(
                "ADK content role '{role}' has no Vertex AI Content role mapping",
            )));
        }
    };

    Ok(VertexContentPayload { role: role.map(str::to_string), parts, extra: Map::new() })
}

fn content_from_vertex(content: VertexContentPayload) -> Result<Content> {
    if !content.extra.is_empty() {
        let fields = bounded_field_names(&content.extra);
        return Err(VertexAiSessionService::session_error(format!(
            "vertex content contains unsupported fields [{fields}]; the event was not loaded to avoid losing content",
        )));
    }
    let role = content.role.unwrap_or_default();
    if !role.is_empty() && role != "user" && role != "model" {
        let role = truncate_for_error(&role);
        return Err(VertexAiSessionService::session_error(format!(
            "vertex content has invalid role '{role}'; expected 'user' or 'model'",
        )));
    }
    if content.parts.is_empty() {
        return Err(VertexAiSessionService::session_error(
            "vertex content has no parts; Content.parts is required",
        ));
    }
    let mut parts = Vec::with_capacity(content.parts.len());

    for (index, part) in content.parts.into_iter().enumerate() {
        if !part.extra.is_empty() {
            let fields = bounded_field_names(&part.extra);
            return Err(VertexAiSessionService::session_error(format!(
                "vertex content part {index} contains unsupported fields [{fields}]; the event was not loaded to avoid losing content",
            )));
        }
        if let Some(media_resolution) = part.media_resolution.as_ref() {
            if !media_resolution.is_object() {
                return Err(VertexAiSessionService::session_error(format!(
                    "vertex content part {index} mediaResolution must be an object",
                )));
            }
            validate_lossless_json_depth_with_trust(
                media_resolution,
                &format!("content.parts[{index}].mediaResolution"),
                VertexStructTrust::Upstream,
            )?;
        }

        let variants = usize::from(part.text.is_some())
            + usize::from(part.inline_data.is_some())
            + usize::from(part.file_data.is_some())
            + usize::from(part.function_call.is_some())
            + usize::from(part.function_response.is_some())
            + usize::from(part.executable_code.is_some())
            + usize::from(part.code_execution_result.is_some());
        if variants != 1 {
            return Err(VertexAiSessionService::session_error(format!(
                "vertex content part {index} must contain exactly one supported data field, found {variants}",
            )));
        }

        let video_metadata = part.video_metadata;
        let restored = if let Some(text) = part.text {
            if video_metadata.is_some() {
                return Err(VertexAiSessionService::session_error(format!(
                    "vertex text content part {index} has videoMetadata without video data",
                )));
            }
            if part.thought.unwrap_or(false) {
                if let Some(signature) = part.thought_signature.as_deref() {
                    validate_base64(signature, &format!("content part {index} thought signature"))?;
                }
                Part::Thinking { thinking: text, signature: part.thought_signature }
            } else {
                Part::Text { text }
            }
        } else if let Some(inline_data) = part.inline_data {
            reject_extra_fields(index, "inlineData", &inline_data.extra)?;
            if let Some(signature) = part.thought_signature.as_deref() {
                validate_base64(signature, &format!("content part {index} thought signature"))?;
            }
            if inline_data.mime_type.trim().is_empty() {
                return Err(VertexAiSessionService::session_error(format!(
                    "vertex inlineData content part {index} has an empty required MIME type",
                )));
            }
            Part::InlineData {
                mime_type: inline_data.mime_type,
                data: decode_base64(
                    &inline_data.data,
                    &format!("content part {index} inlineData"),
                )?,
                uri: None,
                annotations: None,
            }
        } else if let Some(file_data) = part.file_data {
            reject_extra_fields(index, "fileData", &file_data.extra)?;
            if let Some(signature) = part.thought_signature.as_deref() {
                validate_base64(signature, &format!("content part {index} thought signature"))?;
            }
            if file_data.mime_type.trim().is_empty() || file_data.file_uri.trim().is_empty() {
                return Err(VertexAiSessionService::session_error(format!(
                    "vertex fileData content part {index} has an empty required MIME type or URI",
                )));
            }
            Part::FileData {
                mime_type: file_data.mime_type,
                file_uri: file_data.file_uri,
                annotations: None,
            }
        } else if let Some(function_call) = part.function_call {
            if video_metadata.is_some() {
                return Err(VertexAiSessionService::session_error(format!(
                    "vertex functionCall part {index} has unrelated videoMetadata",
                )));
            }
            reject_extra_fields(index, "functionCall", &function_call.extra)?;
            validate_vertex_partial_args(function_call.partial_args.as_deref(), index)?;
            let args = function_call.args.unwrap_or_else(|| Value::Object(Map::new()));
            if !args.is_object() {
                return Err(VertexAiSessionService::session_error(format!(
                    "vertex functionCall part {index} has non-object args; expected a google.protobuf.Struct",
                )));
            }
            validate_vertex_upstream_struct_value(
                &args,
                &format!("content.parts[{index}].functionCall.args"),
            )?;
            if let Some(signature) = part.thought_signature.as_deref() {
                validate_base64(
                    signature,
                    &format!("content part {index} function-call thought signature"),
                )?;
            }
            Part::FunctionCall {
                name: function_call.name.unwrap_or_default(),
                args,
                id: None,
                thought_signature: part.thought_signature,
            }
        } else if let Some(function_response) = part.function_response {
            if video_metadata.is_some() {
                return Err(VertexAiSessionService::session_error(format!(
                    "vertex functionResponse part {index} has unrelated videoMetadata",
                )));
            }
            reject_extra_fields(index, "functionResponse", &function_response.extra)?;
            if let Some(signature) = part.thought_signature.as_deref() {
                validate_base64(signature, &format!("content part {index} thought signature"))?;
            }
            if function_response.name.trim().is_empty() {
                return Err(VertexAiSessionService::session_error(format!(
                    "vertex functionResponse part {index} has an empty required name",
                )));
            }
            if !function_response.response.is_object() {
                return Err(VertexAiSessionService::session_error(format!(
                    "vertex functionResponse part {index} has a non-object response; expected a google.protobuf.Struct",
                )));
            }
            validate_vertex_upstream_struct_value(
                &function_response.response,
                &format!("content.parts[{index}].functionResponse.response"),
            )?;
            let mut inline_data = Vec::new();
            let mut file_data = Vec::new();

            for (response_index, response_part) in function_response.parts.into_iter().enumerate() {
                if !response_part.extra.is_empty() {
                    let fields = bounded_field_names(&response_part.extra);
                    return Err(VertexAiSessionService::session_error(format!(
                        "vertex functionResponse part {index}.{response_index} contains unsupported fields [{fields}]",
                    )));
                }

                match (response_part.inline_data, response_part.file_data) {
                    (Some(data), None) => {
                        reject_extra_fields(index, "functionResponse inlineData", &data.extra)?;
                        if data.mime_type.trim().is_empty() {
                            return Err(VertexAiSessionService::session_error(format!(
                                "vertex functionResponse part {index}.{response_index} inlineData has an empty required MIME type",
                            )));
                        }
                        inline_data.push(InlineDataPart {
                            mime_type: data.mime_type,
                            data: decode_base64(
                                &data.data,
                                &format!(
                                    "functionResponse part {index}.{response_index} inlineData"
                                ),
                            )?,
                            uri: None,
                            annotations: None,
                        });
                    }
                    (None, Some(data)) => {
                        reject_extra_fields(index, "functionResponse fileData", &data.extra)?;
                        if data.mime_type.trim().is_empty() || data.file_uri.trim().is_empty() {
                            return Err(VertexAiSessionService::session_error(format!(
                                "vertex functionResponse part {index}.{response_index} fileData has an empty required MIME type or URI",
                            )));
                        }
                        file_data.push(FileDataPart {
                            mime_type: data.mime_type,
                            file_uri: data.file_uri,
                            annotations: None,
                        });
                    }
                    _ => {
                        return Err(VertexAiSessionService::session_error(format!(
                            "vertex functionResponse part {index}.{response_index} must contain exactly one of inlineData or fileData",
                        )));
                    }
                }
            }

            Part::FunctionResponse {
                function_response: FunctionResponseData {
                    name: function_response.name,
                    response: function_response.response,
                    inline_data,
                    file_data,
                },
                id: None,
                annotations: None,
            }
        } else if let Some(executable_code) = part.executable_code {
            if video_metadata.is_some() {
                return Err(VertexAiSessionService::session_error(format!(
                    "vertex executableCode part {index} has unrelated videoMetadata",
                )));
            }
            Part::ServerToolCall {
                server_tool_call: serde_json::json!({
                    "vertexExecutableCode": executable_code,
                    "thought": part.thought,
                    "thoughtSignature": part.thought_signature,
                }),
            }
        } else if let Some(code_execution_result) = part.code_execution_result {
            if video_metadata.is_some() {
                return Err(VertexAiSessionService::session_error(format!(
                    "vertex codeExecutionResult part {index} has unrelated videoMetadata",
                )));
            }
            Part::ServerToolResponse {
                server_tool_response: serde_json::json!({
                    "vertexCodeExecutionResult": code_execution_result,
                    "thought": part.thought,
                    "thoughtSignature": part.thought_signature,
                }),
            }
        } else {
            unreachable!("the supported vertex part count is checked above");
        };

        parts.push(restored);
    }

    Ok(Content { role, parts })
}

fn validate_vertex_partial_args(partial_args: Option<&[Value]>, part_index: usize) -> Result<()> {
    let Some(partial_args) = partial_args else {
        return Ok(());
    };
    for (index, partial_arg) in partial_args.iter().enumerate() {
        let object = partial_arg.as_object().ok_or_else(|| {
            VertexAiSessionService::session_error(format!(
                "vertex functionCall part {part_index} partialArgs[{index}] must be an object",
            ))
        })?;
        let json_path = object.get("jsonPath").and_then(Value::as_str).ok_or_else(|| {
            VertexAiSessionService::session_error(format!(
                "vertex functionCall part {part_index} partialArgs[{index}] requires string jsonPath",
            ))
        })?;
        if json_path.is_empty() {
            return Err(VertexAiSessionService::session_error(format!(
                "vertex functionCall part {part_index} partialArgs[{index}] has empty jsonPath",
            )));
        }
        if object.get("willContinue").is_some_and(|value| !value.is_boolean()) {
            return Err(VertexAiSessionService::session_error(format!(
                "vertex functionCall part {part_index} partialArgs[{index}].willContinue must be a boolean",
            )));
        }
        let value_fields = ["nullValue", "numberValue", "stringValue", "boolValue"];
        if value_fields.iter().filter(|field| object.contains_key(**field)).count() != 1 {
            return Err(VertexAiSessionService::session_error(format!(
                "vertex functionCall part {part_index} partialArgs[{index}] must contain exactly one value field",
            )));
        }
        let valid_value = object
            .get("nullValue")
            .is_some_and(|value| value.is_null() || value.as_str() == Some("NULL_VALUE"))
            || object.get("numberValue").is_some_and(Value::is_number)
            || object.get("stringValue").is_some_and(Value::is_string)
            || object.get("boolValue").is_some_and(Value::is_boolean);
        if !valid_value {
            return Err(VertexAiSessionService::session_error(format!(
                "vertex functionCall part {part_index} partialArgs[{index}] has an invalid value type",
            )));
        }
        if let Some(field) = object.keys().find(|field| {
            !matches!(
                field.as_str(),
                "jsonPath"
                    | "nullValue"
                    | "numberValue"
                    | "stringValue"
                    | "boolValue"
                    | "willContinue"
            )
        }) {
            let field = truncate_for_error(field);
            return Err(VertexAiSessionService::session_error(format!(
                "vertex functionCall part {part_index} partialArgs[{index}] contains unsupported field '{field}'",
            )));
        }
    }
    Ok(())
}

fn reject_extra_fields(index: usize, part_kind: &str, extra: &Map<String, Value>) -> Result<()> {
    if extra.is_empty() {
        return Ok(());
    }
    let fields = bounded_field_names(extra);
    Err(VertexAiSessionService::session_error(format!(
        "vertex {part_kind} content part {index} contains unsupported fields [{fields}]; the event was not loaded to avoid losing content",
    )))
}

fn reject_vertex_event_fields(context: &str, extra: &Map<String, Value>) -> Result<()> {
    if extra.is_empty() {
        return Ok(());
    }
    let fields = bounded_field_names(extra);
    Err(VertexAiSessionService::session_error(format!(
        "vertex {context} contains unsupported fields [{fields}]; the event was not loaded to avoid silent data loss",
    )))
}

fn is_google_adk_raw_event(raw_event: &Map<String, Value>) -> bool {
    raw_event.get("id").and_then(Value::as_str).is_some_and(|id| !id.is_empty())
        && raw_event.get("invocationId").and_then(Value::as_str).is_some()
        && raw_event.get("author").and_then(Value::as_str).is_some()
        && raw_event.get("timestamp").and_then(Value::as_f64).is_some()
}

fn preserved_custom_metadata(
    custom_metadata: &Map<String, Value>,
    rust_envelope: bool,
) -> Map<String, Value> {
    custom_metadata
        .iter()
        .filter(|(key, _)| !rust_envelope || (*key != "adkEventId" && *key != "adkContentRole"))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn canonical_extensions(
    actions: &VertexEventActionsPayload,
    metadata: &VertexEventMetadataPayload,
) -> Result<Map<String, Value>> {
    let mut extensions = Map::new();
    if let Some(requested_auth_configs) = &actions.requested_auth_configs {
        extensions.insert(
            "requestedAuthConfigs".to_string(),
            Value::Object(requested_auth_configs.clone()),
        );
    }
    for (field, value) in [
        ("groundingMetadata", metadata.grounding_metadata.as_ref()),
        ("inputTranscription", metadata.input_transcription.as_ref()),
        ("outputTranscription", metadata.output_transcription.as_ref()),
    ] {
        if let Some(value) = value {
            if !value.is_object() {
                return Err(VertexAiSessionService::session_error(format!(
                    "vertex SessionEvent.eventMetadata.{field} must be an object",
                )));
            }
            extensions.insert(field.to_string(), value.clone());
        }
    }
    Ok(extensions)
}

fn preserve_vertex_metadata(
    event: &mut Event,
    direct_raw_event: Option<Map<String, Value>>,
    custom_metadata: Map<String, Value>,
    canonical_extensions: Map<String, Value>,
    canonical_content: Option<Value>,
) -> Result<()> {
    for (key, value) in [
        (VERTEX_RAW_EVENT_METADATA_KEY, direct_raw_event.map(Value::Object)),
        (
            VERTEX_CUSTOM_METADATA_KEY,
            (!custom_metadata.is_empty()).then_some(Value::Object(custom_metadata)),
        ),
        (
            VERTEX_CANONICAL_EXTENSIONS_KEY,
            (!canonical_extensions.is_empty()).then_some(Value::Object(canonical_extensions)),
        ),
        (VERTEX_CANONICAL_CONTENT_KEY, canonical_content),
    ] {
        if let Some(value) = value {
            let encoded = serde_json::to_string(&value).map_err(|error| {
                VertexAiSessionService::session_error(format!(
                    "failed to preserve vertex event metadata '{key}': {error}",
                ))
            })?;
            event.provider_metadata.insert(key.to_string(), encoded);
        }
    }
    Ok(())
}

fn apply_google_adk_raw_event(event: &mut Event, raw_event: &Map<String, Value>) -> Result<()> {
    if let Some(content) = raw_event.get("content").filter(|value| !value.is_null()) {
        let content: VertexContentPayload =
            serde_json::from_value(content.clone()).map_err(|error| {
                let error = truncate_for_error(&error.to_string());
                VertexAiSessionService::session_error(format!(
                    "google ADK rawEvent contains invalid content: {error}",
                ))
            })?;
        let raw_content = content_from_vertex(content)
            .map_err(|error| VertexEventPayload::content_error("<google-adk-raw-event>", error))?;
        let raw_content = serde_json::to_value(raw_content).map_err(|error| {
            VertexAiSessionService::session_error(format!(
                "failed to compare google ADK rawEvent content: {error}",
            ))
        })?;
        let canonical_content =
            event.llm_response.content.as_ref().map(serde_json::to_value).transpose().map_err(
                |error| {
                    VertexAiSessionService::session_error(format!(
                        "failed to compare canonical SessionEvent content: {error}",
                    ))
                },
            )?;
        if !canonical_content
            .as_ref()
            .is_some_and(|canonical| vertex_json_semantically_equal(canonical, &raw_content))
        {
            return Err(VertexAiSessionService::session_error(
                "google ADK rawEvent content does not match canonical SessionEvent.content",
            ));
        }
    }

    if let Some(actions) = raw_event.get("actions").filter(|value| !value.is_null()) {
        let actions: GoogleAdkRawActions =
            serde_json::from_value(actions.clone()).map_err(|error| {
                let error = truncate_for_error(&error.to_string());
                VertexAiSessionService::session_error(format!(
                    "google ADK rawEvent contains invalid actions: {error}",
                ))
            })?;
        if let Some(state_delta) = actions.state_delta {
            if state_delta.contains_key(VERTEX_IDENTITY_STATE_KEY) {
                return Err(VertexAiSessionService::session_error(
                    "google ADK rawEvent actions.stateDelta contains the reserved Vertex identity key",
                ));
            }
            let raw_state = Value::Object(sanitize_state_map(state_delta).into_iter().collect());
            let canonical_state = Value::Object(
                sanitize_state_map(event.actions.state_delta.clone()).into_iter().collect(),
            );
            if !vertex_json_semantically_equal(&raw_state, &canonical_state) {
                return Err(VertexAiSessionService::session_error(
                    "google ADK rawEvent actions.stateDelta does not match canonical SessionEvent.actions.stateDelta",
                ));
            }
        }
        if let Some(artifact_delta) = actions.artifact_delta
            && artifact_delta != event.actions.artifact_delta
        {
            return Err(VertexAiSessionService::session_error(
                "google ADK rawEvent actions.artifactDelta does not match canonical SessionEvent.actions.artifactDelta",
            ));
        }
        if let Some(skip_summarization) = actions.skip_summarization
            && skip_summarization != event.actions.skip_summarization
        {
            return Err(VertexAiSessionService::session_error(
                "google ADK rawEvent actions.skipSummarization does not match canonical SessionEvent.actions.skipSummarization",
            ));
        }
        if let Some(transfer_to_agent) = actions.transfer_to_agent
            && (!transfer_to_agent.is_empty()).then_some(transfer_to_agent.as_str())
                != event.actions.transfer_to_agent.as_deref()
        {
            return Err(VertexAiSessionService::session_error(
                "google ADK rawEvent actions.transferToAgent does not match canonical SessionEvent.actions.transferAgent",
            ));
        }
        if let Some(escalate) = actions.escalate
            && escalate != event.actions.escalate
        {
            return Err(VertexAiSessionService::session_error(
                "google ADK rawEvent actions.escalate does not match canonical SessionEvent.actions.escalate",
            ));
        }
    }

    if let Some(value) = raw_event.get("partial").filter(|value| !value.is_null()) {
        let raw = value.as_bool().ok_or_else(|| {
            VertexAiSessionService::session_error("google ADK rawEvent.partial must be a boolean")
        })?;
        if raw != event.llm_response.partial {
            return Err(VertexAiSessionService::session_error(
                "google ADK rawEvent.partial does not match canonical SessionEvent metadata",
            ));
        }
    }
    if let Some(value) = raw_event.get("turnComplete").filter(|value| !value.is_null()) {
        let raw = value.as_bool().ok_or_else(|| {
            VertexAiSessionService::session_error(
                "google ADK rawEvent.turnComplete must be a boolean",
            )
        })?;
        if raw != event.llm_response.turn_complete {
            return Err(VertexAiSessionService::session_error(
                "google ADK rawEvent.turnComplete does not match canonical SessionEvent metadata",
            ));
        }
    }
    if let Some(value) = raw_event.get("interrupted").filter(|value| !value.is_null()) {
        let raw = value.as_bool().ok_or_else(|| {
            VertexAiSessionService::session_error(
                "google ADK rawEvent.interrupted must be a boolean",
            )
        })?;
        if raw != event.llm_response.interrupted {
            return Err(VertexAiSessionService::session_error(
                "google ADK rawEvent.interrupted does not match canonical SessionEvent metadata",
            ));
        }
    }
    if let Some(value) = raw_event.get("branch").filter(|value| !value.is_null()) {
        let raw = value.as_str().ok_or_else(|| {
            VertexAiSessionService::session_error("google ADK rawEvent.branch must be a string")
        })?;
        if raw != event.branch {
            return Err(VertexAiSessionService::session_error(
                "google ADK rawEvent.branch does not match canonical SessionEvent metadata",
            ));
        }
    }
    if let Some(value) = raw_event.get("longRunningToolIds").filter(|value| !value.is_null()) {
        let raw = value
            .as_array()
            .ok_or_else(|| {
                VertexAiSessionService::session_error(
                    "google ADK rawEvent.longRunningToolIds must be an array",
                )
            })?
            .iter()
            .map(|value| {
                value.as_str().map(str::to_string).ok_or_else(|| {
                    VertexAiSessionService::session_error(
                        "google ADK rawEvent.longRunningToolIds must contain only strings",
                    )
                })
            })
            .collect::<Result<Vec<_>>>()?;
        if raw != event.long_running_tool_ids {
            return Err(VertexAiSessionService::session_error(
                "google ADK rawEvent.longRunningToolIds does not match canonical SessionEvent metadata",
            ));
        }
    }
    if let Some(value) = raw_event.get("errorCode").filter(|value| !value.is_null()) {
        let raw = value.as_str().ok_or_else(|| {
            VertexAiSessionService::session_error("google ADK rawEvent.errorCode must be a string")
        })?;
        if event.llm_response.error_code.as_deref() != Some(raw) {
            return Err(VertexAiSessionService::session_error(
                "google ADK rawEvent.errorCode does not match canonical SessionEvent.errorCode",
            ));
        }
    }
    if let Some(value) = raw_event.get("errorMessage").filter(|value| !value.is_null()) {
        let raw = value.as_str().ok_or_else(|| {
            VertexAiSessionService::session_error(
                "google ADK rawEvent.errorMessage must be a string",
            )
        })?;
        if event.llm_response.error_message.as_deref() != Some(raw) {
            return Err(VertexAiSessionService::session_error(
                "google ADK rawEvent.errorMessage does not match canonical SessionEvent.errorMessage",
            ));
        }
    }
    if let Some(value) = raw_event.get("usageMetadata").filter(|value| !value.is_null()) {
        event.llm_response.usage_metadata = Some(google_usage_metadata(value)?);
    }
    if let Some(value) = raw_event.get("finishReason").and_then(Value::as_str) {
        event.llm_response.finish_reason = Some(match value {
            "STOP" | "Stop" => FinishReason::Stop,
            "MAX_TOKENS" | "MaxTokens" => FinishReason::MaxTokens,
            "SAFETY" | "Safety" => FinishReason::Safety,
            "RECITATION" | "Recitation" => FinishReason::Recitation,
            _ => FinishReason::Other,
        });
    }
    if let Some(value) = raw_event.get("citationMetadata").filter(|value| !value.is_null())
        && let Ok(citation_metadata) = serde_json::from_value::<CitationMetadata>(value.clone())
    {
        event.llm_response.citation_metadata = Some(citation_metadata);
    }
    if let Some(value) = raw_event.get("interactionId").filter(|value| !value.is_null()) {
        event.llm_response.interaction_id = Some(
            value
                .as_str()
                .ok_or_else(|| {
                    VertexAiSessionService::session_error(
                        "google ADK rawEvent.interactionId must be a string",
                    )
                })?
                .to_string(),
        );
    }
    if let Some(value) = raw_event.get("providerMetadata").filter(|value| !value.is_null()) {
        event.llm_response.provider_metadata = Some(value.clone());
    }
    if let Some(value) = raw_event.get("llmRequest").filter(|value| !value.is_null()) {
        event.llm_request = Some(
            value
                .as_str()
                .ok_or_else(|| {
                    VertexAiSessionService::session_error(
                        "google ADK rawEvent.llmRequest must be a string",
                    )
                })?
                .to_string(),
        );
    }
    Ok(())
}

fn google_usage_metadata(value: &Value) -> Result<UsageMetadata> {
    let object = value.as_object().ok_or_else(|| {
        VertexAiSessionService::session_error("google ADK rawEvent.usageMetadata must be an object")
    })?;
    let count = |key: &str| -> Result<i32> {
        let Some(value) = object.get(key) else {
            return Ok(0);
        };
        let value = value.as_i64().ok_or_else(|| {
            VertexAiSessionService::session_error(format!(
                "google ADK rawEvent.usageMetadata.{key} must be an integer",
            ))
        })?;
        i32::try_from(value).map_err(|_| {
            VertexAiSessionService::session_error(format!(
                "google ADK rawEvent.usageMetadata.{key} is outside the i32 range",
            ))
        })
    };
    Ok(UsageMetadata {
        prompt_token_count: count("promptTokenCount")?,
        candidates_token_count: count("candidatesTokenCount")?,
        total_token_count: count("totalTokenCount")?,
        cache_read_input_token_count: object
            .contains_key("cachedContentTokenCount")
            .then(|| count("cachedContentTokenCount"))
            .transpose()?,
        thinking_token_count: object
            .contains_key("thoughtsTokenCount")
            .then(|| count("thoughtsTokenCount"))
            .transpose()?,
        provider_usage: Some(value.clone()),
        ..UsageMetadata::default()
    })
}

fn decode_base64(value: &str, context: &str) -> Result<Vec<u8>> {
    decode_flexible_base64(value).map_err(|error| {
        VertexAiSessionService::session_error(format!(
            "vertex {context} contains invalid base64 data: {error}"
        ))
    })
}

fn validate_base64(value: &str, context: &str) -> Result<()> {
    decode_base64(value, context).map(|_| ())
}

fn normalize_outbound_base64(value: &str, context: &str) -> Result<String> {
    let normalized = decode_flexible_base64(value)
        .map(|bytes| BASE64_STANDARD.encode(bytes))
        .map_err(|error| {
            VertexAiSessionService::invalid_input(format!(
                "ADK {context} contains invalid base64 data: {error}",
            ))
        })?;
    if normalized != value {
        return Err(VertexAiSessionService::invalid_input(format!(
            "ADK {context} uses a non-canonical base64 spelling; the event must use rawEvent persistence to preserve the exact value",
        )));
    }
    Ok(normalized)
}

fn decode_flexible_base64(value: &str) -> std::result::Result<Vec<u8>, base64::DecodeError> {
    BASE64_STANDARD
        .decode(value)
        .or_else(|_| BASE64_STANDARD_NO_PAD.decode(value))
        .or_else(|_| BASE64_URL_SAFE.decode(value))
        .or_else(|_| BASE64_URL_SAFE_NO_PAD.decode(value))
}

fn unsupported_content_part(index: usize, variant: &str) -> AdkError {
    VertexAiSessionService::invalid_input(format!(
        "ADK content part {index} ({variant}) has no lossless Vertex AI Content representation; remove or transform it before appending the event",
    ))
}

#[derive(Clone, Copy)]
enum VertexStructTrust {
    Caller,
    Upstream,
}

enum VertexJsonFrame<'a> {
    Value(&'a Value, usize),
    Array(std::slice::Iter<'a, Value>, usize),
    Object(serde_json::map::Iter<'a>, usize),
}

fn vertex_struct_validation_error(
    trust: VertexStructTrust,
    message: impl Into<String>,
) -> AdkError {
    match trust {
        VertexStructTrust::Caller => VertexAiSessionService::invalid_input(message),
        VertexStructTrust::Upstream => VertexAiSessionService::session_error(message),
    }
}

fn validate_vertex_struct_value(value: &Value, path: &str) -> Result<()> {
    validate_vertex_struct_value_with_trust(value, path, 0, VertexStructTrust::Caller, true)
}

fn validate_vertex_upstream_struct_value(value: &Value, path: &str) -> Result<()> {
    validate_vertex_struct_value_with_trust(value, path, 0, VertexStructTrust::Upstream, true)
}

fn validate_lossless_json_depth(value: &Value, path: &str) -> Result<()> {
    validate_lossless_json_depth_with_trust(value, path, VertexStructTrust::Caller)
}

fn validate_lossless_json_depth_with_trust(
    value: &Value,
    path: &str,
    trust: VertexStructTrust,
) -> Result<()> {
    validate_vertex_struct_value_with_trust(value, path, 0, trust, false)
}

fn validate_upstream_json_map_depth(values: &Map<String, Value>, path: &str) -> Result<()> {
    for (key, value) in values {
        let key = truncate_for_error(key);
        validate_vertex_struct_value_with_trust(
            value,
            &format!("{path}.{key}"),
            1,
            VertexStructTrust::Upstream,
            false,
        )?;
    }
    Ok(())
}

fn validate_vertex_struct_value_with_trust(
    value: &Value,
    path: &str,
    initial_depth: usize,
    trust: VertexStructTrust,
    require_exact_integers: bool,
) -> Result<()> {
    let mut pending = vec![VertexJsonFrame::Value(value, initial_depth)];

    while let Some(frame) = pending.pop() {
        match frame {
            VertexJsonFrame::Value(value, depth) => {
                if depth > VERTEX_VALUE_MAX_DEPTH {
                    return Err(vertex_struct_validation_error(
                        trust,
                        format!(
                            "{path} exceeds the maximum Vertex JSON/Struct nesting depth of {VERTEX_VALUE_MAX_DEPTH}",
                        ),
                    ));
                }
                match value {
                    Value::Array(values) => {
                        pending.push(VertexJsonFrame::Array(values.iter(), depth));
                    }
                    Value::Object(values) => {
                        pending.push(VertexJsonFrame::Object(values.iter(), depth));
                    }
                    Value::Number(number) => {
                        let exact = if let Some(value) = number.as_i64() {
                            value.unsigned_abs() <= VERTEX_MAX_EXACT_INTEGER
                        } else if let Some(value) = number.as_u64() {
                            value <= VERTEX_MAX_EXACT_INTEGER
                        } else {
                            true
                        };
                        if require_exact_integers && !exact {
                            return Err(vertex_struct_validation_error(
                                trust,
                                format!(
                                    "{path} contains integer {number}, which exceeds Vertex Struct's exact range of ±{VERTEX_MAX_EXACT_INTEGER}; encode it as a string to preserve the value",
                                ),
                            ));
                        }
                    }
                    Value::Bool(_) | Value::Null | Value::String(_) => {}
                }
            }
            VertexJsonFrame::Array(mut values, depth) => {
                if let Some(value) = values.next() {
                    pending.push(VertexJsonFrame::Array(values, depth));
                    pending.push(VertexJsonFrame::Value(value, depth + 1));
                }
            }
            VertexJsonFrame::Object(mut values, depth) => {
                if let Some((_, value)) = values.next() {
                    pending.push(VertexJsonFrame::Object(values, depth));
                    pending.push(VertexJsonFrame::Value(value, depth + 1));
                }
            }
        }
    }
    Ok(())
}

fn validate_vertex_struct_map(values: &HashMap<String, Value>, path: &str) -> Result<()> {
    for (key, value) in values {
        let key = truncate_for_error(key);
        validate_vertex_struct_value_with_trust(
            value,
            &format!("{path}.{key}"),
            1,
            VertexStructTrust::Caller,
            true,
        )?;
    }
    Ok(())
}

fn validate_vertex_upstream_struct_map(values: &HashMap<String, Value>, path: &str) -> Result<()> {
    for (key, value) in values {
        let key = truncate_for_error(key);
        validate_vertex_struct_value_with_trust(
            value,
            &format!("{path}.{key}"),
            1,
            VertexStructTrust::Upstream,
            true,
        )?;
    }
    Ok(())
}

fn validate_content_json_depth_with_trust(
    content: &Content,
    path: &str,
    trust: VertexStructTrust,
) -> Result<()> {
    for (index, part) in content.parts.iter().enumerate() {
        let part_path = format!("{path}.parts[{index}]");
        match part {
            Part::FunctionCall { args, .. } => {
                validate_lossless_json_depth_with_trust(
                    args,
                    &format!("{part_path}.functionCall.args"),
                    trust,
                )?;
            }
            Part::FunctionResponse { function_response, annotations, .. } => {
                validate_lossless_json_depth_with_trust(
                    &function_response.response,
                    &format!("{part_path}.functionResponse.response"),
                    trust,
                )?;
                if let Some(annotations) = annotations {
                    validate_lossless_json_depth_with_trust(
                        annotations,
                        &format!("{part_path}.functionResponse.annotations"),
                        trust,
                    )?;
                }
                for (media_index, media) in function_response.inline_data.iter().enumerate() {
                    if let Some(annotations) = &media.annotations {
                        validate_lossless_json_depth_with_trust(
                            annotations,
                            &format!(
                                "{part_path}.functionResponse.inlineData[{media_index}].annotations"
                            ),
                            trust,
                        )?;
                    }
                }
                for (media_index, media) in function_response.file_data.iter().enumerate() {
                    if let Some(annotations) = &media.annotations {
                        validate_lossless_json_depth_with_trust(
                            annotations,
                            &format!(
                                "{part_path}.functionResponse.fileData[{media_index}].annotations"
                            ),
                            trust,
                        )?;
                    }
                }
            }
            Part::ServerToolCall { server_tool_call } => {
                validate_lossless_json_depth_with_trust(
                    server_tool_call,
                    &format!("{part_path}.serverToolCall"),
                    trust,
                )?;
            }
            Part::ServerToolResponse { server_tool_response } => {
                validate_lossless_json_depth_with_trust(
                    server_tool_response,
                    &format!("{part_path}.serverToolResponse"),
                    trust,
                )?;
            }
            Part::InlineData { annotations, .. } | Part::FileData { annotations, .. } => {
                if let Some(annotations) = annotations {
                    validate_lossless_json_depth_with_trust(
                        annotations,
                        &format!("{part_path}.annotations"),
                        trust,
                    )?;
                }
            }
            Part::Thinking { .. } | Part::Text { .. } | Part::EmbeddedResource { .. } => {}
        }
    }
    Ok(())
}

fn validate_event_json_depth(event: &Event) -> Result<()> {
    validate_event_json_depth_with_trust(event, VertexStructTrust::Caller)
}

fn validate_upstream_event_json_depth(event: &Event) -> Result<()> {
    validate_event_json_depth_with_trust(event, VertexStructTrust::Upstream)
}

fn validate_event_json_depth_with_trust(event: &Event, trust: VertexStructTrust) -> Result<()> {
    if let Some(provider_metadata) = event.llm_response.provider_metadata.as_ref() {
        validate_lossless_json_depth_with_trust(
            provider_metadata,
            "llmResponse.providerMetadata",
            trust,
        )?;
    }
    if let Some(provider_usage) =
        event.llm_response.usage_metadata.as_ref().and_then(|usage| usage.provider_usage.as_ref())
    {
        validate_lossless_json_depth_with_trust(
            provider_usage,
            "llmResponse.usageMetadata.providerUsage",
            trust,
        )?;
    }
    if let Some(confirmation) = event.actions.tool_confirmation.as_ref() {
        validate_lossless_json_depth_with_trust(
            &confirmation.args,
            "actions.toolConfirmation.args",
            trust,
        )?;
    }
    if let Some(content) = event.llm_response.content.as_ref() {
        validate_content_json_depth_with_trust(content, "content", trust)?;
    }
    if let Some(compaction) = event.actions.compaction.as_ref() {
        validate_content_json_depth_with_trust(
            &compaction.compacted_content,
            "actions.compaction.compactedContent",
            trust,
        )?;
    }
    Ok(())
}

fn sanitize_state_map(mut state: HashMap<String, Value>) -> HashMap<String, Value> {
    state.retain(|key, _| !key.starts_with(KEY_PREFIX_TEMP));
    state
}

fn vertex_remote_session_id(app_name: &str, user_id: &str, session_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"adk.vertex.session.identity.v1\0");
    for value in [app_name, user_id, session_id] {
        hasher.update((value.len() as u64).to_be_bytes());
        hasher.update(value.as_bytes());
    }
    let digest = hasher.finalize();
    let suffix = digest[..29].iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    format!("adk1-{suffix}")
}

fn vertex_session_identity(
    app_name: &str,
    user_id: &str,
    session_id: &str,
) -> VertexSessionIdentity {
    VertexSessionIdentity {
        schema_version: 1,
        app_name: app_name.to_string(),
        user_id: user_id.to_string(),
        session_id: session_id.to_string(),
    }
}

fn insert_vertex_session_identity(
    state: &mut HashMap<String, Value>,
    identity: &VertexSessionIdentity,
) -> Result<()> {
    if state.contains_key(VERTEX_IDENTITY_STATE_KEY) {
        return Err(VertexAiSessionService::invalid_input(format!(
            "session state key '{VERTEX_IDENTITY_STATE_KEY}' is reserved for Vertex identity isolation",
        )));
    }
    let encoded = serde_json::to_string(identity).map_err(|error| {
        VertexAiSessionService::session_error(format!(
            "failed to encode Vertex session identity marker: {error}",
        ))
    })?;
    state.insert(VERTEX_IDENTITY_STATE_KEY.to_string(), Value::String(encoded));
    Ok(())
}

fn vertex_session_identity_from_state(
    state: &HashMap<String, Value>,
    context: &str,
) -> Result<Option<VertexSessionIdentity>> {
    let Some(value) = state.get(VERTEX_IDENTITY_STATE_KEY) else {
        return Ok(None);
    };
    let encoded = value.as_str().ok_or_else(|| {
        VertexAiSessionService::session_error(format!(
            "vertex {context} has a non-string reserved identity marker",
        ))
    })?;
    let identity: VertexSessionIdentity = serde_json::from_str(encoded).map_err(|error| {
        let error = truncate_for_error(&error.to_string());
        VertexAiSessionService::session_error(format!(
            "vertex {context} has an invalid reserved identity marker: {error}",
        ))
    })?;
    if identity.schema_version != 1
        || identity.app_name.is_empty()
        || identity.user_id.is_empty()
        || identity.session_id.is_empty()
    {
        return Err(VertexAiSessionService::session_error(format!(
            "vertex {context} has an unsupported or incomplete reserved identity marker",
        )));
    }
    AppName::try_from(identity.app_name.as_str()).map_err(|error| {
        VertexAiSessionService::session_error(format!(
            "vertex {context} has an invalid appName in its identity marker: {error}",
        ))
    })?;
    validate_vertex_upstream_user_id(
        identity.user_id.as_str(),
        &format!("{context} identity marker"),
    )?;
    SessionId::try_from(identity.session_id.as_str()).map_err(|error| {
        VertexAiSessionService::session_error(format!(
            "vertex {context} has an invalid sessionId in its identity marker: {error}",
        ))
    })?;
    Ok(Some(identity))
}

fn verify_vertex_session_identity(
    payload: &VertexSessionPayload,
    remote_session_id: &str,
    expected: &VertexSessionIdentity,
    context: &str,
) -> Result<bool> {
    let Some(actual) = vertex_session_identity_from_state(&payload.session_state, context)? else {
        return Ok(false);
    };
    if payload.user_id != actual.user_id {
        return Err(VertexAiSessionService::session_error(format!(
            "vertex {context} userId does not match its reserved identity marker",
        )));
    }
    if vertex_remote_session_id(&actual.app_name, &actual.user_id, &actual.session_id)
        != remote_session_id
    {
        return Err(VertexAiSessionService::session_error(format!(
            "vertex {context} resource ID does not match its reserved identity marker",
        )));
    }
    Ok(actual == *expected)
}

fn public_vertex_session_state(mut state: HashMap<String, Value>) -> HashMap<String, Value> {
    state.remove(VERTEX_IDENTITY_STATE_KEY);
    sanitize_state_map(state)
}

fn validate_no_vertex_identity_state_key(state: &HashMap<String, Value>) -> Result<()> {
    if state.contains_key(VERTEX_IDENTITY_STATE_KEY) {
        return Err(VertexAiSessionService::invalid_input(format!(
            "state key '{VERTEX_IDENTITY_STATE_KEY}' is reserved for Vertex identity isolation",
        )));
    }
    Ok(())
}

fn validate_no_vertex_identity_state_key_upstream(
    state: &HashMap<String, Value>,
    context: &str,
) -> Result<()> {
    if state.contains_key(VERTEX_IDENTITY_STATE_KEY) {
        return Err(VertexAiSessionService::session_error(format!(
            "vertex {context} contains the reserved Vertex identity key",
        )));
    }
    Ok(())
}

fn parse_rfc3339_utc(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value).ok().map(|dt| dt.with_timezone(&Utc))
}

fn session_update_timestamp(
    session: &VertexSessionPayload,
    context: &str,
) -> Result<DateTime<Utc>> {
    if let Some(update_time) = session.update_time.as_deref() {
        return parse_rfc3339_utc(update_time).ok_or_else(|| {
            let update_time = truncate_for_error(update_time);
            VertexAiSessionService::session_error(format!(
                "vertex {context} contains invalid updateTime '{update_time}'",
            ))
        });
    }
    if let Some(create_time) = session.create_time.as_deref() {
        return parse_rfc3339_utc(create_time).ok_or_else(|| {
            let create_time = truncate_for_error(create_time);
            VertexAiSessionService::session_error(format!(
                "vertex {context} contains invalid createTime '{create_time}'",
            ))
        });
    }
    Err(VertexAiSessionService::session_error(format!(
        "vertex {context} is missing both updateTime and createTime",
    )))
}

fn vertex_user_filter(user_id: &str) -> Result<String> {
    let quoted = serde_json::to_string(user_id).map_err(|error| {
        VertexAiSessionService::invalid_input(format!(
            "user_id cannot be encoded as a Vertex filter string: {error}",
        ))
    })?;
    Ok(format!("user_id={quoted}"))
}

fn extract_reasoning_engine_id_from_resource_name(app_name: &str) -> Option<String> {
    let segments = app_name.split('/').collect::<Vec<_>>();
    if segments.len() != 6 {
        return None;
    }

    if segments[0] != "projects" || segments[2] != "locations" || segments[4] != "reasoningEngines"
    {
        return None;
    }

    let engine_id = segments[5];
    if !is_canonical_reasoning_engine_id(engine_id) {
        return None;
    }

    Some(engine_id.to_string())
}

fn is_canonical_reasoning_engine_id(value: &str) -> bool {
    !value.is_empty()
        && value.as_bytes().first().is_some_and(|byte| matches!(byte, b'1'..=b'9'))
        && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn session_id_from_session_name(name: &str) -> Option<String> {
    let marker = "/sessions/";
    let idx = name.rfind(marker)?;
    let remainder = &name[idx + marker.len()..];
    let session_id = remainder.split('/').next()?;
    if session_id.is_empty() {
        return None;
    }
    Some(session_id.to_string())
}

fn event_id_from_resource_name(name: &str) -> Option<String> {
    let marker = "/events/";
    let idx = name.rfind(marker)?;
    let event_id = &name[idx + marker.len()..];
    if event_id.is_empty() {
        return None;
    }
    Some(event_id.to_string())
}

fn event_provider_metadata_map(event: &Event, key: &str) -> Result<Option<Map<String, Value>>> {
    let Some(encoded) = event.provider_metadata.get(key) else {
        return Ok(None);
    };
    let value: Value = serde_json::from_str(encoded).map_err(|error| {
        let error = truncate_for_error(&error.to_string());
        VertexAiSessionService::invalid_input(format!(
            "event provider metadata '{key}' does not contain valid JSON: {error}",
        ))
    })?;
    validate_lossless_json_depth(&value, &format!("event.providerMetadata.{key}"))?;
    value.as_object().cloned().map(Some).ok_or_else(|| {
        VertexAiSessionService::invalid_input(format!(
            "event provider metadata '{key}' must contain a JSON object",
        ))
    })
}

fn build_google_adk_raw_event(
    event: &Event,
    canonical: &Map<String, Value>,
    rust_envelope: Map<String, Value>,
) -> Result<Map<String, Value>> {
    if let Some(mut raw_event) = event_provider_metadata_map(event, VERTEX_RAW_EVENT_METADATA_KEY)?
    {
        raw_event.remove(RUST_RAW_EVENT_ENVELOPE_KEY);
        raw_event.insert(RUST_RAW_EVENT_ENVELOPE_KEY.to_string(), Value::Object(rust_envelope));
        return Ok(raw_event);
    }

    let mut raw_event = Map::new();
    raw_event.insert("id".to_string(), Value::String(event.id.clone()));
    let timestamp =
        serde_json::Number::from_f64(event.timestamp.timestamp_micros() as f64 / 1_000_000.0)
            .ok_or_else(|| {
                VertexAiSessionService::session_error(
                    "event timestamp could not be represented in google ADK rawEvent",
                )
            })?;
    raw_event.insert("timestamp".to_string(), Value::Number(timestamp));
    raw_event.insert("invocationId".to_string(), Value::String(event.invocation_id.clone()));
    raw_event.insert("author".to_string(), Value::String(event.author.clone()));
    if let Some(content) = canonical.get("content")
        && raw_event.get("content").is_none_or(Value::is_null)
    {
        raw_event.insert("content".to_string(), content.clone());
    }

    let mut actions =
        canonical.get("actions").and_then(Value::as_object).cloned().unwrap_or_default();
    if let Some(transfer_agent) = actions.remove("transferAgent") {
        actions.insert("transferToAgent".to_string(), transfer_agent);
    }
    if !actions.is_empty() {
        merge_map_field(&mut raw_event, "actions", Value::Object(actions));
    }
    raw_event.insert("partial".to_string(), Value::Bool(event.llm_response.partial));
    raw_event.insert("turnComplete".to_string(), Value::Bool(event.llm_response.turn_complete));
    raw_event.insert("interrupted".to_string(), Value::Bool(event.llm_response.interrupted));
    raw_event.insert("branch".to_string(), Value::String(event.branch.clone()));
    raw_event.insert(
        "longRunningToolIds".to_string(),
        Value::Array(
            event
                .long_running_tool_ids
                .iter()
                .map(|tool_id| Value::String(tool_id.clone()))
                .collect(),
        ),
    );
    if let Some(error_code) = &event.llm_response.error_code {
        raw_event.insert("errorCode".to_string(), Value::String(error_code.clone()));
    }
    if let Some(error_message) = &event.llm_response.error_message {
        raw_event.insert("errorMessage".to_string(), Value::String(error_message.clone()));
    }
    if let Some(usage) = &event.llm_response.usage_metadata {
        let mut usage_value =
            usage.provider_usage.as_ref().and_then(Value::as_object).cloned().unwrap_or_default();
        usage_value.insert("promptTokenCount".to_string(), Value::from(usage.prompt_token_count));
        usage_value
            .insert("candidatesTokenCount".to_string(), Value::from(usage.candidates_token_count));
        usage_value.insert("totalTokenCount".to_string(), Value::from(usage.total_token_count));
        if let Some(value) = usage.cache_read_input_token_count {
            usage_value.insert("cachedContentTokenCount".to_string(), Value::from(value));
        }
        if let Some(value) = usage.thinking_token_count {
            usage_value.insert("thoughtsTokenCount".to_string(), Value::from(value));
        }
        merge_map_field(&mut raw_event, "usageMetadata", Value::Object(usage_value));
    }
    if let Some(finish_reason) = event.llm_response.finish_reason {
        let finish_reason = match finish_reason {
            FinishReason::Stop => Some("STOP"),
            FinishReason::MaxTokens => Some("MAX_TOKENS"),
            FinishReason::Safety => Some("SAFETY"),
            FinishReason::Recitation => Some("RECITATION"),
            FinishReason::Other => None,
        };
        if let Some(finish_reason) = finish_reason {
            raw_event.insert("finishReason".to_string(), Value::String(finish_reason.to_string()));
        } else {
            raw_event
                .entry("finishReason".to_string())
                .or_insert_with(|| Value::String("OTHER".to_string()));
        }
    }
    if let Some(citation_metadata) = &event.llm_response.citation_metadata {
        merge_map_field(
            &mut raw_event,
            "citationMetadata".to_string(),
            serde_json::to_value(citation_metadata).map_err(|error| {
                VertexAiSessionService::session_error(format!(
                    "failed to serialize citation metadata for google ADK rawEvent: {error}",
                ))
            })?,
        );
    }
    if let Some(interaction_id) = &event.llm_response.interaction_id {
        raw_event.insert("interactionId".to_string(), Value::String(interaction_id.clone()));
    }
    if let Some(llm_request) = &event.llm_request {
        raw_event.insert("llmRequest".to_string(), Value::String(llm_request.clone()));
    }
    if let Some(provider_metadata) = &event.llm_response.provider_metadata {
        merge_map_field(&mut raw_event, "providerMetadata", provider_metadata.clone());
    }
    if let Some(metadata) = canonical.get("eventMetadata").and_then(Value::as_object) {
        for field in
            ["customMetadata", "groundingMetadata", "inputTranscription", "outputTranscription"]
        {
            if let Some(value) = metadata.get(field) {
                merge_map_field(&mut raw_event, field, value.clone());
            }
        }
    }
    raw_event.insert(RUST_RAW_EVENT_ENVELOPE_KEY.to_string(), Value::Object(rust_envelope));
    Ok(raw_event)
}

fn merge_map_field(raw_event: &mut Map<String, Value>, field: impl Into<String>, value: Value) {
    let field = field.into();
    if let Some(existing) = raw_event.get_mut(&field) {
        merge_json_preserving_unknown(existing, value);
    } else {
        raw_event.insert(field, value);
    }
}

fn merge_json_preserving_unknown(existing: &mut Value, authoritative: Value) {
    match (existing, authoritative) {
        (Value::Object(existing), Value::Object(authoritative)) => {
            for (key, value) in authoritative {
                if let Some(existing) = existing.get_mut(&key) {
                    merge_json_preserving_unknown(existing, value);
                } else {
                    existing.insert(key, value);
                }
            }
        }
        (Value::Array(existing), Value::Array(authoritative))
            if existing.len() == authoritative.len() =>
        {
            for (existing, authoritative) in existing.iter_mut().zip(authoritative) {
                merge_json_preserving_unknown(existing, authoritative);
            }
        }
        (existing, authoritative) => *existing = authoritative,
    }
}

fn vertex_json_semantically_equal(left: &Value, right: &Value) -> bool {
    let mut pending = vec![(left, right, 0_usize)];
    while let Some((left, right, depth)) = pending.pop() {
        if depth > VERTEX_VALUE_MAX_DEPTH {
            return false;
        }
        match (left, right) {
            (Value::Null, Value::Null) => {}
            (Value::Bool(left), Value::Bool(right)) if left == right => {}
            (Value::String(left), Value::String(right)) if left == right => {}
            (Value::Number(left), Value::Number(right)) => {
                if left == right {
                    continue;
                }
                let exact = |number: &serde_json::Number| {
                    if let Some(value) = number.as_i64() {
                        (value.unsigned_abs() <= VERTEX_MAX_EXACT_INTEGER).then_some(value as f64)
                    } else if let Some(value) = number.as_u64() {
                        (value <= VERTEX_MAX_EXACT_INTEGER).then_some(value as f64)
                    } else {
                        number.as_f64().filter(|value| value.is_finite())
                    }
                };
                match (exact(left), exact(right)) {
                    (Some(left), Some(right)) if left == right => {}
                    _ => return false,
                }
            }
            (Value::Array(left), Value::Array(right)) if left.len() == right.len() => {
                pending
                    .extend(left.iter().zip(right).map(|(left, right)| (left, right, depth + 1)));
            }
            (Value::Object(left), Value::Object(right)) if left.len() == right.len() => {
                for (key, left) in left {
                    let Some(right) = right.get(key) else {
                        return false;
                    };
                    pending.push((left, right, depth + 1));
                }
            }
            _ => return false,
        }
    }
    true
}

fn canonical_content_for_append(event: &Event, content: &Content) -> Result<Value> {
    if let Some(preserved) = event_provider_metadata_map(event, VERTEX_CANONICAL_CONTENT_KEY)? {
        let preserved_value = Value::Object(preserved);
        let preserved_payload: VertexContentPayload =
            serde_json::from_value(preserved_value.clone()).map_err(|error| {
                let error = truncate_for_error(&error.to_string());
                VertexAiSessionService::invalid_input(format!(
                    "preserved vertex canonical content is invalid: {error}",
                ))
            })?;
        let restored = content_from_vertex(preserved_payload).map_err(|error| {
            let error = truncate_for_error(&error.message);
            VertexAiSessionService::invalid_input(format!(
                "preserved vertex canonical content cannot be restored: {error}",
            ))
        })?;
        let current = serde_json::to_value(content).map_err(|error| {
            VertexAiSessionService::session_error(format!(
                "failed to compare ADK event content with preserved vertex content: {error}",
            ))
        })?;
        let restored = serde_json::to_value(restored).map_err(|error| {
            VertexAiSessionService::session_error(format!(
                "failed to compare preserved vertex content with ADK content: {error}",
            ))
        })?;
        if vertex_json_semantically_equal(&current, &restored) {
            return Ok(preserved_value);
        }
    }

    serde_json::to_value(content_to_vertex(content)?).map_err(|error| {
        VertexAiSessionService::session_error(format!(
            "failed to serialize vertex event content: {error}"
        ))
    })
}

#[cfg(test)]
fn build_append_event_payload(event: &Event) -> Result<Value> {
    build_append_event_payload_with_limit(event, DEFAULT_MAX_REQUEST_BYTES)
}

fn build_append_event_payload_with_limit(event: &Event, max_request_bytes: usize) -> Result<Value> {
    if event.author.trim().is_empty() {
        return Err(VertexAiSessionService::invalid_input(
            "event author is required when appending to Vertex AI sessions",
        ));
    }
    if event.invocation_id.trim().is_empty() {
        return Err(VertexAiSessionService::invalid_input(
            "event invocation_id is required when appending to Vertex AI sessions",
        ));
    }
    validate_event_json_depth(event)?;
    validate_vertex_struct_map(&event.actions.state_delta, "actions.stateDelta")?;
    validate_json_encoded_size(event, max_request_bytes, "ADK event preflight")?;
    let preserved_custom_metadata =
        event_provider_metadata_map(event, VERTEX_CUSTOM_METADATA_KEY)?.unwrap_or_default();
    let canonical_extensions =
        event_provider_metadata_map(event, VERTEX_CANONICAL_EXTENSIONS_KEY)?.unwrap_or_default();

    let mut event_payload = Map::new();
    event_payload.insert("timestamp".to_string(), Value::String(event.timestamp.to_rfc3339()));
    event_payload.insert("author".to_string(), Value::String(event.author.clone()));
    event_payload.insert("invocationId".to_string(), Value::String(event.invocation_id.clone()));
    let content_source = if let Some(content) = &event.llm_response.content {
        match canonical_content_for_append(event, content) {
            Ok(content) => {
                event_payload.insert("content".to_string(), content);
                "canonical"
            }
            Err(error) => {
                tracing::debug!(
                    error = %error,
                    "storing ADK-only event content in vertex rawEvent"
                );
                "raw"
            }
        }
    } else {
        "none"
    };

    let mut raw_event = event.clone();
    if content_source == "canonical" {
        raw_event.llm_response.content = None;
    }
    let raw_event =
        serialize_json_bounded(&raw_event, max_request_bytes, "ADK raw-event envelope")?;
    let raw_event = String::from_utf8(raw_event).map_err(|error| {
        VertexAiSessionService::session_error(format!(
            "failed to encode ADK event as UTF-8 for vertex rawEvent persistence: {error}",
        ))
    })?;
    let rust_envelope = Map::from_iter([
        ("adkEvent".to_string(), Value::String(raw_event)),
        ("contentSource".to_string(), Value::String(content_source.to_string())),
        ("schemaVersion".to_string(), Value::from(1)),
    ]);

    let mut actions = Map::new();
    if !event.actions.state_delta.is_empty() {
        actions.insert(
            "stateDelta".to_string(),
            Value::Object(Map::from_iter(
                event.actions.state_delta.iter().map(|(key, value)| (key.clone(), value.clone())),
            )),
        );
    }
    if !event.actions.artifact_delta.is_empty() {
        let artifact_delta = event
            .actions
            .artifact_delta
            .iter()
            .map(|(key, value)| {
                i32::try_from(*value).map(|value| (key.clone(), Value::from(value)))
            })
            .collect::<std::result::Result<Map<_, _>, _>>();
        match artifact_delta {
            Ok(artifact_delta) => {
                actions.insert("artifactDelta".to_string(), Value::Object(artifact_delta));
            }
            Err(error) => {
                tracing::debug!(
                    error = %error,
                    "storing out-of-range artifact versions in vertex rawEvent only"
                );
            }
        }
    }
    if event.actions.skip_summarization {
        actions.insert("skipSummarization".to_string(), Value::Bool(true));
    }
    if event.actions.escalate {
        actions.insert("escalate".to_string(), Value::Bool(true));
    }
    if let Some(transfer_agent) =
        event.actions.transfer_to_agent.as_deref().filter(|value| !value.is_empty())
    {
        actions.insert("transferAgent".to_string(), Value::String(transfer_agent.to_string()));
    }
    if let Some(requested_auth_configs) = canonical_extensions.get("requestedAuthConfigs") {
        if !requested_auth_configs.is_object() {
            return Err(VertexAiSessionService::invalid_input(
                "preserved requestedAuthConfigs must be a JSON object",
            ));
        }
        actions.insert("requestedAuthConfigs".to_string(), requested_auth_configs.clone());
    }
    if !actions.is_empty() {
        event_payload.insert("actions".to_string(), Value::Object(actions));
    }

    let mut metadata = Map::new();
    metadata.insert("branch".to_string(), Value::String(event.branch.clone()));
    metadata.insert("partial".to_string(), Value::Bool(event.llm_response.partial));
    metadata.insert("turnComplete".to_string(), Value::Bool(event.llm_response.turn_complete));
    metadata.insert("interrupted".to_string(), Value::Bool(event.llm_response.interrupted));
    metadata.insert(
        "longRunningToolIds".to_string(),
        Value::Array(
            event
                .long_running_tool_ids
                .iter()
                .map(|tool_id| Value::String(tool_id.clone()))
                .collect(),
        ),
    );
    let mut custom_metadata = preserved_custom_metadata;
    custom_metadata.insert("adkEventId".to_string(), Value::String(event.id.clone()));
    if let Some(content) = &event.llm_response.content {
        custom_metadata.insert("adkContentRole".to_string(), Value::String(content.role.clone()));
    }
    metadata.insert("customMetadata".to_string(), Value::Object(custom_metadata));
    for field in ["groundingMetadata", "inputTranscription", "outputTranscription"] {
        if let Some(value) = canonical_extensions.get(field) {
            if !value.is_object() {
                return Err(VertexAiSessionService::invalid_input(format!(
                    "preserved {field} must be a JSON object",
                )));
            }
            metadata.insert(field.to_string(), value.clone());
        }
    }
    event_payload.insert("eventMetadata".to_string(), Value::Object(metadata));

    if let Some(error_code) =
        event.llm_response.error_code.as_deref().filter(|value| !value.is_empty())
    {
        event_payload.insert("errorCode".to_string(), Value::String(error_code.to_string()));
    }
    if let Some(error_message) =
        event.llm_response.error_message.as_deref().filter(|value| !value.is_empty())
    {
        event_payload.insert("errorMessage".to_string(), Value::String(error_message.to_string()));
    }
    let raw_event = build_google_adk_raw_event(event, &event_payload, rust_envelope)?;
    event_payload.insert("rawEvent".to_string(), Value::Object(raw_event));

    Ok(Value::Object(event_payload))
}

fn serialize_json_bounded<T: Serialize + ?Sized>(
    value: &T,
    limit: usize,
    context: &str,
) -> Result<Vec<u8>> {
    let mut writer = BoundedJsonWriter::new(limit);
    if let Err(error) = serde_json::to_writer(&mut writer, value) {
        if writer.exceeded {
            return Err(VertexAiSessionService::request_too_large(
                context,
                limit,
                writer.observed_at_least,
            ));
        }
        return Err(VertexAiSessionService::session_error(format!(
            "failed to serialize vertex {context} JSON: {error}",
        )));
    }
    Ok(writer.bytes)
}

fn validate_json_encoded_size<T: Serialize + ?Sized>(
    value: &T,
    limit: usize,
    context: &str,
) -> Result<()> {
    let mut counter = BoundedJsonCounter::new(limit);
    if let Err(error) = serde_json::to_writer(&mut counter, value) {
        if counter.exceeded {
            return Err(VertexAiSessionService::request_too_large(
                context,
                limit,
                counter.observed_at_least,
            ));
        }
        return Err(VertexAiSessionService::session_error(format!(
            "failed to measure vertex {context} JSON: {error}",
        )));
    }
    Ok(())
}

fn bounded_field_names(extra: &Map<String, Value>) -> String {
    const MAX_FIELDS: usize = 8;
    const MAX_LIST_BYTES: usize = 192;

    let mut fields = String::with_capacity(MAX_LIST_BYTES);
    let mut shown = 0;
    for field in extra.keys().take(MAX_FIELDS) {
        let separator = if fields.is_empty() { "" } else { ", " };
        let available = MAX_LIST_BYTES.saturating_sub(fields.len().saturating_add(separator.len()));
        if available <= 3 {
            break;
        }

        let field = truncate_for_error(field);
        fields.push_str(separator);
        if field.len() <= available {
            fields.push_str(&field);
        } else {
            for character in field.chars() {
                if fields.len() + character.len_utf8() > MAX_LIST_BYTES - 3 {
                    break;
                }
                fields.push(character);
            }
            fields.push_str("...");
        }
        shown += 1;
    }

    let omitted = extra.len().saturating_sub(shown);
    if omitted > 0 {
        if !fields.is_empty() {
            fields.push_str(", ");
        }
        fields.push_str(&format!("… (+{omitted} more)"));
    }
    fields
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_proto_duration_string_matches_protobuf_json_format() {
        for (duration, expected) in [
            (Duration::from_secs(86_400), "86400s"),
            (Duration::from_secs(172_800), "172800s"),
            (Duration::new(86_400, 500_000_000), "86400.5s"),
            (Duration::new(86_400, 1), "86400.000000001s"),
        ] {
            assert_eq!(proto_duration_string(duration), expected);
        }
    }

    #[test]
    fn test_vertex_endpoint_routes_global_and_multi_regions() {
        for (location, expected) in [
            ("global", "https://aiplatform.googleapis.com/"),
            ("us", "https://aiplatform.us.rep.googleapis.com/"),
            ("eu", "https://aiplatform.eu.rep.googleapis.com/"),
            ("us-central1", "https://us-central1-aiplatform.googleapis.com/"),
        ] {
            assert_eq!(VertexAiSessionConfig::new("project", location).endpoint(), expected);
        }
    }

    #[tokio::test]
    async fn test_vertex_custom_endpoint_must_be_a_trusted_origin_shape() {
        use google_cloud_auth::credentials::api_key_credentials;

        for endpoint in [
            "https://user@example.com",
            "https://example.com/api",
            "https://example.com?query=value",
            "https://example.com#fragment",
            "https://example.com//",
        ] {
            let error = VertexAiSessionService::with_credentials(
                VertexAiSessionConfig::new("project", "us-central1").with_endpoint(endpoint),
                api_key_credentials::Builder::new("test-key").build(),
            )
            .err()
            .expect("non-origin endpoint must fail during construction");
            assert_eq!(error.category, ErrorCategory::InvalidInput);
            assert!(error.message.contains("must be an origin"));
        }

        let insecure = VertexAiSessionService::with_credentials(
            VertexAiSessionConfig::new("project", "us-central1")
                .with_endpoint("http://example.com"),
            api_key_credentials::Builder::new("test-key").build(),
        )
        .err()
        .expect("non-loopback HTTP must fail during construction");
        assert_eq!(insecure.category, ErrorCategory::InvalidInput);
        assert!(insecure.message.contains("must use HTTPS"));
    }

    #[test]
    fn test_vertex_request_body_limit_accepts_exact_and_rejects_over_limit() {
        let value = serde_json::json!({ "ok": true });
        let exact =
            serialize_json_bounded(&value, 11, "test request").expect("exact-limit request body");
        assert_eq!(exact, br#"{"ok":true}"#);

        let error = serialize_json_bounded(&value, 10, "test request")
            .expect_err("over-limit request body must fail");
        assert_eq!(error.category, ErrorCategory::InvalidInput);
        assert_eq!(error.code, "session.vertex.request_too_large");
        assert!(!error.is_retryable());

        let mut event = Event::new("inv-large-inline");
        event.author = "model".to_string();
        event.llm_response.content = Some(Content {
            role: "model".to_string(),
            parts: vec![Part::InlineData {
                mime_type: "application/octet-stream".to_string(),
                data: vec![0; 256],
                uri: None,
                annotations: None,
            }],
        });
        let preflight = build_append_event_payload_with_limit(&event, 64)
            .expect_err("large inline data must fail before canonical transformation");
        assert_eq!(preflight.code, "session.vertex.request_too_large");
        assert!(preflight.message.contains("ADK event preflight"));
    }

    #[tokio::test]
    async fn test_vertex_response_byte_limit_covers_length_and_chunked_bodies() {
        use axum::{
            Router,
            body::{Body, Bytes},
            http::{Response, header::CONTENT_LENGTH},
            routing::get,
        };
        use futures::stream;
        use google_cloud_auth::credentials::api_key_credentials;
        use std::convert::Infallible;

        let app = Router::new()
            .route("/v1/under", get(|| async { "{}" }))
            .route("/v1/exact", get(|| async { r#"{"ok":true}"# }))
            .route(
                "/v1/declared-over",
                get(|| async {
                    Response::builder()
                        .header(CONTENT_LENGTH, "100")
                        .body(Body::from(vec![b'x'; 100]))
                        .expect("declared response")
                }),
            )
            .route(
                "/v1/chunked-over",
                get(|| async {
                    let chunks = stream::iter([
                        Ok::<_, Infallible>(Bytes::from_static(b"{\"large\":")),
                        Ok::<_, Infallible>(Bytes::from_static(b"true,\"more\":true}")),
                    ]);
                    Response::new(Body::from_stream(chunks))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });
        let service = VertexAiSessionService::with_credentials(
            VertexAiSessionConfig::new("project", "us-central1")
                .with_endpoint(format!("http://{address}")),
            api_key_credentials::Builder::new("test-key").build(),
        )
        .expect("build response-limit service")
        .with_max_response_bytes(11);

        let request = service.client.request(Method::GET, "under").await.expect("under request");
        assert_eq!(
            service.client.send_value(request).await.expect("under-limit response"),
            serde_json::json!({})
        );
        let request = service.client.request(Method::GET, "exact").await.expect("exact request");
        assert_eq!(
            service.client.send_value(request).await.expect("exact-limit response"),
            serde_json::json!({ "ok": true })
        );
        for path in ["declared-over", "chunked-over"] {
            let request =
                service.client.request(Method::GET, path).await.expect("oversized request");
            let error =
                service.client.send_value(request).await.expect_err("oversized response must fail");
            assert_eq!(error.code, "session.vertex.response_too_large");
            assert!(!error.is_retryable());
        }

        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn test_vertex_http_client_does_not_follow_redirects() {
        use axum::{Router, response::Redirect, routing::get};
        use google_cloud_auth::credentials::api_key_credentials;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let sink_requests = Arc::new(AtomicUsize::new(0));
        let sink_counter = Arc::clone(&sink_requests);
        let sink = Router::new().route(
            "/captured",
            get(move || {
                let sink_counter = Arc::clone(&sink_counter);
                async move {
                    sink_counter.fetch_add(1, Ordering::SeqCst);
                    "{}"
                }
            }),
        );
        let sink_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind sink");
        let sink_address = sink_listener.local_addr().expect("sink address");
        let sink_server = tokio::spawn(async move {
            axum::serve(sink_listener, sink).await.expect("serve sink");
        });

        let redirect_target = format!("http://{sink_address}/captured");
        let source = Router::new().fallback(move || {
            let redirect_target = redirect_target.clone();
            async move { Redirect::temporary(&redirect_target) }
        });
        let source_listener =
            tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind source");
        let source_address = source_listener.local_addr().expect("source address");
        let source_server = tokio::spawn(async move {
            axum::serve(source_listener, source).await.expect("serve source");
        });
        let service = VertexAiSessionService::with_credentials(
            VertexAiSessionConfig::new("project", "us-central1")
                .with_endpoint(format!("http://{source_address}")),
            api_key_credentials::Builder::new("secret-api-key").build(),
        )
        .expect("build redirect test service");
        let request = service.client.request(Method::GET, "test").await.expect("apply test auth");
        let error =
            service.client.send_value(request).await.expect_err("redirect must not be followed");
        assert_eq!(error.details.upstream_status_code, Some(307));
        assert_eq!(sink_requests.load(Ordering::SeqCst), 0);

        source_server.abort();
        sink_server.abort();
        let _ = source_server.await;
        let _ = sink_server.await;
    }

    #[test]
    fn test_vertex_remote_session_id_v1_vector() {
        let remote = vertex_remote_session_id("orders-api", "alice@example.com", "session-42");
        assert_eq!(remote, "adk1-8c1f9e7e248ea23bf89da2707f38e51d5e936384cba94f73a6eb79403a");
        assert_eq!(remote.len(), 63);
        assert!(validate_vertex_create_session_id(&remote).is_ok());
    }

    #[test]
    fn test_vertex_identity_marker_enforces_typed_field_invariants() {
        let too_long = "x".repeat(513);
        for (app_name, user_id, session_id) in [
            ("bad\0app".to_string(), "user".to_string(), "session".to_string()),
            ("app".to_string(), "bad\0user".to_string(), "session".to_string()),
            ("app".to_string(), "user".to_string(), "bad\0session".to_string()),
            (too_long.clone(), "user".to_string(), "session".to_string()),
            ("app".to_string(), too_long.clone(), "session".to_string()),
            ("app".to_string(), "user".to_string(), too_long),
        ] {
            let marker = serde_json::json!({
                "schemaVersion": 1,
                "appName": app_name,
                "userId": user_id,
                "sessionId": session_id,
            })
            .to_string();
            let state =
                HashMap::from([(VERTEX_IDENTITY_STATE_KEY.to_string(), Value::String(marker))]);
            let error = vertex_session_identity_from_state(&state, "test response")
                .expect_err("invalid marker field must fail");
            assert_eq!(error.category, ErrorCategory::Internal);
            assert!(error.message.contains("identity marker"));
        }
    }

    #[tokio::test]
    async fn test_vertex_session_scope_cache_is_bounded_deduplicated_and_exact() {
        use google_cloud_auth::credentials::api_key_credentials;

        let service = VertexAiSessionService::with_credentials(
            VertexAiSessionConfig::new("project", "us-central1")
                .with_endpoint("http://127.0.0.1:1"),
            api_key_credentials::Builder::new("test-key").build(),
        )
        .expect("build test service");
        for index in 0..VERTEX_SESSION_SCOPE_CACHE_CAPACITY {
            service.remember_session_scope(&format!("session-{index}"), "app", "user").await;
        }
        assert_eq!(service.session_scopes.read().await.len(), VERTEX_SESSION_SCOPE_CACHE_CAPACITY);

        service.remember_session_scope("session-0", "app", "user").await;
        assert_eq!(service.session_scopes.read().await.len(), VERTEX_SESSION_SCOPE_CACHE_CAPACITY);
        assert_eq!(
            service.session_scopes.read().await.back().map(|(id, _)| id.as_str()),
            Some("session-0")
        );

        service
            .remember_session_scope(
                &format!("session-{VERTEX_SESSION_SCOPE_CACHE_CAPACITY}"),
                "app",
                "user",
            )
            .await;
        assert_eq!(service.session_scopes.read().await.len(), VERTEX_SESSION_SCOPE_CACHE_CAPACITY);
        assert!(service.resolve_session_scope_for_append("session-1").await.is_err());
        assert_eq!(
            service.resolve_session_scope_for_append("session-0").await.unwrap(),
            SessionScope { app_name: "app".to_string(), user_id: "user".to_string() }
        );

        service.remember_session_scope("shared", "app-a", "user").await;
        service.remember_session_scope("shared", "app-b", "user").await;
        assert!(service.resolve_session_scope_for_append("shared").await.is_err());
        service.forget_session_scope("shared", "app-a", "user").await;
        assert_eq!(
            service.resolve_session_scope_for_append("shared").await.unwrap(),
            SessionScope { app_name: "app-b".to_string(), user_id: "user".to_string() }
        );
    }

    #[test]
    fn test_truncate_for_error_ascii() {
        let short = "hello";
        assert_eq!(truncate_for_error(short), "hello");

        let long = "a".repeat(600);
        let result = truncate_for_error(&long);
        assert!(result.ends_with("..."));
        assert!(result.len() <= 515); // 512 + "..."
    }

    #[test]
    fn test_truncate_for_error_multibyte_no_panic() {
        // Chinese characters are 3 bytes each in UTF-8
        // 200 repetitions of "你好" = 1200 bytes, byte 512 is NOT a char boundary
        let value = "你好".repeat(200);
        assert_eq!(value.len(), 1200);
        assert!(!value.is_char_boundary(512)); // confirms the bug condition

        // This must NOT panic
        let result = truncate_for_error(&value);
        assert!(result.ends_with("..."));
        assert!(result.is_char_boundary(result.len())); // valid UTF-8
    }

    #[test]
    fn test_truncate_for_error_emoji() {
        // Emoji are 4 bytes each
        let value = "🎉".repeat(200); // 800 bytes
        let result = truncate_for_error(&value);
        assert!(result.ends_with("..."));
        // Should truncate to a valid char boundary
        let without_dots = &result[..result.len() - 3];
        assert!(without_dots.is_char_boundary(without_dots.len()));
    }

    #[test]
    fn test_upstream_error_text_is_bounded_and_control_sanitized() {
        assert_eq!(truncate_for_error("line\nbreak\tend"), "line�break�end");
        let input = format!("{}\nsecret\tline", "x".repeat(600));
        let sanitized = truncate_for_error(&input);
        assert!(sanitized.ends_with("..."));
        assert!(!sanitized.chars().any(char::is_control));
        assert!(sanitized.len() <= 515);

        let status = gcp_error_context().status_error(reqwest::StatusCode::BAD_GATEWAY, &input);
        assert!(!status.message.chars().any(char::is_control));
        let operation = gcp_error_context().operation_error(
            "create session",
            "projects/p/locations/l/operations/op",
            13,
            &input,
        );
        assert!(!operation.message.chars().any(char::is_control));
        assert!(operation.message.len() < 700);
    }

    #[test]
    fn test_unknown_field_errors_are_bounded_and_control_sanitized() {
        let mut extra = Map::new();
        for index in 0..100 {
            extra.insert(format!("{index:03}-{}\nsecret", "x".repeat(1_024)), Value::Null);
        }

        let fields = bounded_field_names(&extra);
        assert!(fields.len() < 256);
        assert!(!fields.chars().any(char::is_control));
        assert!(fields.contains("more"));

        let error =
            reject_vertex_event_fields("SessionEvent", &extra).expect_err("unknown fields fail");
        assert!(error.message.len() < 512);
        assert!(!error.message.chars().any(char::is_control));
    }

    #[test]
    fn test_credentials_errors_preserve_provider_retryability() {
        use google_cloud_auth::errors::CredentialsError;

        let transient = gcp_error_context().credentials_error(&CredentialsError::from_msg(
            true,
            "temporary metadata server failure",
        ));
        assert_eq!(transient.category, ErrorCategory::Unavailable);
        assert_eq!(transient.code, "session.vertex.credentials_unavailable");
        assert!(transient.is_retryable());

        let permanent = gcp_error_context()
            .credentials_error(&CredentialsError::from_msg(false, "invalid service account"));
        assert_eq!(permanent.category, ErrorCategory::Unauthorized);
        assert!(!permanent.is_retryable());
    }

    #[test]
    fn test_extract_reasoning_engine_id_from_resource_name() {
        assert_eq!(
            extract_reasoning_engine_id_from_resource_name(
                "projects/my-project/locations/us-central1/reasoningEngines/123456",
            ),
            Some("123456".to_string())
        );
        assert_eq!(extract_reasoning_engine_id_from_resource_name("123456"), None);
        assert_eq!(
            extract_reasoning_engine_id_from_resource_name(
                "projects/my-project/locations/us-central1/reasoningEngines/0123456",
            ),
            None
        );
        assert_eq!(
            extract_reasoning_engine_id_from_resource_name(
                "projects/my-project/locations/us-central1/reasoningEngines/not-numeric",
            ),
            None
        );
    }

    #[test]
    fn test_validate_vertex_session_ids() {
        assert!(validate_vertex_create_session_id("a").is_ok());
        assert!(validate_vertex_create_session_id("session-123").is_ok());
        assert!(validate_vertex_create_session_id("").is_err());
        assert!(validate_vertex_create_session_id("1session").is_err());
        assert!(validate_vertex_create_session_id("Session").is_err());
        assert!(validate_vertex_create_session_id("session-").is_err());
        assert!(validate_vertex_create_session_id("../session").is_err());
        assert!(validate_vertex_create_session_id("session/other").is_err());
        assert!(validate_vertex_create_session_id("session?other").is_err());
        assert!(validate_vertex_create_session_id(&format!("a{}", "1".repeat(63))).is_err());

        assert!(validate_vertex_session_resource_id("9Server_ID").is_ok());
        assert!(validate_vertex_session_resource_id("../session").is_err());
        assert!(validate_vertex_session_resource_id("session/other").is_err());
        assert!(validate_vertex_session_resource_id("session?other").is_err());
        assert!(validate_vertex_resource_segment("project/escape", "project_id").is_err());
        assert!(validate_vertex_resource_segment("location?query", "location").is_err());
        assert!(
            validate_vertex_operation_name(
                "projects/other/locations/us-central1/operations/op",
                "project",
                "us-central1",
            )
            .is_err()
        );

        let session_error = validate_vertex_upstream_session_resource_id("bad/id", "test response")
            .expect_err("malformed upstream session IDs must fail");
        assert_eq!(session_error.category, ErrorCategory::Internal);
        let operation_error = validate_vertex_operation_name(
            "projects/project/locations/us-central1/operations/bad/id",
            "project",
            "us-central1",
        )
        .expect_err("malformed upstream operation IDs must fail");
        assert_eq!(operation_error.category, ErrorCategory::Internal);
        let dot_segment = validate_vertex_operation_name(
            "projects/project/locations/us-central1/operations/..",
            "project",
            "us-central1",
        )
        .expect_err("operation dot segments must fail");
        assert_eq!(dot_segment.category, ErrorCategory::Internal);

        assert!(
            validate_vertex_page_token(&"x".repeat(VERTEX_MAX_PAGE_TOKEN_BYTES), "test").is_ok()
        );
        let page_token =
            validate_vertex_page_token(&"x".repeat(VERTEX_MAX_PAGE_TOKEN_BYTES + 1), "test")
                .expect_err("oversized page tokens must fail");
        assert_eq!(page_token.category, ErrorCategory::Internal);
        assert!(page_token.message.contains("page token"));
    }

    #[test]
    fn test_vertex_user_filter_escapes_aip160_string_literals() {
        assert_eq!(
            vertex_user_filter("user\\\"\n\t\u{0001} OR user_id=\"admin").unwrap(),
            r#"user_id="user\\\"\n\t\u0001 OR user_id=\"admin""#
        );
    }

    #[test]
    fn test_endpoint_does_not_treat_localhost_subdomain_as_loopback() {
        let remote = VertexAiSessionConfig::new("project", "us-central1")
            .with_endpoint("http://localhost.evil.com");
        assert_eq!(remote.endpoint(), "http://localhost.evil.com/");

        let local = VertexAiSessionConfig::new("project", "us-central1")
            .with_endpoint("http://localhost:8080");
        assert_eq!(local.endpoint(), "http://localhost:8080/");
    }

    #[test]
    fn test_sanitize_state_map_removes_temp_and_preserves_null() {
        let mut state = HashMap::new();
        state.insert("k".to_string(), Value::String("v".to_string()));
        state.insert("temp:k".to_string(), Value::String("temp".to_string()));
        state.insert("null".to_string(), Value::Null);

        let sanitized = sanitize_state_map(state);
        assert_eq!(sanitized.get("k"), Some(&Value::String("v".to_string())));
        assert!(!sanitized.contains_key("temp:k"));
        assert_eq!(sanitized.get("null"), Some(&Value::Null));
    }

    #[test]
    fn test_append_payload_requires_vertex_event_identity() {
        let event = Event::new("inv-1");
        let error = build_append_event_payload(&event).expect_err("missing author must fail");
        assert!(error.message.contains("author is required"));

        let mut event = Event::new("");
        event.author = "agent".to_string();
        let error =
            build_append_event_payload(&event).expect_err("missing invocation ID must fail");
        assert!(error.message.contains("invocation_id is required"));
    }

    #[test]
    fn test_append_payload_uses_raw_event_for_opaque_thought_signature() {
        let mut event = Event::new("inv-1");
        event.author = "agent".to_string();
        event.llm_response.content = Some(Content {
            role: "model".to_string(),
            parts: vec![Part::Thinking {
                thinking: "reasoning".to_string(),
                signature: Some("not base64!".to_string()),
            }],
        });

        let content_error = content_to_vertex(event.llm_response.content.as_ref().unwrap())
            .expect_err("invalid outbound signatures must be rejected as caller input");
        assert_eq!(content_error.category, ErrorCategory::InvalidInput);
        let payload =
            build_append_event_payload(&event).expect("opaque signature must use raw fallback");
        assert!(payload.get("content").is_none());
        assert_eq!(payload["rawEvent"]["_adkRust"]["contentSource"], "raw");
        let restored: Event =
            serde_json::from_str(payload["rawEvent"]["_adkRust"]["adkEvent"].as_str().unwrap())
                .unwrap();
        assert_eq!(
            restored.llm_response.content.unwrap().parts,
            event.llm_response.content.unwrap().parts
        );
    }

    #[test]
    fn test_content_metadata_uses_lossless_raw_event_fallback() {
        let annotations = serde_json::json!({"audience": ["user"], "priority": 0.8});
        let parts = vec![
            Part::InlineData {
                mime_type: "image/png".to_string(),
                data: vec![1, 2, 3],
                uri: Some("file:///screenshots/result.png".to_string()),
                annotations: Some(annotations.clone()),
            },
            Part::FileData {
                mime_type: "application/pdf".to_string(),
                file_uri: "gs://reports/result.pdf".to_string(),
                annotations: Some(annotations.clone()),
            },
            Part::FunctionResponse {
                function_response: FunctionResponseData::with_multimodal(
                    "render_report",
                    serde_json::json!({"ok": true}),
                    vec![InlineDataPart {
                        mime_type: "image/png".to_string(),
                        data: vec![4, 5, 6],
                        uri: Some("file:///screenshots/tool-result.png".to_string()),
                        annotations: Some(annotations.clone()),
                    }],
                    vec![FileDataPart {
                        mime_type: "application/pdf".to_string(),
                        file_uri: "gs://reports/tool-result.pdf".to_string(),
                        annotations: Some(annotations.clone()),
                    }],
                ),
                id: None,
                annotations: Some(annotations),
            },
        ];

        for part in &parts {
            let error = content_to_vertex(&Content {
                role: "model".to_string(),
                parts: vec![part.clone()],
            })
            .expect_err("metadata must not be dropped by canonical Vertex persistence");
            assert!(error.message.contains("rawEvent persistence"));
        }

        let content = Content { role: "model".to_string(), parts };
        let mut event = Event::new("inv-content-metadata");
        event.author = "model".to_string();
        event.llm_response.content = Some(content.clone());
        let payload =
            build_append_event_payload(&event).expect("metadata must use rawEvent persistence");
        assert!(payload.get("content").is_none());
        assert_eq!(payload["rawEvent"]["_adkRust"]["contentSource"], "raw");

        let restored: Event =
            serde_json::from_str(payload["rawEvent"]["_adkRust"]["adkEvent"].as_str().unwrap())
                .expect("restore private event");
        assert_eq!(
            serde_json::to_value(restored.llm_response.content).unwrap(),
            serde_json::to_value(Some(content)).unwrap(),
        );
    }

    #[test]
    fn test_noncanonical_base64_thought_signatures_round_trip_raw() {
        for (event_id, part) in [
            (
                "url-safe-signature",
                Part::Thinking {
                    thinking: "reasoning".to_string(),
                    signature: Some("-_8=".to_string()),
                },
            ),
            (
                "unpadded-signature",
                Part::FunctionCall {
                    name: "lookup".to_string(),
                    args: serde_json::json!({}),
                    id: None,
                    thought_signature: Some("YQ".to_string()),
                },
            ),
        ] {
            let mut event = Event::new(format!("inv-{event_id}"));
            event.id = event_id.to_string();
            event.author = "model".to_string();
            event.llm_response.content =
                Some(Content { role: "model".to_string(), parts: vec![part] });
            let expected = serde_json::to_value(&event).expect("serialize expected event");

            let mut payload =
                build_append_event_payload(&event).expect("use raw base64 persistence");
            assert!(payload.get("content").is_none());
            assert_eq!(payload["rawEvent"]["_adkRust"]["contentSource"], "raw");
            payload["name"] = Value::String(format!(
                "projects/p/locations/l/reasoningEngines/1/sessions/s/events/{event_id}"
            ));
            let payload: VertexEventPayload =
                serde_json::from_value(payload).expect("parse raw signature event");
            let restored =
                payload.try_into_event().expect("restore exact noncanonical signature event");
            assert_eq!(serde_json::to_value(restored).expect("serialize restored event"), expected,);
        }
    }

    #[test]
    fn test_private_event_restores_proto3_omitted_empty_scalars() {
        let empty_signature_parts = vec![
            Part::Thinking { thinking: "reasoning".to_string(), signature: Some(String::new()) },
            Part::FunctionCall {
                name: "lookup".to_string(),
                args: serde_json::json!({}),
                id: None,
                thought_signature: Some(String::new()),
            },
        ];
        for part in &empty_signature_parts {
            let error = content_to_vertex(&Content {
                role: "model".to_string(),
                parts: vec![part.clone()],
            })
            .expect_err("empty optional bytes must not use canonical proto3 JSON");
            assert_eq!(error.category, ErrorCategory::InvalidInput);
            assert!(error.message.contains("rawEvent persistence"));
        }

        let mut event = Event::new("inv-proto-defaults");
        event.id = "proto-defaults".to_string();
        event.author = "model".to_string();
        event.actions.state_delta.insert("count".to_string(), Value::from(1));
        event.actions.transfer_to_agent = Some(String::new());
        event.llm_response.error_code = Some(String::new());
        event.llm_response.error_message = Some(String::new());
        event.llm_response.content =
            Some(Content { role: "model".to_string(), parts: empty_signature_parts });

        let expected = serde_json::to_value(&event).expect("serialize expected private event");
        let mut payload =
            build_append_event_payload(&event).expect("build proto3 default-normalization payload");
        assert!(payload.get("content").is_none());
        assert_eq!(payload["rawEvent"]["_adkRust"]["contentSource"], "raw");
        assert!(payload["actions"].get("transferAgent").is_none());
        payload["actions"]["stateDelta"]["count"] =
            Value::Number(serde_json::Number::from_f64(1.0).expect("finite Struct number"));
        assert!(payload.get("errorCode").is_none());
        assert!(payload.get("errorMessage").is_none());
        payload["rawEvent"]["_adkRust"]["schemaVersion"] =
            Value::Number(serde_json::Number::from_f64(1.0).expect("finite schema version"));

        let metadata = payload["eventMetadata"].as_object_mut().expect("event metadata");
        for field in ["branch", "partial", "turnComplete", "interrupted", "longRunningToolIds"] {
            metadata.remove(field);
        }
        payload["name"] = Value::String(
            "projects/p/locations/l/reasoningEngines/1/sessions/s/events/proto-defaults"
                .to_string(),
        );

        let payload: VertexEventPayload =
            serde_json::from_value(payload).expect("parse normalized SessionEvent");
        let restored =
            payload.try_into_event().expect("proto3-omitted empty scalars must restore privately");
        assert_eq!(
            serde_json::to_value(restored).expect("serialize restored private event"),
            expected,
        );
    }

    #[test]
    fn test_ga_v1_function_ids_use_lossless_raw_event_fallback() {
        let content = Content {
            role: "model".to_string(),
            parts: vec![
                Part::FunctionCall {
                    name: "lookup".to_string(),
                    args: serde_json::json!({ "key": "value" }),
                    id: Some("call-1".to_string()),
                    thought_signature: None,
                },
                Part::FunctionResponse {
                    function_response: FunctionResponseData::new(
                        "lookup",
                        serde_json::json!({ "ok": true }),
                    ),
                    id: Some("call-1".to_string()),
                    annotations: None,
                },
            ],
        };
        let error = content_to_vertex(&content)
            .expect_err("GA v1 canonical function content must reject IDs");
        assert_eq!(error.category, ErrorCategory::InvalidInput);
        assert!(error.message.contains("GA v1"));

        let mut event = Event::new("inv-function-ids");
        event.author = "model".to_string();
        event.llm_response.content = Some(content.clone());
        let payload =
            build_append_event_payload(&event).expect("ID-bearing content uses rawEvent fallback");
        assert!(payload.get("content").is_none());
        assert_eq!(payload["rawEvent"]["_adkRust"]["contentSource"], "raw");
        let restored: Event =
            serde_json::from_str(payload["rawEvent"]["_adkRust"]["adkEvent"].as_str().unwrap())
                .expect("restore private event");
        assert_eq!(
            serde_json::to_value(restored.llm_response.content).unwrap(),
            serde_json::to_value(Some(content)).unwrap(),
        );

        for function_part in [
            serde_json::json!({
                "functionCall": {
                    "id": "call-1",
                    "name": "lookup",
                    "args": {}
                }
            }),
            serde_json::json!({
                "functionResponse": {
                    "id": "call-1",
                    "name": "lookup",
                    "response": {}
                }
            }),
        ] {
            let payload: VertexEventPayload = serde_json::from_value(serde_json::json!({
                "name": "projects/p/locations/l/reasoningEngines/1/sessions/s/events/e",
                "timestamp": "2026-01-02T03:04:05Z",
                "invocationId": "inv-1",
                "author": "model",
                "content": { "parts": [function_part] }
            }))
            .expect("parse ID-bearing upstream content");
            let error =
                payload.try_into_event().expect_err("GA v1 upstream function ID must fail closed");
            assert!(error.message.contains("unsupported fields [id]"));
        }
    }

    #[test]
    fn test_append_payload_uses_raw_event_for_non_struct_and_wide_artifact_version() {
        let mut event = Event::new("inv-1");
        event.author = "agent".to_string();
        event.actions.artifact_delta.insert("artifact".to_string(), i64::MAX);
        event.llm_response.content = Some(Content {
            role: "model".to_string(),
            parts: vec![Part::FunctionCall {
                name: "lookup".to_string(),
                args: Value::String("not-a-struct".to_string()),
                id: Some("call-1".to_string()),
                thought_signature: None,
            }],
        });

        let payload = build_append_event_payload(&event).expect("raw fallback must be lossless");
        assert!(payload.get("content").is_none());
        assert!(payload["actions"].get("artifactDelta").is_none());
        assert_eq!(payload["rawEvent"]["_adkRust"]["contentSource"], "raw");
        let raw_event: Value =
            serde_json::from_str(payload["rawEvent"]["_adkRust"]["adkEvent"].as_str().unwrap())
                .unwrap();
        assert_eq!(raw_event["actions"]["artifact_delta"]["artifact"], i64::MAX);
        assert_eq!(raw_event["content"]["parts"][0]["args"], "not-a-struct");
    }

    #[test]
    fn test_vertex_struct_rejects_wide_state_integer() {
        let exact_limit = (1_u64 << 53) - 1;
        assert!(
            validate_vertex_struct_value(&serde_json::json!({ "value": exact_limit }), "state")
                .is_ok()
        );
        let error = validate_vertex_struct_value(
            &serde_json::json!({ "nested": [{ "value": exact_limit + 1 }] }),
            "state",
        )
        .expect_err("wide integer must not enter a Vertex Struct");
        assert!(error.message.contains("exact range"));

        let mut event = Event::new("inv-wide-state");
        event.author = "agent".to_string();
        event.actions.state_delta.insert("wide".to_string(), Value::from(exact_limit + 1));
        let error =
            build_append_event_payload(&event).expect_err("wide state delta must fail locally");
        assert!(error.message.contains("encode it as a string"));
    }

    #[test]
    fn test_vertex_struct_depth_is_bounded_without_recursive_descent() {
        let mut at_limit = Value::Null;
        for _ in 0..VERTEX_VALUE_MAX_DEPTH {
            at_limit = serde_json::json!({ "nested": at_limit });
        }
        validate_vertex_struct_value(&at_limit, "state").expect("maximum depth is accepted");
        validate_vertex_struct_value(&Value::Array(vec![Value::Null; 100_000]), "wide")
            .expect("wide values are traversed with depth-bounded auxiliary storage");

        let too_deep = serde_json::json!({ "nested": at_limit });
        let caller_error = validate_vertex_struct_value(&too_deep, "state")
            .expect_err("caller Struct depth must be bounded");
        assert_eq!(caller_error.category, ErrorCategory::InvalidInput);
        assert!(caller_error.message.contains("nesting depth"));

        let upstream_error = validate_vertex_upstream_struct_value(&too_deep, "rawEvent")
            .expect_err("upstream Struct depth must be bounded");
        assert_eq!(upstream_error.category, ErrorCategory::Internal);
        assert!(upstream_error.message.contains("nesting depth"));

        let caller_state = HashMap::from([("deep".to_string(), too_deep.clone())]);
        let caller_state_error = validate_vertex_struct_map(&caller_state, "sessionState")
            .expect_err("caller session state depth must be bounded");
        assert_eq!(caller_state_error.category, ErrorCategory::InvalidInput);

        let payload = VertexSessionPayload {
            name: "projects/p/locations/l/reasoningEngines/1/sessions/s".to_string(),
            user_id: "user".to_string(),
            session_state: caller_state,
            create_time: Some("2026-01-01T00:00:00Z".to_string()),
            update_time: None,
        };
        let upstream_state_error = validate_vertex_session_payload(&payload, "test response")
            .expect_err("upstream session state depth must be bounded");
        assert_eq!(upstream_state_error.category, ErrorCategory::Internal);
        assert!(upstream_state_error.message.contains("nesting depth"));
    }

    #[test]
    fn test_vertex_inbound_event_depth_is_bounded_before_clone_or_replay() {
        fn deep_value() -> Value {
            let mut value = Value::Null;
            for _ in 0..=VERTEX_VALUE_MAX_DEPTH {
                value = serde_json::json!({ "nested": value });
            }
            value
        }

        let metadata_payload = VertexEventPayload {
            timestamp: Some("2026-01-01T00:00:00Z".to_string()),
            invocation_id: "inv-metadata-depth".to_string(),
            author: "model".to_string(),
            event_metadata: VertexEventMetadataPayload {
                custom_metadata: Map::from_iter([("deep".to_string(), deep_value())]),
                ..Default::default()
            },
            ..Default::default()
        };
        let metadata_error = metadata_payload
            .try_into_event()
            .expect_err("deep upstream custom metadata must fail before cloning");
        assert_eq!(metadata_error.category, ErrorCategory::Internal);
        assert!(metadata_error.message.contains("nesting depth"));

        let mut raw_event = Event::new("inv-raw-depth");
        raw_event.author = "model".to_string();
        raw_event.llm_response.provider_metadata = Some(deep_value());
        let payload = VertexEventPayload {
            timestamp: Some(raw_event.timestamp.to_rfc3339()),
            invocation_id: raw_event.invocation_id.clone(),
            author: raw_event.author.clone(),
            raw_event: Some(Map::from_iter([(
                RUST_RAW_EVENT_ENVELOPE_KEY.to_string(),
                serde_json::json!({
                    "schemaVersion": 1,
                    "contentSource": "none",
                    "adkEvent": serde_json::to_string(&raw_event).expect("encode deep raw event"),
                }),
            )])),
            ..Default::default()
        };
        let raw_error =
            payload.try_into_event().expect_err("deep upstream raw event must fail before replay");
        assert_eq!(raw_error.category, ErrorCategory::Internal);
        assert!(raw_error.message.contains("nesting depth"));
    }

    #[test]
    fn test_append_depth_preflight_covers_every_lossless_event_value_path() {
        fn deep_value() -> Value {
            let mut value = Value::Null;
            for _ in 0..=VERTEX_VALUE_MAX_DEPTH {
                value = serde_json::json!({ "nested": value });
            }
            value
        }

        fn event_with_part(part: Part) -> Event {
            let mut event = Event::new("inv-depth");
            event.author = "agent".to_string();
            event.llm_response.content =
                Some(Content { role: "model".to_string(), parts: vec![part] });
            event
        }

        fn assert_depth_error(event: &Event) {
            let error = build_append_event_payload(event)
                .expect_err("deep lossless JSON must fail before serialization");
            assert_eq!(error.category, ErrorCategory::InvalidInput);
            assert!(error.message.contains("nesting depth"));
        }

        let early_raw_fallback = {
            let mut event = event_with_part(Part::ServerToolCall {
                server_tool_call: serde_json::json!({ "name": "search" }),
            });
            event.llm_response.content.as_mut().unwrap().parts.push(Part::FunctionCall {
                name: "late".to_string(),
                args: deep_value(),
                id: None,
                thought_signature: None,
            });
            event
        };
        assert_depth_error(&early_raw_fallback);

        for part in [
            Part::FunctionResponse {
                function_response: FunctionResponseData::new("tool", deep_value()),
                id: None,
                annotations: None,
            },
            Part::ServerToolCall { server_tool_call: deep_value() },
            Part::ServerToolResponse { server_tool_response: deep_value() },
        ] {
            assert_depth_error(&event_with_part(part));
        }

        let mut provider_metadata = Event::new("inv-provider-metadata");
        provider_metadata.author = "agent".to_string();
        provider_metadata.llm_response.provider_metadata = Some(deep_value());
        assert_depth_error(&provider_metadata);

        let mut provider_usage = Event::new("inv-provider-usage");
        provider_usage.author = "agent".to_string();
        provider_usage.llm_response.usage_metadata =
            Some(UsageMetadata { provider_usage: Some(deep_value()), ..Default::default() });
        assert_depth_error(&provider_usage);

        let mut confirmation = Event::new("inv-confirmation");
        confirmation.author = "agent".to_string();
        confirmation.actions.tool_confirmation = Some(adk_core::ToolConfirmationRequest {
            tool_name: "tool".to_string(),
            function_call_id: None,
            args: deep_value(),
        });
        assert_depth_error(&confirmation);

        let mut compaction = Event::new("inv-compaction");
        compaction.author = "agent".to_string();
        compaction.actions.compaction = Some(adk_core::EventCompaction {
            start_timestamp: Utc::now(),
            end_timestamp: Utc::now(),
            compacted_content: Content {
                role: "model".to_string(),
                parts: vec![Part::ServerToolResponse { server_tool_response: deep_value() }],
            },
        });
        assert_depth_error(&compaction);

        let mut sidecar = Event::new("inv-sidecar");
        sidecar.author = "agent".to_string();
        sidecar.provider_metadata.insert(
            VERTEX_CUSTOM_METADATA_KEY.to_string(),
            serde_json::to_string(&serde_json::json!({ "deep": deep_value() }))
                .expect("encode deep sidecar"),
        );
        assert_depth_error(&sidecar);

        let mut state_delta = Event::new("inv-state-delta");
        state_delta.author = "agent".to_string();
        state_delta.actions.state_delta.insert("deep".to_string(), deep_value());
        assert_depth_error(&state_delta);
    }

    #[test]
    fn test_append_payload_uses_raw_event_for_wide_function_args() {
        let mut event = Event::new("inv-wide-args");
        event.author = "agent".to_string();
        event.llm_response.content = Some(Content {
            role: "model".to_string(),
            parts: vec![Part::FunctionCall {
                name: "lookup".to_string(),
                args: serde_json::json!({ "wide": 1_u64 << 53 }),
                id: None,
                thought_signature: None,
            }],
        });

        let payload = build_append_event_payload(&event).expect("wide args must use raw fallback");
        assert!(payload.get("content").is_none());
        assert_eq!(payload["rawEvent"]["_adkRust"]["contentSource"], "raw");
    }

    #[test]
    fn test_canonical_content_accepts_blank_role_and_optional_function_call_fields() {
        let payload: VertexEventPayload = serde_json::from_value(serde_json::json!({
            "name": "projects/p/locations/l/reasoningEngines/1/sessions/s/events/e",
            "timestamp": "2026-01-02T03:04:05Z",
            "invocationId": "inv-1",
            "author": "model",
            "content": {
                "parts": [{ "functionCall": {} }]
            },
            "eventMetadata": {
                "customMetadata": {
                    "adkContentRole": "untrusted-role",
                    "tenant": null
                }
            }
        }))
        .expect("parse canonical event");

        let event = payload.try_into_event().expect("optional proto fields must load");
        let content = event.llm_response.content.as_ref().expect("content");
        assert_eq!(content.role, "");
        assert_eq!(
            content.parts,
            vec![Part::FunctionCall {
                name: String::new(),
                args: Value::Object(Map::new()),
                id: None,
                thought_signature: None,
            }]
        );
        let metadata: Value = serde_json::from_str(
            event
                .provider_metadata
                .get(VERTEX_CUSTOM_METADATA_KEY)
                .expect("untrusted custom metadata preserved"),
        )
        .expect("custom metadata JSON");
        assert_eq!(metadata["adkContentRole"], "untrusted-role");
        assert!(metadata["tenant"].is_null());

        let appended = build_append_event_payload(&event).expect("append blank-role content");
        assert!(appended["content"].get("role").is_none());
        assert!(appended["rawEvent"]["content"].get("role").is_none());
        assert_eq!(appended["rawEvent"]["_adkRust"]["contentSource"], "canonical");

        let mut stored = appended;
        stored["name"] = Value::String(
            "projects/p/locations/l/reasoningEngines/1/sessions/s/events/e".to_string(),
        );
        let stored: VertexEventPayload =
            serde_json::from_value(stored).expect("parse appended blank-role event");
        let restored = stored.try_into_event().expect("restore appended blank-role event");
        assert_eq!(restored.llm_response.content.unwrap().role, "");
    }

    #[test]
    fn test_raw_content_source_remains_authoritative_when_content_is_convertible() {
        let mut raw_event = Event::with_id("event-1", "inv-1");
        raw_event.author = "model".to_string();
        raw_event.timestamp = parse_rfc3339_utc("2026-01-02T03:04:05Z").unwrap();
        raw_event.llm_response.content = Some(Content {
            role: "model".to_string(),
            parts: vec![Part::Text { text: "convertible now".to_string() }],
        });
        let payload: VertexEventPayload = serde_json::from_value(serde_json::json!({
            "name": "projects/p/locations/l/reasoningEngines/1/sessions/s/events/event-1",
            "timestamp": "2026-01-02T03:04:05Z",
            "invocationId": "inv-1",
            "author": "model",
            "eventMetadata": {
                "customMetadata": {
                    "adkEventId": "event-1",
                    "adkContentRole": "model"
                }
            },
            "rawEvent": {
                "id": "event-1",
                "timestamp": 1767323045.0,
                "invocationId": "inv-1",
                "author": "model",
                "_adkRust": {
                    "schemaVersion": 1,
                    "contentSource": "raw",
                    "adkEvent": serde_json::to_string(&raw_event).unwrap()
                }
            }
        }))
        .expect("parse raw-only event");

        let restored = payload.try_into_event().expect("raw-only source is authoritative");
        assert_eq!(
            restored.llm_response.content.unwrap().parts,
            raw_event.llm_response.content.unwrap().parts
        );
    }

    #[test]
    fn test_malformed_private_raw_envelope_fails_closed() {
        let payload: VertexEventPayload = serde_json::from_value(serde_json::json!({
            "name": "projects/p/locations/l/reasoningEngines/1/sessions/s/events/e",
            "timestamp": "2026-01-02T03:04:05Z",
            "invocationId": "inv-1",
            "author": "model",
            "rawEvent": {
                "id": "e",
                "timestamp": 1767323045.0,
                "invocationId": "inv-1",
                "author": "model",
                "_adkRust": {
                    "schemaVersion": 2,
                    "contentSource": "none",
                    "adkEvent": "{}"
                }
            }
        }))
        .expect("parse event payload");
        let error = payload.try_into_event().expect_err("unsupported private envelope must fail");
        assert!(error.message.contains("schemaVersion"));
    }

    #[test]
    fn test_private_raw_envelope_requires_canonical_event_id_marker() {
        let mut event = Event::with_id("event-1", "inv-1");
        event.author = "model".to_string();
        let mut stored = build_append_event_payload(&event).expect("build private envelope");
        stored["name"] = Value::String(
            "projects/p/locations/l/reasoningEngines/1/sessions/s/events/event-1".to_string(),
        );
        stored["eventMetadata"]["customMetadata"].as_object_mut().unwrap().remove("adkEventId");
        let stored: VertexEventPayload =
            serde_json::from_value(stored).expect("parse stripped private envelope");
        let error = stored.try_into_event().expect_err("stripped ID marker must fail closed");
        assert!(error.message.contains("missing canonical adkEventId"));
    }

    #[test]
    fn test_inbound_events_reject_reserved_identity_state_delta() {
        let direct: VertexEventPayload = serde_json::from_value(serde_json::json!({
            "name": "projects/p/locations/l/reasoningEngines/1/sessions/s/events/direct",
            "timestamp": "2026-01-02T03:04:05Z",
            "invocationId": "direct-invocation",
            "author": "model",
            "actions": {
                "stateDelta": {
                    VERTEX_IDENTITY_STATE_KEY: "forged"
                }
            },
            "rawEvent": {
                "id": "direct",
                "timestamp": 1.0,
                "invocationId": "direct-invocation",
                "author": "model",
                "actions": {
                    "stateDelta": {
                        VERTEX_IDENTITY_STATE_KEY: "forged"
                    }
                }
            }
        }))
        .expect("parse direct event with reserved delta");
        let direct_error = direct.try_into_event().expect_err("direct reserved delta must fail");
        assert_eq!(direct_error.category, ErrorCategory::Internal);
        assert!(direct_error.message.contains("reserved Vertex identity key"));

        let mut raw_event = Event::with_id("rust", "rust-invocation");
        raw_event.author = "model".to_string();
        raw_event
            .actions
            .state_delta
            .insert(VERTEX_IDENTITY_STATE_KEY.to_string(), Value::String("forged".to_string()));
        let mut stored = build_append_event_payload(&raw_event)
            .expect("build private event with reserved delta");
        stored["name"] = Value::String(
            "projects/p/locations/l/reasoningEngines/1/sessions/s/events/rust".to_string(),
        );
        let stored: VertexEventPayload =
            serde_json::from_value(stored).expect("parse private event with reserved delta");
        let rust_error = stored.try_into_event().expect_err("private reserved delta must fail");
        assert_eq!(rust_error.category, ErrorCategory::Internal);
        assert!(rust_error.message.contains("reserved Vertex identity key"));
    }

    #[test]
    fn test_google_adk_direct_raw_event_is_decoded_and_preserved() {
        let direct_raw = serde_json::json!({
            "id": "private-id",
            "timestamp": 1.0,
            "invocationId": "private-invocation",
            "author": "private-author",
            "content": {
                "parts": [{ "text": "from direct raw" }]
            },
            "actions": {
                "stateDelta": { "deleted": null },
                "artifactDelta": { "a": 7 },
                "transferToAgent": "next"
            },
            "usageMetadata": {
                "promptTokenCount": 2,
                "candidatesTokenCount": 3,
                "totalTokenCount": 5,
                "vendorExtension": { "future": true }
            },
            "finishReason": "STOP",
            "output": { "future": true }
        });
        let payload: VertexEventPayload = serde_json::from_value(serde_json::json!({
            "name": "projects/p/locations/l/reasoningEngines/1/sessions/s/events/canonical-id",
            "timestamp": "2026-01-02T03:04:05Z",
            "invocationId": "canonical-invocation",
            "author": "canonical-author",
            "content": {
                "parts": [{ "text": "from direct raw" }]
            },
            "actions": {
                "stateDelta": { "deleted": null },
                "artifactDelta": { "a": 7 },
                "transferAgent": "next"
            },
            "rawEvent": direct_raw
        }))
        .expect("parse direct google ADK event");

        let event = payload.try_into_event().expect("direct google ADK event must load");
        assert_eq!(event.id, "canonical-id");
        assert_eq!(event.invocation_id, "canonical-invocation");
        assert_eq!(event.author, "canonical-author");
        assert_eq!(event.actions.state_delta.get("deleted"), Some(&Value::Null));
        assert_eq!(event.actions.transfer_to_agent.as_deref(), Some("next"));
        assert_eq!(event.llm_response.usage_metadata.as_ref().unwrap().total_token_count, 5);
        let preserved: Value = serde_json::from_str(
            event
                .provider_metadata
                .get(VERTEX_RAW_EVENT_METADATA_KEY)
                .expect("direct raw event preserved"),
        )
        .expect("direct raw JSON");
        assert_eq!(preserved["output"]["future"], true);
        assert_eq!(preserved["usageMetadata"]["vendorExtension"]["future"], true);
    }

    #[test]
    fn test_google_adk_direct_raw_event_reappend_preserves_unknown_fields_without_growth() {
        let canonical_content = serde_json::json!({
            "parts": [{
                "inlineData": {
                    "mimeType": "application/octet-stream",
                    "data": "+/8="
                },
                "videoMetadata": {
                    "startOffset": "1s",
                    "future": { "nested": true }
                }
            }]
        });
        let direct_content = serde_json::json!({
            "parts": [{
                "inlineData": {
                    "mimeType": "application/octet-stream",
                    "data": "-_8="
                },
                "videoMetadata": {
                    "startOffset": "1s",
                    "future": { "nested": true }
                }
            }]
        });
        let payload: VertexEventPayload = serde_json::from_value(serde_json::json!({
            "name": "projects/p/locations/l/reasoningEngines/1/sessions/s/events/canonical-id",
            "timestamp": "2026-01-02T03:04:05Z",
            "invocationId": "canonical-invocation",
            "author": "canonical-author",
            "content": canonical_content,
            "actions": {
                "stateDelta": { "deleted": null }
            },
            "rawEvent": {
                "id": "private-id",
                "timestamp": 1.0,
                "invocationId": "private-invocation",
                "author": "private-author",
                "content": direct_content,
                "actions": {
                    "stateDelta": { "deleted": null },
                    "route": ["review", "publish"],
                    "requestedToolConfirmations": {
                        "call-1": { "future": { "nested": true } }
                    },
                    "compaction": { "summary": "kept" },
                    "endOfAgent": true
                },
                "usageMetadata": {
                    "promptTokenCount": 2,
                    "candidatesTokenCount": 3,
                    "totalTokenCount": 5,
                    "vendor": { "nested": { "kept": true } }
                },
                "finishReason": "FUTURE_FINISH_REASON",
                "citationMetadata": {
                    "future": { "nested": { "kept": true } }
                },
                "futureRoot": { "nested": { "kept": true } }
            }
        }))
        .expect("parse direct google ADK event");

        let event = payload.try_into_event().expect("restore direct google ADK event");
        assert_eq!(event.id, "canonical-id");
        assert_eq!(event.invocation_id, "canonical-invocation");
        assert_eq!(event.author, "canonical-author");
        assert_eq!(event.timestamp.to_rfc3339(), "2026-01-02T03:04:05+00:00");
        let Part::InlineData { data, .. } = &event.llm_response.content.as_ref().unwrap().parts[0]
        else {
            panic!("expected inline data");
        };
        assert_eq!(data, &[251, 255]);

        let first = build_append_event_payload(&event).expect("first append");
        assert_eq!(first["content"]["parts"][0]["inlineData"]["data"], "+/8=");
        assert_eq!(first["rawEvent"]["content"]["parts"][0]["inlineData"]["data"], "-_8=");
        assert_eq!(first["rawEvent"]["actions"]["route"][1], "publish");
        assert_eq!(
            first["rawEvent"]["actions"]["requestedToolConfirmations"]["call-1"]["future"]["nested"],
            true
        );
        assert_eq!(first["rawEvent"]["actions"]["compaction"]["summary"], "kept");
        assert_eq!(first["rawEvent"]["actions"]["endOfAgent"], true);
        assert_eq!(first["rawEvent"]["usageMetadata"]["vendor"]["nested"]["kept"], true);
        assert_eq!(first["rawEvent"]["finishReason"], "FUTURE_FINISH_REASON");
        assert_eq!(first["rawEvent"]["citationMetadata"]["future"]["nested"]["kept"], true);
        assert_eq!(first["rawEvent"]["futureRoot"]["nested"]["kept"], true);
        assert_eq!(first["rawEvent"]["id"], "private-id");
        assert_eq!(first["rawEvent"]["timestamp"], 1.0);
        assert_eq!(first["rawEvent"]["invocationId"], "private-invocation");
        assert_eq!(first["rawEvent"]["author"], "private-author");

        let mut stored = first.clone();
        stored["name"] = Value::String(
            "projects/p/locations/l/reasoningEngines/1/sessions/s/events/canonical-id".to_string(),
        );
        let stored: VertexEventPayload =
            serde_json::from_value(stored).expect("parse first appended event");
        let restored = stored.try_into_event().expect("restore first appended event");
        let second = build_append_event_payload(&restored).expect("second append");

        let mut first_direct = first["rawEvent"].clone();
        first_direct.as_object_mut().unwrap().remove(RUST_RAW_EVENT_ENVELOPE_KEY);
        let mut second_direct = second["rawEvent"].clone();
        second_direct.as_object_mut().unwrap().remove(RUST_RAW_EVENT_ENVELOPE_KEY);
        assert_eq!(second_direct, first_direct);
        assert_eq!(second["rawEvent"]["id"], "private-id");
        assert_eq!(second["rawEvent"]["timestamp"], 1.0);
        assert_eq!(second["rawEvent"]["invocationId"], "private-invocation");
        assert_eq!(second["rawEvent"]["author"], "private-author");

        let embedded: Event = serde_json::from_str(
            second["rawEvent"][RUST_RAW_EVENT_ENVELOPE_KEY]["adkEvent"].as_str().unwrap(),
        )
        .expect("parse second private event");
        let preserved: Value = serde_json::from_str(
            embedded.provider_metadata.get(VERTEX_RAW_EVENT_METADATA_KEY).unwrap(),
        )
        .expect("parse preserved direct event");
        assert!(preserved.get(RUST_RAW_EVENT_ENVELOPE_KEY).is_none());
        assert_eq!(preserved["id"], "private-id");
        assert_eq!(preserved["timestamp"], 1.0);
        assert_eq!(preserved["invocationId"], "private-invocation");
        assert_eq!(preserved["author"], "private-author");
    }

    #[test]
    fn test_google_adk_direct_raw_event_mismatches_fall_back_to_opaque_preservation() {
        fn assert_opaque_projection_fallback(payload: Value) {
            let expected_raw_event = payload["rawEvent"].clone();
            let payload: VertexEventPayload =
                serde_json::from_value(payload).expect("parse incompatible direct event");
            let event = payload
                .try_into_event()
                .expect("incompatible direct projection must not reject the canonical event");
            assert_eq!(
                event.llm_response.content.as_ref().unwrap().parts,
                [Part::Text { text: "canonical".to_string() }]
            );
            assert_eq!(event.actions.state_delta.get("count"), Some(&Value::from(1)));
            assert!(!event.llm_response.partial);

            let preserved: Value = serde_json::from_str(
                event.provider_metadata.get(VERTEX_RAW_EVENT_METADATA_KEY).unwrap(),
            )
            .expect("parse opaque rawEvent sidecar");
            assert_eq!(preserved, expected_raw_event);
            let appended =
                build_append_event_payload(&event).expect("reappend opaque direct rawEvent");
            let mut reappended = appended["rawEvent"].clone();
            reappended.as_object_mut().unwrap().remove(RUST_RAW_EVENT_ENVELOPE_KEY);
            assert_eq!(reappended, expected_raw_event);
        }

        let payload = serde_json::json!({
            "name": "projects/p/locations/l/reasoningEngines/1/sessions/s/events/e",
            "timestamp": "2026-01-02T03:04:05Z",
            "invocationId": "canonical-invocation",
            "author": "canonical-author",
            "content": {
                "parts": [{ "text": "canonical" }]
            },
            "actions": {
                "stateDelta": { "count": 1 }
            },
            "rawEvent": {
                "id": "python-client-id",
                "timestamp": 1.0,
                "invocationId": "python-invocation",
                "author": "python-author",
                "content": {
                    "parts": [{ "text": "canonical" }]
                },
                "actions": {
                    "stateDelta": { "count": 1 }
                }
            }
        });

        let valid: VertexEventPayload =
            serde_json::from_value(payload.clone()).expect("parse valid direct event");
        let event = valid.try_into_event().expect("matching overlaps must load");
        assert_eq!(event.id, "e");
        assert_eq!(event.invocation_id, "canonical-invocation");
        assert_eq!(event.author, "canonical-author");

        let mut content_mismatch = payload.clone();
        content_mismatch["rawEvent"]["content"]["parts"][0]["text"] =
            Value::String("tampered".to_string());
        assert_opaque_projection_fallback(content_mismatch);

        let mut state_mismatch = payload.clone();
        state_mismatch["rawEvent"]["actions"]["stateDelta"]["count"] = Value::from(2);
        assert_opaque_projection_fallback(state_mismatch);

        let mut metadata_mismatch = payload.clone();
        metadata_mismatch["rawEvent"]["partial"] = Value::Bool(true);
        assert_opaque_projection_fallback(metadata_mismatch);

        let mut unsupported_optional = payload;
        unsupported_optional["rawEvent"]["interactionId"] =
            serde_json::json!({ "unexpected": true });
        assert_opaque_projection_fallback(unsupported_optional);
    }

    #[test]
    fn test_google_adk_direct_raw_event_preserves_absent_and_null_defaults() {
        let payload: VertexEventPayload = serde_json::from_value(serde_json::json!({
            "name": "projects/p/locations/l/reasoningEngines/1/sessions/s/events/e",
            "timestamp": "2026-01-02T03:04:05Z",
            "invocationId": "canonical-invocation",
            "author": "canonical-author",
            "rawEvent": {
                "id": "python-client-id",
                "timestamp": 1.0,
                "invocationId": "python-invocation",
                "author": "python-author",
                "partial": null,
                "futureRoot": { "kept": true }
            }
        }))
        .expect("parse direct event defaults");

        let event = payload.try_into_event().expect("direct event defaults must load");
        let appended = build_append_event_payload(&event).expect("reappend direct event defaults");
        let raw = appended["rawEvent"].as_object().expect("rawEvent object");
        assert!(raw.get("partial").is_some_and(Value::is_null));
        for field in ["turnComplete", "interrupted", "branch", "longRunningToolIds"] {
            assert!(!raw.contains_key(field), "{field} must remain absent");
        }
        assert_eq!(raw["futureRoot"]["kept"], true);
    }

    #[test]
    fn test_arbitrary_official_raw_event_round_trips_opaquely() {
        let arbitrary = serde_json::json!({
            "client": "custom-runtime",
            "timestamp": 1.0,
            "invocationId": "near-collision-invocation",
            "author": "near-collision-author",
            "nested": {
                "nullable": null,
                "wide": "9223372036854775807",
                "items": [1, true, { "future": "kept" }]
            }
        });
        let payload: VertexEventPayload = serde_json::from_value(serde_json::json!({
            "name": "projects/p/locations/l/reasoningEngines/1/sessions/s/events/e",
            "timestamp": "2026-01-02T03:04:05Z",
            "invocationId": "invocation",
            "author": "author",
            "rawEvent": arbitrary
        }))
        .expect("parse arbitrary raw event");

        let event = payload.try_into_event().expect("arbitrary raw event must load");
        assert_eq!(event.id, "e");
        assert_eq!(event.invocation_id, "invocation");
        assert_eq!(event.author, "author");
        assert_eq!(event.timestamp.to_rfc3339(), "2026-01-02T03:04:05+00:00");
        let appended = build_append_event_payload(&event).expect("reappend arbitrary raw event");
        let mut reappended = appended["rawEvent"].clone();
        reappended.as_object_mut().unwrap().remove(RUST_RAW_EVENT_ENVELOPE_KEY);
        assert_eq!(reappended, arbitrary);
        assert!(reappended.get("id").is_none());
    }

    #[test]
    fn test_canonical_content_sidecar_preserves_current_official_part_shapes() {
        let canonical_content = serde_json::json!({
            "parts": [
                {
                    "functionCall": {
                        "args": { "count": 1 },
                        "partialArgs": [{
                            "jsonPath": "$.city",
                            "stringValue": "Nairobi",
                            "willContinue": false
                        }],
                        "willContinue": true
                    },
                    "thought": true,
                    "thoughtSignature": "YQ=="
                },
                {
                    "functionResponse": {
                        "name": "lookup",
                        "response": { "ok": true, "count": 1 },
                        "parts": [
                            {
                                "fileData": {
                                    "mimeType": "text/plain",
                                    "fileUri": "gs://bucket/result.txt",
                                    "displayName": "result.txt"
                                }
                            },
                            {
                                "inlineData": {
                                    "mimeType": "application/octet-stream",
                                    "data": "YQ==",
                                    "displayName": "inline.bin"
                                }
                            }
                        ]
                    },
                    "thought": true,
                    "thoughtSignature": "YQ=="
                },
                {
                    "inlineData": {
                        "mimeType": "video/mp4",
                        "data": "YQ=="
                    },
                    "videoMetadata": { "startOffset": "1s", "endOffset": "2s" },
                    "thought": true,
                    "thoughtSignature": "YQ=="
                },
                {
                    "executableCode": { "language": "PYTHON", "code": "print('ok')" }
                },
                {
                    "codeExecutionResult": { "outcome": "OUTCOME_OK", "output": "ok" }
                }
            ]
        });
        let payload: VertexEventPayload = serde_json::from_value(serde_json::json!({
            "name": "projects/p/locations/l/reasoningEngines/1/sessions/s/events/e",
            "timestamp": "2026-01-02T03:04:05Z",
            "invocationId": "inv-1",
            "author": "model",
            "content": canonical_content
        }))
        .expect("parse current official content");
        let mut event = payload.try_into_event().expect("current official content must load");
        let content = event.llm_response.content.as_mut().expect("projected content");
        let Part::FunctionCall { args, .. } = &mut content.parts[0] else {
            panic!("expected function call");
        };
        args["count"] =
            Value::Number(serde_json::Number::from_f64(1.0).expect("finite Struct number"));
        let Part::FunctionResponse { function_response, .. } = &mut content.parts[1] else {
            panic!("expected function response");
        };
        function_response.response["count"] =
            Value::Number(serde_json::Number::from_f64(1.0).expect("finite Struct number"));
        let appended = build_append_event_payload(&event).expect("reappend official content");
        assert_eq!(appended["content"], canonical_content);
        assert!(appended["content"].get("role").is_none());
    }

    #[test]
    fn test_ga_v1_media_resolution_is_validated_and_preserved() {
        let canonical_content = serde_json::json!({
            "role": "user",
            "parts": [
                {
                    "inlineData": {
                        "mimeType": "image/png",
                        "data": "YQ=="
                    },
                    "mediaResolution": {
                        "level": "MEDIA_RESOLUTION_HIGH",
                        "future": { "kept": true }
                    }
                },
                {
                    "fileData": {
                        "mimeType": "video/mp4",
                        "fileUri": "gs://bucket/video.mp4"
                    },
                    "mediaResolution": {
                        "level": "MEDIA_RESOLUTION_MEDIUM"
                    }
                },
                {
                    "text": "schema-valid placement",
                    "mediaResolution": {
                        "level": "MEDIA_RESOLUTION_LOW"
                    }
                }
            ]
        });
        let payload: VertexEventPayload = serde_json::from_value(serde_json::json!({
            "name": "projects/p/locations/l/reasoningEngines/1/sessions/s/events/media",
            "timestamp": "2026-01-02T03:04:05Z",
            "invocationId": "inv-media",
            "author": "user",
            "content": canonical_content
        }))
        .expect("parse GA v1 mediaResolution content");
        let event = payload.try_into_event().expect("valid mediaResolution must load");
        assert_eq!(event.llm_response.content.as_ref().unwrap().parts.len(), 3);
        let preserved: Value = serde_json::from_str(
            event.provider_metadata.get(VERTEX_CANONICAL_CONTENT_KEY).unwrap(),
        )
        .expect("parse preserved canonical content");
        assert_eq!(preserved, canonical_content);

        let first = build_append_event_payload(&event).expect("reappend mediaResolution content");
        assert_eq!(first["content"], canonical_content);
        let mut stored = first;
        stored["name"] = Value::String(
            "projects/p/locations/l/reasoningEngines/1/sessions/s/events/media".to_string(),
        );
        let stored: VertexEventPayload =
            serde_json::from_value(stored).expect("parse stored mediaResolution content");
        let restored = stored.try_into_event().expect("restore mediaResolution content");
        let second =
            build_append_event_payload(&restored).expect("reappend restored mediaResolution");
        assert_eq!(second["content"], canonical_content);

        let non_object: VertexContentPayload = serde_json::from_value(serde_json::json!({
            "parts": [{ "text": "invalid", "mediaResolution": "HIGH" }]
        }))
        .expect("parse non-object mediaResolution");
        let error =
            content_from_vertex(non_object).expect_err("non-object mediaResolution must fail");
        assert!(error.message.contains("mediaResolution must be an object"));

        let mut too_deep = Value::Null;
        for _ in 0..=VERTEX_VALUE_MAX_DEPTH {
            too_deep = serde_json::json!({ "nested": too_deep });
        }
        let deep_payload: VertexEventPayload = serde_json::from_value(serde_json::json!({
            "name": "projects/p/locations/l/reasoningEngines/1/sessions/s/events/deep-media",
            "timestamp": "2026-01-02T03:04:05Z",
            "invocationId": "inv-deep-media",
            "author": "user",
            "content": {
                "parts": [{ "text": "deep", "mediaResolution": too_deep }]
            }
        }))
        .expect("parse deep mediaResolution");
        let error =
            deep_payload.try_into_event().expect_err("deep mediaResolution must be bounded");
        assert!(error.message.contains("mediaResolution"));
        assert!(error.message.contains("nesting depth"));
    }

    #[test]
    fn test_top_level_media_display_names_are_preserved_by_canonical_sidecar() {
        let canonical_content = serde_json::json!({
            "parts": [
                {
                    "inlineData": {
                        "mimeType": "application/octet-stream",
                        "data": "YQ==",
                        "displayName": "inline.bin"
                    }
                },
                {
                    "fileData": {
                        "mimeType": "text/plain",
                        "fileUri": "gs://bucket/file.txt",
                        "displayName": "file.txt"
                    }
                }
            ]
        });
        let payload: VertexEventPayload = serde_json::from_value(serde_json::json!({
            "name": "projects/p/locations/l/reasoningEngines/1/sessions/s/events/display",
            "timestamp": "2026-01-02T03:04:05Z",
            "invocationId": "inv-display",
            "author": "user",
            "content": canonical_content
        }))
        .expect("parse top-level media display names");
        let event = payload.try_into_event().expect("top-level displayName fields must load");
        let projected = event.llm_response.content.as_ref().expect("projected content");
        assert!(matches!(projected.parts[0], Part::InlineData { .. }));
        assert!(matches!(projected.parts[1], Part::FileData { .. }));

        let first = build_append_event_payload(&event).expect("reappend displayName content");
        assert_eq!(first["content"], canonical_content);
        let mut stored = first;
        stored["name"] = Value::String(
            "projects/p/locations/l/reasoningEngines/1/sessions/s/events/display".to_string(),
        );
        let stored: VertexEventPayload =
            serde_json::from_value(stored).expect("parse stored displayName content");
        let restored = stored.try_into_event().expect("restore displayName content");
        let second = build_append_event_payload(&restored).expect("reappend restored displayName");
        assert_eq!(second["content"], canonical_content);

        let native = content_to_vertex(projected).expect("convert native ADK media");
        let native = serde_json::to_value(native).expect("serialize native media");
        assert!(native["parts"][0]["inlineData"].get("displayName").is_none());
        assert!(native["parts"][1]["fileData"].get("displayName").is_none());
    }

    #[test]
    fn test_function_call_partial_args_reject_malformed_value_shape() {
        let content: VertexContentPayload = serde_json::from_value(serde_json::json!({
            "parts": [{
                "functionCall": {
                    "name": "lookup",
                    "args": {},
                    "partialArgs": [{
                        "jsonPath": "$.city",
                        "value": "Nairobi"
                    }]
                }
            }]
        }))
        .expect("parse malformed partial args");
        let error = content_from_vertex(content).expect_err("malformed partialArgs must fail");
        assert!(error.message.contains("exactly one value field"));
    }

    #[test]
    fn test_function_response_interleaving_is_accepted() {
        let content: VertexContentPayload = serde_json::from_value(serde_json::json!({
            "role": "user",
            "parts": [{
                "functionResponse": {
                    "name": "tool",
                    "response": {},
                    "parts": [
                        { "fileData": { "mimeType": "text/plain", "fileUri": "gs://b/a" } },
                        { "inlineData": { "mimeType": "text/plain", "data": "YQ==" } }
                    ]
                }
            }]
        }))
        .expect("parse function response");
        let restored = content_from_vertex(content).expect("valid ordered parts must load");
        let Part::FunctionResponse { function_response, .. } = &restored.parts[0] else {
            panic!("expected function response");
        };
        assert_eq!(function_response.file_data.len(), 1);
        assert_eq!(function_response.inline_data.len(), 1);
    }

    #[tokio::test]
    async fn test_vertex_operation_rejects_contradictory_result_shapes() {
        use google_cloud_auth::credentials::api_key_credentials;

        // Malformed shapes are rejected while parsing the initial operation,
        // before any poll request is sent, so no mock server is needed.
        let service = VertexAiSessionService::with_credentials(
            VertexAiSessionConfig::new("p", "l"),
            api_key_credentials::Builder::new("test-key").build(),
        )
        .expect("build operation-shape service");

        let both = service
            .wait_for_operation(
                serde_json::json!({
                    "name": "projects/p/locations/l/operations/op",
                    "done": true,
                    "error": { "code": 13, "message": "failed" },
                    "response": {}
                }),
                "create session",
                false,
            )
            .await
            .expect_err("operation result oneof must reject both arms");
        assert!(both.message.contains("both error and response"));

        for result in [
            serde_json::json!({ "response": {} }),
            serde_json::json!({ "error": { "code": 13, "message": "failed" } }),
        ] {
            let mut operation = serde_json::json!({
                "name": "projects/p/locations/l/operations/op",
                "done": false
            });
            operation.as_object_mut().unwrap().extend(result.as_object().unwrap().clone());
            let error = service
                .wait_for_operation(operation, "create session", false)
                .await
                .expect_err("pending operation must reject terminal results");
            assert!(error.message.contains("done is false"));
        }
    }

    #[test]
    fn test_canonical_extensions_and_wide_provider_metadata_round_trip() {
        let payload: VertexEventPayload = serde_json::from_value(serde_json::json!({
            "name": "projects/p/locations/l/reasoningEngines/1/sessions/s/events/e",
            "timestamp": "2026-01-02T03:04:05Z",
            "invocationId": "inv-1",
            "author": "model",
            "actions": {
                "requestedAuthConfigs": { "call-1": { "scheme": "oauth" } }
            },
            "eventMetadata": {
                "groundingMetadata": { "searchEntryPoint": { "renderedContent": "x" } },
                "inputTranscription": { "text": "in" },
                "outputTranscription": { "text": "out" },
                "customMetadata": { "vendor": { "nullable": null } }
            }
        }))
        .expect("parse canonical extensions");
        let mut event = payload.try_into_event().expect("canonical extensions load");
        event.llm_response.provider_metadata =
            Some(serde_json::json!({ "wide": 9223372036854775807_i64 }));

        let appended = build_append_event_payload(&event).expect("append payload");
        assert_eq!(appended["actions"]["requestedAuthConfigs"]["call-1"]["scheme"], "oauth");
        assert_eq!(
            appended["eventMetadata"]["groundingMetadata"]["searchEntryPoint"]["renderedContent"],
            "x"
        );
        assert_eq!(appended["eventMetadata"]["inputTranscription"]["text"], "in");
        assert_eq!(appended["eventMetadata"]["outputTranscription"]["text"], "out");
        assert!(appended["eventMetadata"]["customMetadata"]["vendor"]["nullable"].is_null());
        let mut stored = appended;
        stored["rawEvent"]["providerMetadata"]["wide"] =
            Value::Number(serde_json::Number::from_f64(9_223_372_036_854_776_000.0).unwrap());
        stored["name"] = Value::String(
            "projects/p/locations/l/reasoningEngines/1/sessions/s/events/e".to_string(),
        );
        let restored: VertexEventPayload =
            serde_json::from_value(stored).expect("parse stored event");
        let restored = restored.try_into_event().expect("restore stored event");
        assert_eq!(restored.llm_response.provider_metadata, event.llm_response.provider_metadata);
    }

    #[test]
    fn test_required_timestamps_and_error_categories() {
        let missing_timestamp: VertexEventPayload = serde_json::from_value(serde_json::json!({
            "name": "projects/p/locations/l/reasoningEngines/1/sessions/s/events/e",
            "invocationId": "inv-1",
            "author": "model"
        }))
        .expect("parse event");
        assert!(
            missing_timestamp
                .try_into_event()
                .expect_err("timestamp required")
                .message
                .contains("timestamp")
        );

        let session = VertexSessionPayload {
            name: "projects/p/locations/l/reasoningEngines/1/sessions/s".to_string(),
            user_id: "u".to_string(),
            session_state: HashMap::new(),
            create_time: Some("2026-01-02T03:04:05Z".to_string()),
            update_time: None,
        };
        assert_eq!(
            session_update_timestamp(&session, "test").unwrap(),
            parse_rfc3339_utc("2026-01-02T03:04:05Z").unwrap()
        );
        let missing_times = VertexSessionPayload { create_time: None, ..session };
        assert!(session_update_timestamp(&missing_times, "test").is_err());

        assert_eq!(
            gcp_error_context()
                .status_error(reqwest::StatusCode::TOO_MANY_REQUESTS, "slow")
                .category,
            ErrorCategory::RateLimited
        );
        assert_eq!(
            gcp_error_context()
                .status_error(reqwest::StatusCode::SERVICE_UNAVAILABLE, "down")
                .category,
            ErrorCategory::Unavailable
        );
        assert_eq!(
            VertexAiSessionService::invalid_input("bad").category,
            ErrorCategory::InvalidInput
        );
        for (code, category) in [
            (1, ErrorCategory::Cancelled),
            (10, ErrorCategory::Unavailable),
            (12, ErrorCategory::Unsupported),
        ] {
            let error = gcp_error_context().operation_error(
                "test",
                "projects/p/locations/l/operations/op",
                code,
                "test",
            );
            assert_eq!(error.category, category);
        }
        assert!(
            gcp_error_context()
                .operation_error("test", "projects/p/locations/l/operations/op", 10, "aborted")
                .is_retryable()
        );
        assert!(validate_empty_response(&serde_json::json!({}), "append event").is_ok());
        assert!(
            validate_delete_operation_response(&serde_json::json!({
                "@type": "type.googleapis.com/google.protobuf.Empty"
            }))
            .is_ok()
        );
        assert!(validate_delete_operation_response(&serde_json::json!({})).is_err());
        assert!(
            validate_delete_operation_response(&serde_json::json!({
                "@type": "type.googleapis.com/google.protobuf.Empty",
                "extra": true
            }))
            .is_err()
        );
        let create_response = serde_json::json!({
            "@type": "type.googleapis.com/google.cloud.aiplatform.v1.Session",
            "name": "projects/p/locations/l/reasoningEngines/1/sessions/s",
            "userId": "u"
        });
        assert!(parse_create_session_operation_response(create_response.clone()).is_ok());
        let mut missing_type = create_response.clone();
        missing_type.as_object_mut().unwrap().remove("@type");
        assert!(parse_create_session_operation_response(missing_type).is_err());
        let mut wrong_type = create_response;
        wrong_type["@type"] = Value::String(
            "type.googleapis.com/google.cloud.aiplatform.v1beta1.Session".to_string(),
        );
        assert!(parse_create_session_operation_response(wrong_type).is_err());
    }

    #[tokio::test]
    async fn test_operation_deadline_bounds_hung_http_poll() {
        use axum::{Json, Router, routing::get};
        use google_cloud_auth::credentials::api_key_credentials;

        let app = Router::new().route(
            "/v1/projects/test-project/locations/us-central1/operations/hang",
            get(|| async {
                tokio::time::sleep(Duration::from_secs(1)).await;
                Json(serde_json::json!({
                    "name": "projects/test-project/locations/us-central1/operations/hang",
                    "done": false
                }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });
        let service = VertexAiSessionService::with_credentials(
            VertexAiSessionConfig::new("test-project", "us-central1")
                .with_endpoint(format!("http://{address}")),
            api_key_credentials::Builder::new("test-key").build(),
        )
        .expect("build test service");
        let start = Instant::now();
        let error = service
            .wait_for_operation_with_timeout(
                serde_json::json!({
                    "name": "projects/test-project/locations/us-central1/operations/hang",
                    "done": false
                }),
                "test",
                false,
                Duration::from_millis(30),
            )
            .await
            .expect_err("hung poll must time out");
        assert_eq!(error.category, ErrorCategory::Timeout);
        assert!(start.elapsed() < Duration::from_millis(500));
        server.abort();
    }

    #[derive(Debug)]
    struct HangingCredentials;

    impl google_cloud_auth::credentials::CredentialsProvider for HangingCredentials {
        fn headers(
            &self,
            _extensions: http::Extensions,
        ) -> impl std::future::Future<
            Output = std::result::Result<
                google_cloud_auth::credentials::CacheableResource<http::HeaderMap>,
                google_cloud_auth::errors::CredentialsError,
            >,
        > + Send {
            std::future::pending()
        }

        fn universe_domain(&self) -> impl std::future::Future<Output = Option<String>> + Send {
            std::future::ready(None)
        }
    }

    #[tokio::test]
    async fn test_auth_header_acquisition_is_bounded() {
        // The bound now lives in adk-gcp's client; this pins the
        // session-branded behavior with the backend's own error identity.
        let client = GcpHttpClient::builder(
            gcp_error_context(),
            "https://us-central1-aiplatform.googleapis.com",
        )
        .auth_timeout(Duration::from_millis(20))
        .credentials(Credentials::from(HangingCredentials))
        .build()
        .expect("build bounded-auth client");
        let start = Instant::now();
        let error = client.auth_headers().await.expect_err("hung credential refresh must time out");
        assert_eq!(error.category, ErrorCategory::Timeout);
        assert_eq!(error.code, "session.vertex.timeout");
        assert!(start.elapsed() < Duration::from_millis(500));
    }
}
