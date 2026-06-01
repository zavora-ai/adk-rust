//! ManagedAgentsClient implementation.
//!
//! Provides the primary entry point for all Managed Agents API operations,
//! including agent, environment, and session CRUD, event dispatch, and SSE streaming.

use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use futures::stream::Stream;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};

use super::dreams::{CreateDreamParams, Dream, DreamListResponse};
use super::events::{SessionEvent, UserEvent};
use super::memory::{
    CreateMemoryParams, CreateMemoryStoreParams, Memory, MemoryListResponse, MemoryStore,
    MemoryVersion, UpdateMemoryParams,
};
use super::stream::process_managed_agents_sse;
use super::types::{
    Agent, CreateAgentParams, CreateEnvironmentParams, CreateSessionParams, Environment,
    ListResponse, ListSessionsParams, Session, SessionResourceResponse, SessionThread,
};
use super::vaults::{
    CreateCredentialParams, CreateVaultParams, Credential, CredentialValidation,
    UpdateCredentialParams, Vault, VaultListResponse,
};
use crate::{Error, Result};

/// Default base URL for the Anthropic API.
const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";

/// Default SSE stream timeout in seconds.
const DEFAULT_SSE_TIMEOUT_SECS: u64 = 300;

/// Client for the Anthropic Managed Agents API.
///
/// This is a direct-client surface for managing long-running agent sessions.
/// It is NOT wired into the `adk-runner` `Agent` trait — managed agent sessions
/// are stateful, SSE-driven, and long-running (minutes to hours).
///
/// All requests include the beta header `managed-agents-2026-04-01`.
///
/// # Example
///
/// ```rust,ignore
/// use adk_anthropic::managed_agents::ManagedAgentsClient;
///
/// let client = ManagedAgentsClient::new("sk-ant-api03-...")?;
/// ```
#[derive(Debug, Clone)]
pub struct ManagedAgentsClient {
    pub(crate) client: reqwest::Client,
    #[allow(dead_code)] // Retained for potential reconnection/refresh scenarios
    pub(crate) api_key: String,
    pub(crate) base_url: String,
    pub(crate) sse_timeout: Duration,
    pub(crate) cached_headers: Arc<HeaderMap>,
}

impl ManagedAgentsClient {
    /// Create a new client from an API key.
    ///
    /// Uses the default base URL (`https://api.anthropic.com`) and default
    /// SSE timeout of 300 seconds.
    ///
    /// # Arguments
    ///
    /// * `api_key` - The Anthropic API key for authentication.
    ///
    /// # Errors
    ///
    /// Returns an error if the API key contains invalid header characters.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_anthropic::managed_agents::ManagedAgentsClient;
    ///
    /// let client = ManagedAgentsClient::new("sk-ant-api03-...")?;
    /// ```
    pub fn new(api_key: impl Into<String>) -> Result<Self> {
        let api_key = api_key.into();
        let cached_headers = Arc::new(build_headers(&api_key)?);

        Ok(Self {
            client: reqwest::Client::new(),
            api_key,
            base_url: DEFAULT_BASE_URL.to_string(),
            sse_timeout: Duration::from_secs(DEFAULT_SSE_TIMEOUT_SECS),
            cached_headers,
        })
    }

    /// Create a new client from the `ANTHROPIC_API_KEY` environment variable.
    ///
    /// # Errors
    ///
    /// Returns an `Error::Authentication` if the environment variable is not set
    /// or contains invalid header characters.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_anthropic::managed_agents::ManagedAgentsClient;
    ///
    /// let client = ManagedAgentsClient::from_env()?;
    /// ```
    pub fn from_env() -> Result<Self> {
        let api_key = std::env::var("ANTHROPIC_API_KEY").map_err(|_| Error::Authentication {
            message: "ANTHROPIC_API_KEY environment variable is not set".to_string(),
        })?;

        Self::new(api_key)
    }

    /// Override the base URL (for testing or proxies).
    ///
    /// # Arguments
    ///
    /// * `base_url` - The custom base URL to use for API requests.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_anthropic::managed_agents::ManagedAgentsClient;
    ///
    /// let client = ManagedAgentsClient::new("sk-ant-api03-...")?
    ///     .with_base_url("https://my-proxy.example.com");
    /// ```
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// Override the SSE stream timeout (default: 300 seconds).
    ///
    /// This timeout controls how long the client waits for new data on an
    /// SSE stream before considering the connection stale.
    ///
    /// # Arguments
    ///
    /// * `timeout` - The new timeout duration.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use std::time::Duration;
    /// use adk_anthropic::managed_agents::ManagedAgentsClient;
    ///
    /// let client = ManagedAgentsClient::new("sk-ant-api03-...")?
    ///     .with_sse_timeout(Duration::from_secs(600));
    /// ```
    pub fn with_sse_timeout(mut self, timeout: Duration) -> Self {
        self.sse_timeout = timeout;
        self
    }

    /// Build the full URL for an API endpoint.
    ///
    /// Constructs the URL by combining the base URL with the `/v1/` prefix
    /// and the given endpoint path. The beta access is controlled via the
    /// `anthropic-beta` header, not the URL path.
    pub(crate) fn build_url(&self, endpoint: &str) -> String {
        let base = self.base_url.trim_end_matches('/');
        format!("{base}/v1/{endpoint}")
    }

    // ─── Environment CRUD ────────────────────────────────────────────────────

    /// Create a new sandbox environment.
    ///
    /// Creates an environment via `POST /environments` with the specified
    /// sandbox configuration (cloud or self-hosted).
    ///
    /// # Arguments
    ///
    /// * `params` - The environment creation parameters including sandbox config.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or the API returns an error response.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_anthropic::managed_agents::{
    ///     ManagedAgentsClient, CreateEnvironmentParams, SandboxConfig,
    /// };
    ///
    /// let client = ManagedAgentsClient::new("sk-ant-api03-...")?;
    /// let env = client.create_environment(CreateEnvironmentParams::cloud("my-env")).await?;
    /// println!("Created environment: {}", env.id);
    /// ```
    pub async fn create_environment(&self, params: CreateEnvironmentParams) -> Result<Environment> {
        let url = self.build_url("environments");
        let response = self
            .client
            .post(&url)
            .headers((*self.cached_headers).clone())
            .json(&params)
            .send()
            .await
            .map_err(|e| {
                Error::connection(format!("failed to send create_environment request: {e}"), None)
            })?;

        handle_response(response).await
    }

    /// Retrieve an environment by ID.
    ///
    /// Fetches an environment via `GET /environments/{id}`.
    ///
    /// # Arguments
    ///
    /// * `environment_id` - The unique identifier of the environment to retrieve.
    ///
    /// # Errors
    ///
    /// Returns an error if the environment is not found or the request fails.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_anthropic::managed_agents::ManagedAgentsClient;
    ///
    /// let client = ManagedAgentsClient::new("sk-ant-api03-...")?;
    /// let env = client.get_environment("env_abc123").await?;
    /// println!("Environment sandbox: {:?}", env.sandbox);
    /// ```
    pub async fn get_environment(&self, environment_id: &str) -> Result<Environment> {
        let url = self.build_url(&format!("environments/{environment_id}"));
        let response =
            self.client.get(&url).headers((*self.cached_headers).clone()).send().await.map_err(
                |e| Error::connection(format!("failed to send get_environment request: {e}"), None),
            )?;

        handle_response(response).await
    }

    /// Delete an environment by ID.
    ///
    /// Deletes an environment via `DELETE /environments/{id}`. On success,
    /// the API returns 204 No Content.
    ///
    /// # Arguments
    ///
    /// * `environment_id` - The unique identifier of the environment to delete.
    ///
    /// # Errors
    ///
    /// Returns an error if the environment is not found or the request fails.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_anthropic::managed_agents::ManagedAgentsClient;
    ///
    /// let client = ManagedAgentsClient::new("sk-ant-api03-...")?;
    /// client.delete_environment("env_abc123").await?;
    /// ```
    pub async fn delete_environment(&self, environment_id: &str) -> Result<()> {
        let url = self.build_url(&format!("environments/{environment_id}"));
        let response = self
            .client
            .delete(&url)
            .headers((*self.cached_headers).clone())
            .send()
            .await
            .map_err(|e| {
                Error::connection(format!("failed to send delete_environment request: {e}"), None)
            })?;

        handle_empty_response(response).await
    }
}

// ─── Agent CRUD ──────────────────────────────────────────────────────────────

impl ManagedAgentsClient {
    /// Create a new managed agent configuration.
    ///
    /// Sends a `POST /agents` request with the given parameters and returns
    /// the created agent with its server-assigned ID and timestamps.
    ///
    /// # Arguments
    ///
    /// * `params` - The agent configuration including model, system prompt, tools, and MCP servers.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or the server returns an error response.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_anthropic::managed_agents::{ManagedAgentsClient, CreateAgentParams};
    ///
    /// let client = ManagedAgentsClient::new("sk-ant-api03-...")?;
    /// let agent = client.create_agent(CreateAgentParams {
    ///     name: "My Agent".to_string(),
    ///     model: serde_json::json!("claude-sonnet-4-6"),
    ///     system: Some("You are a helpful assistant.".to_string()),
    ///     description: None,
    ///     tools: vec![],
    ///     mcp_servers: vec![],
    ///     skills: vec![],
    ///     metadata: None,
    /// }).await?;
    /// println!("Created agent: {}", agent.id);
    /// ```
    pub async fn create_agent(&self, params: CreateAgentParams) -> Result<Agent> {
        let url = self.build_url("agents");
        let response = self
            .client
            .post(&url)
            .headers((*self.cached_headers).clone())
            .json(&params)
            .send()
            .await
            .map_err(|e| {
                Error::connection(format!("failed to send create_agent request: {e}"), None)
            })?;

        handle_response(response).await
    }

    /// List all managed agent configurations.
    ///
    /// Sends a `GET /agents` request and returns all agents associated with
    /// the authenticated account.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or the server returns an error response.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_anthropic::managed_agents::ManagedAgentsClient;
    ///
    /// let client = ManagedAgentsClient::new("sk-ant-api03-...")?;
    /// let agents = client.list_agents().await?;
    /// for agent in &agents {
    ///     println!("{}: {}", agent.id, agent.model);
    /// }
    /// ```
    pub async fn list_agents(&self) -> Result<Vec<Agent>> {
        let url = self.build_url("agents");
        let response =
            self.client.get(&url).headers((*self.cached_headers).clone()).send().await.map_err(
                |e| Error::connection(format!("failed to send list_agents request: {e}"), None),
            )?;

        let list: ListResponse<Agent> = handle_response(response).await?;
        Ok(list.data)
    }

    /// Retrieve a single managed agent by ID.
    ///
    /// Sends a `GET /agents/{id}` request and returns the agent configuration.
    ///
    /// # Arguments
    ///
    /// * `agent_id` - The unique identifier of the agent to retrieve.
    ///
    /// # Errors
    ///
    /// Returns `Error::NotFound` if the agent does not exist, or another error
    /// if the request fails.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_anthropic::managed_agents::ManagedAgentsClient;
    ///
    /// let client = ManagedAgentsClient::new("sk-ant-api03-...")?;
    /// let agent = client.get_agent("agent_abc123").await?;
    /// println!("Agent model: {}", agent.model);
    /// ```
    pub async fn get_agent(&self, agent_id: &str) -> Result<Agent> {
        let url = self.build_url(&format!("agents/{agent_id}"));
        let response =
            self.client.get(&url).headers((*self.cached_headers).clone()).send().await.map_err(
                |e| Error::connection(format!("failed to send get_agent request: {e}"), None),
            )?;

        handle_response(response).await
    }

    /// Delete a managed agent by ID.
    ///
    /// Sends a `DELETE /agents/{id}` request. On success (204 No Content),
    /// returns `Ok(())`.
    ///
    /// # Arguments
    ///
    /// * `agent_id` - The unique identifier of the agent to delete.
    ///
    /// # Errors
    ///
    /// Returns `Error::NotFound` if the agent does not exist, or another error
    /// if the request fails.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_anthropic::managed_agents::ManagedAgentsClient;
    ///
    /// let client = ManagedAgentsClient::new("sk-ant-api03-...")?;
    /// client.delete_agent("agent_abc123").await?;
    /// println!("Agent deleted successfully");
    /// ```
    pub async fn delete_agent(&self, agent_id: &str) -> Result<()> {
        let url = self.build_url(&format!("agents/{agent_id}"));
        let response =
            self.client.delete(&url).headers((*self.cached_headers).clone()).send().await.map_err(
                |e| Error::connection(format!("failed to send delete_agent request: {e}"), None),
            )?;

        handle_empty_response(response).await
    }
}

// ─── Session CRUD ────────────────────────────────────────────────────────────

impl ManagedAgentsClient {
    /// Create a new session referencing an agent and environment.
    ///
    /// Sends a `POST /sessions` request with the given parameters and returns
    /// the created session with its server-assigned ID, initial status, and timestamps.
    ///
    /// # Arguments
    ///
    /// * `params` - The session creation parameters including agent ID and environment ID.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or the server returns an error response.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_anthropic::managed_agents::{ManagedAgentsClient, CreateSessionParams};
    ///
    /// let client = ManagedAgentsClient::new("sk-ant-api03-...")?;
    /// let session = client.create_session(CreateSessionParams::new(
    ///     "agent_abc123",
    ///     "env_abc123",
    /// )).await?;
    /// println!("Created session: {} (status: {:?})", session.id, session.status);
    /// ```
    pub async fn create_session(&self, params: CreateSessionParams) -> Result<Session> {
        let url = self.build_url("sessions");
        let response = self
            .client
            .post(&url)
            .headers((*self.cached_headers).clone())
            .json(&params)
            .send()
            .await
            .map_err(|e| {
                Error::connection(format!("failed to send create_session request: {e}"), None)
            })?;

        handle_response(response).await
    }

    /// Retrieve a session by ID.
    ///
    /// Fetches a session via `GET /sessions/{id}`, including its current status
    /// and usage tracking information.
    ///
    /// # Arguments
    ///
    /// * `session_id` - The unique identifier of the session to retrieve.
    ///
    /// # Errors
    ///
    /// Returns `Error::NotFound` if the session does not exist, or another error
    /// if the request fails.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_anthropic::managed_agents::ManagedAgentsClient;
    ///
    /// let client = ManagedAgentsClient::new("sk-ant-api03-...")?;
    /// let session = client.get_session("sess_abc123").await?;
    /// println!("Session status: {:?}, tokens used: {}", session.status, session.usage.input_tokens);
    /// ```
    pub async fn get_session(&self, session_id: &str) -> Result<Session> {
        let url = self.build_url(&format!("sessions/{session_id}"));
        let response =
            self.client.get(&url).headers((*self.cached_headers).clone()).send().await.map_err(
                |e| Error::connection(format!("failed to send get_session request: {e}"), None),
            )?;

        handle_response(response).await
    }

    /// List sessions with optional filtering parameters.
    ///
    /// Sends a `GET /sessions` request with optional query parameters for filtering.
    /// If `params` is `None`, no query parameters are sent and all sessions are returned.
    ///
    /// # Arguments
    ///
    /// * `params` - Optional filtering parameters (agent_id, limit).
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails or the server returns an error response.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_anthropic::managed_agents::{ManagedAgentsClient, ListSessionsParams};
    ///
    /// let client = ManagedAgentsClient::new("sk-ant-api03-...")?;
    ///
    /// // List all sessions
    /// let sessions = client.list_sessions(None).await?;
    ///
    /// // List sessions filtered by agent ID
    /// let sessions = client.list_sessions(Some(ListSessionsParams {
    ///     agent_id: Some("agent_abc123".to_string()),
    ///     limit: Some(10),
    /// })).await?;
    /// for session in &sessions {
    ///     println!("{}: {:?}", session.id, session.status);
    /// }
    /// ```
    pub async fn list_sessions(&self, params: Option<ListSessionsParams>) -> Result<Vec<Session>> {
        let url = self.build_url("sessions");
        let mut request = self.client.get(&url).headers((*self.cached_headers).clone());

        if let Some(params) = &params {
            if let Some(agent_id) = &params.agent_id {
                request = request.query(&[("agent_id", agent_id.as_str())]);
            }
            if let Some(limit) = params.limit {
                request = request.query(&[("limit", &limit.to_string())]);
            }
        }

        let response = request.send().await.map_err(|e| {
            Error::connection(format!("failed to send list_sessions request: {e}"), None)
        })?;

        let list: ListResponse<Session> = handle_response(response).await?;
        Ok(list.data)
    }

    /// Archive a session.
    ///
    /// Archives a session via `POST /sessions/{id}/archive`. This transitions
    /// the session to `terminated` status. On success, the API returns an empty
    /// response.
    ///
    /// # Arguments
    ///
    /// * `session_id` - The unique identifier of the session to archive.
    ///
    /// # Errors
    ///
    /// Returns an error if the session is not found or the request fails.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_anthropic::managed_agents::ManagedAgentsClient;
    ///
    /// let client = ManagedAgentsClient::new("sk-ant-api03-...")?;
    /// client.archive_session("sess_abc123").await?;
    /// println!("Session archived successfully");
    /// ```
    pub async fn archive_session(&self, session_id: &str) -> Result<()> {
        let url = self.build_url(&format!("sessions/{session_id}/archive"));
        let response =
            self.client.post(&url).headers((*self.cached_headers).clone()).send().await.map_err(
                |e| Error::connection(format!("failed to send archive_session request: {e}"), None),
            )?;

        handle_empty_response(response).await
    }

    /// Delete a session by ID.
    ///
    /// Deletes a session via `DELETE /sessions/{id}`. On success, the API
    /// returns 204 No Content.
    ///
    /// # Arguments
    ///
    /// * `session_id` - The unique identifier of the session to delete.
    ///
    /// # Errors
    ///
    /// Returns `Error::NotFound` if the session does not exist, or another error
    /// if the request fails.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_anthropic::managed_agents::ManagedAgentsClient;
    ///
    /// let client = ManagedAgentsClient::new("sk-ant-api03-...")?;
    /// client.delete_session("sess_abc123").await?;
    /// println!("Session deleted successfully");
    /// ```
    pub async fn delete_session(&self, session_id: &str) -> Result<()> {
        let url = self.build_url(&format!("sessions/{session_id}"));
        let response =
            self.client.delete(&url).headers((*self.cached_headers).clone()).send().await.map_err(
                |e| Error::connection(format!("failed to send delete_session request: {e}"), None),
            )?;

        handle_empty_response(response).await
    }
}

// ─── Event Dispatch ──────────────────────────────────────────────────────────

impl ManagedAgentsClient {
    /// Send a user event to a session.
    ///
    /// Serializes the `UserEvent` and POSTs it to `POST /sessions/{id}/events`.
    /// On success, the API returns 200 or 204 with an empty body.
    ///
    /// # Arguments
    ///
    /// * `session_id` - The unique identifier of the session to send the event to.
    /// * `event` - The user event to send (message, interrupt, tool result, etc.).
    ///
    /// # Errors
    ///
    /// Returns an error if the session is not found, the session is terminated,
    /// or the request fails.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_anthropic::managed_agents::{ManagedAgentsClient, UserEvent};
    ///
    /// let client = ManagedAgentsClient::new("sk-ant-api03-...")?;
    ///
    /// // Send a message to a session
    /// client.send_event("sess_abc123", UserEvent::Message {
    ///     content: "Hello, agent!".to_string(),
    /// }).await?;
    ///
    /// // Interrupt a running session
    /// client.send_event("sess_abc123", UserEvent::Interrupt {}).await?;
    /// ```
    pub async fn send_event(&self, session_id: &str, event: UserEvent) -> Result<()> {
        use super::events::SendEventsRequest;

        let url = format!("{}?beta=true", self.build_url(&format!("sessions/{session_id}/events")));
        let body = SendEventsRequest { events: vec![event] };
        let response = self
            .client
            .post(&url)
            .headers((*self.cached_headers).clone())
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                Error::connection(format!("failed to send send_event request: {e}"), None)
            })?;

        handle_empty_response(response).await
    }
}

// ─── SSE Streaming ───────────────────────────────────────────────────────────

impl ManagedAgentsClient {
    /// Open an SSE stream for session events.
    ///
    /// Opens a `GET /sessions/{id}/events` SSE connection and returns an async
    /// stream of typed [`SessionEvent`] values. The stream yields events as they
    /// arrive from the server, including agent messages, tool use requests, and
    /// session status changes.
    ///
    /// The stream uses the client's configured SSE timeout (default: 300 seconds).
    /// If no data is received within the timeout, a timeout error is yielded through
    /// the stream.
    ///
    /// If the SSE connection is interrupted, a connection error is yielded through
    /// the stream.
    ///
    /// # Arguments
    ///
    /// * `session_id` - The unique identifier of the session to stream events from.
    ///
    /// # Errors
    ///
    /// Returns an error if the initial HTTP request fails (e.g., network error,
    /// authentication failure, session not found). Once the stream is established,
    /// errors are yielded as `Err` items within the stream.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_anthropic::managed_agents::{ManagedAgentsClient, SessionEvent};
    /// use futures::StreamExt;
    ///
    /// let client = ManagedAgentsClient::new("sk-ant-api03-...")?;
    /// let mut stream = client.stream_events("sess_abc123").await?;
    ///
    /// while let Some(event) = stream.next().await {
    ///     match event? {
    ///         SessionEvent::AgentMessage { content } => {
    ///             println!("Agent: {content}");
    ///         }
    ///         SessionEvent::AgentCustomToolUse { tool_use_id, name, input } => {
    ///             println!("Tool request: {name} ({tool_use_id})");
    ///         }
    ///         SessionEvent::StatusIdle {} => {
    ///             println!("Session is idle");
    ///             break;
    ///         }
    ///         _ => {}
    ///     }
    /// }
    /// ```
    pub async fn stream_events(
        &self,
        session_id: &str,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<SessionEvent>> + Send>>> {
        let url = format!(
            "{}?beta=true",
            self.build_url(&format!("sessions/{session_id}/events/stream"))
        );
        let mut headers = (*self.cached_headers).clone();
        headers.insert(reqwest::header::ACCEPT, HeaderValue::from_static("text/event-stream"));
        let response = self
            .client
            .get(&url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| Error::connection(format!("failed to open SSE stream: {e}"), None))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(map_api_error(status, &body));
        }

        let byte_stream = response.bytes_stream();
        Ok(process_managed_agents_sse(byte_stream, self.sse_timeout))
    }
}

// ─── Convenience Methods ─────────────────────────────────────────────────────

impl ManagedAgentsClient {
    /// Send an interrupt to a running session.
    pub async fn interrupt(&self, session_id: &str) -> Result<()> {
        self.send_event(session_id, UserEvent::Interrupt {}).await
    }

    /// Send a custom tool result back to the session.
    ///
    /// The `custom_tool_use_id` must match the event ID from the
    /// `AgentCustomToolUse` event.
    pub async fn custom_tool_result(
        &self,
        session_id: &str,
        custom_tool_use_id: &str,
        content: impl Into<String>,
    ) -> Result<()> {
        let event = UserEvent::custom_tool_result(custom_tool_use_id, content);
        self.send_event(session_id, event).await
    }

    /// Allow a tool to execute (tool confirmation).
    ///
    /// The `tool_use_id` must match the event ID from the blocking
    /// `AgentToolUse` or `AgentMcpToolUse` event.
    pub async fn allow_tool(&self, session_id: &str, tool_use_id: &str) -> Result<()> {
        let event = UserEvent::allow_tool(tool_use_id);
        self.send_event(session_id, event).await
    }

    /// Deny a tool execution (tool confirmation).
    pub async fn deny_tool(
        &self,
        session_id: &str,
        tool_use_id: &str,
        reason: impl Into<String>,
    ) -> Result<()> {
        let event = UserEvent::deny_tool(tool_use_id, reason);
        self.send_event(session_id, event).await
    }

    /// Define an outcome (success criteria) for the session.
    pub async fn define_outcome(
        &self,
        session_id: &str,
        criteria: impl Into<String>,
    ) -> Result<()> {
        let event = UserEvent::DefineOutcome { criteria: criteria.into() };
        self.send_event(session_id, event).await
    }

    /// Archive an agent (makes it read-only).
    pub async fn archive_agent(&self, agent_id: &str) -> Result<()> {
        let url = self.build_url(&format!("agents/{agent_id}/archive"));
        let response = self
            .client
            .post(&url)
            .headers((*self.cached_headers).clone())
            .send()
            .await
            .map_err(|e| Error::connection(format!("failed to archive agent: {e}"), None))?;

        handle_empty_response(response).await
    }

    /// Archive an environment (makes it read-only).
    pub async fn archive_environment(&self, environment_id: &str) -> Result<()> {
        let url = self.build_url(&format!("environments/{environment_id}/archive"));
        let response =
            self.client.post(&url).headers((*self.cached_headers).clone()).send().await.map_err(
                |e| Error::connection(format!("failed to archive environment: {e}"), None),
            )?;

        handle_empty_response(response).await
    }
}

// ─── Vault CRUD ──────────────────────────────────────────────────────────────

impl ManagedAgentsClient {
    /// Create a new vault for storing per-user MCP credentials.
    pub async fn create_vault(&self, params: CreateVaultParams) -> Result<Vault> {
        let url = self.build_url("vaults");
        let response = self
            .client
            .post(&url)
            .headers((*self.cached_headers).clone())
            .json(&params)
            .send()
            .await
            .map_err(|e| Error::connection(format!("failed to create vault: {e}"), None))?;

        handle_response(response).await
    }

    /// List all vaults.
    pub async fn list_vaults(&self) -> Result<Vec<Vault>> {
        let url = self.build_url("vaults");
        let response =
            self.client
                .get(&url)
                .headers((*self.cached_headers).clone())
                .send()
                .await
                .map_err(|e| Error::connection(format!("failed to list vaults: {e}"), None))?;

        let list: VaultListResponse<Vault> = handle_response(response).await?;
        Ok(list.data)
    }

    /// Get a vault by ID.
    pub async fn get_vault(&self, vault_id: &str) -> Result<Vault> {
        let url = self.build_url(&format!("vaults/{vault_id}"));
        let response = self
            .client
            .get(&url)
            .headers((*self.cached_headers).clone())
            .send()
            .await
            .map_err(|e| Error::connection(format!("failed to get vault: {e}"), None))?;

        handle_response(response).await
    }

    /// Archive a vault (cascades to all credentials, purges secrets).
    pub async fn archive_vault(&self, vault_id: &str) -> Result<()> {
        let url = self.build_url(&format!("vaults/{vault_id}/archive"));
        let response = self
            .client
            .post(&url)
            .headers((*self.cached_headers).clone())
            .send()
            .await
            .map_err(|e| Error::connection(format!("failed to archive vault: {e}"), None))?;

        handle_empty_response(response).await
    }

    /// Delete a vault (hard delete, no audit trail).
    pub async fn delete_vault(&self, vault_id: &str) -> Result<()> {
        let url = self.build_url(&format!("vaults/{vault_id}"));
        let response = self
            .client
            .delete(&url)
            .headers((*self.cached_headers).clone())
            .send()
            .await
            .map_err(|e| Error::connection(format!("failed to delete vault: {e}"), None))?;

        handle_empty_response(response).await
    }

    /// Add a credential to a vault.
    pub async fn create_credential(
        &self,
        vault_id: &str,
        params: CreateCredentialParams,
    ) -> Result<Credential> {
        let url = self.build_url(&format!("vaults/{vault_id}/credentials"));
        let response = self
            .client
            .post(&url)
            .headers((*self.cached_headers).clone())
            .json(&params)
            .send()
            .await
            .map_err(|e| Error::connection(format!("failed to create credential: {e}"), None))?;

        handle_response(response).await
    }

    /// List credentials in a vault.
    pub async fn list_credentials(&self, vault_id: &str) -> Result<Vec<Credential>> {
        let url = self.build_url(&format!("vaults/{vault_id}/credentials"));
        let response =
            self.client
                .get(&url)
                .headers((*self.cached_headers).clone())
                .send()
                .await
                .map_err(|e| Error::connection(format!("failed to list credentials: {e}"), None))?;

        let list: VaultListResponse<Credential> = handle_response(response).await?;
        Ok(list.data)
    }

    /// Get a credential by ID.
    pub async fn get_credential(&self, vault_id: &str, credential_id: &str) -> Result<Credential> {
        let url = self.build_url(&format!("vaults/{vault_id}/credentials/{credential_id}"));
        let response =
            self.client
                .get(&url)
                .headers((*self.cached_headers).clone())
                .send()
                .await
                .map_err(|e| Error::connection(format!("failed to get credential: {e}"), None))?;

        handle_response(response).await
    }

    /// Rotate/update a credential's secret payload.
    pub async fn update_credential(
        &self,
        vault_id: &str,
        credential_id: &str,
        params: UpdateCredentialParams,
    ) -> Result<Credential> {
        let url = self.build_url(&format!("vaults/{vault_id}/credentials/{credential_id}"));
        let response = self
            .client
            .patch(&url)
            .headers((*self.cached_headers).clone())
            .json(&params)
            .send()
            .await
            .map_err(|e| Error::connection(format!("failed to update credential: {e}"), None))?;

        handle_response(response).await
    }

    /// Archive a credential (purges secret, retains record).
    pub async fn archive_credential(&self, vault_id: &str, credential_id: &str) -> Result<()> {
        let url = self.build_url(&format!("vaults/{vault_id}/credentials/{credential_id}/archive"));
        let response =
            self.client.post(&url).headers((*self.cached_headers).clone()).send().await.map_err(
                |e| Error::connection(format!("failed to archive credential: {e}"), None),
            )?;

        handle_empty_response(response).await
    }

    /// Delete a credential (hard delete).
    pub async fn delete_credential(&self, vault_id: &str, credential_id: &str) -> Result<()> {
        let url = self.build_url(&format!("vaults/{vault_id}/credentials/{credential_id}"));
        let response =
            self.client.delete(&url).headers((*self.cached_headers).clone()).send().await.map_err(
                |e| Error::connection(format!("failed to delete credential: {e}"), None),
            )?;

        handle_empty_response(response).await
    }

    /// Validate an MCP OAuth credential (diagnose refresh failures).
    pub async fn validate_credential(
        &self,
        vault_id: &str,
        credential_id: &str,
    ) -> Result<CredentialValidation> {
        let url = format!(
            "{}?beta=true",
            self.build_url(&format!(
                "vaults/{vault_id}/credentials/{credential_id}/mcp_oauth_validate"
            ))
        );
        let response =
            self.client.post(&url).headers((*self.cached_headers).clone()).send().await.map_err(
                |e| Error::connection(format!("failed to validate credential: {e}"), None),
            )?;

        handle_response(response).await
    }
}

// ─── Memory Store CRUD ───────────────────────────────────────────────────────

impl ManagedAgentsClient {
    /// Create a new memory store.
    pub async fn create_memory_store(
        &self,
        params: CreateMemoryStoreParams,
    ) -> Result<MemoryStore> {
        let url = self.build_url("memory_stores");
        let response = self
            .client
            .post(&url)
            .headers((*self.cached_headers).clone())
            .json(&params)
            .send()
            .await
            .map_err(|e| Error::connection(format!("failed to create memory store: {e}"), None))?;

        handle_response(response).await
    }

    /// List memory stores.
    pub async fn list_memory_stores(&self) -> Result<Vec<MemoryStore>> {
        let url = self.build_url("memory_stores");
        let response =
            self.client.get(&url).headers((*self.cached_headers).clone()).send().await.map_err(
                |e| Error::connection(format!("failed to list memory stores: {e}"), None),
            )?;

        let list: MemoryListResponse<MemoryStore> = handle_response(response).await?;
        Ok(list.data)
    }

    /// Get a memory store by ID.
    pub async fn get_memory_store(&self, store_id: &str) -> Result<MemoryStore> {
        let url = self.build_url(&format!("memory_stores/{store_id}"));
        let response =
            self.client
                .get(&url)
                .headers((*self.cached_headers).clone())
                .send()
                .await
                .map_err(|e| Error::connection(format!("failed to get memory store: {e}"), None))?;

        handle_response(response).await
    }

    /// Archive a memory store (makes it read-only, one-way).
    pub async fn archive_memory_store(&self, store_id: &str) -> Result<()> {
        let url = self.build_url(&format!("memory_stores/{store_id}/archive"));
        let response =
            self.client.post(&url).headers((*self.cached_headers).clone()).send().await.map_err(
                |e| Error::connection(format!("failed to archive memory store: {e}"), None),
            )?;

        handle_empty_response(response).await
    }

    /// Delete a memory store permanently (removes all memories and versions).
    pub async fn delete_memory_store(&self, store_id: &str) -> Result<()> {
        let url = self.build_url(&format!("memory_stores/{store_id}"));
        let response =
            self.client.delete(&url).headers((*self.cached_headers).clone()).send().await.map_err(
                |e| Error::connection(format!("failed to delete memory store: {e}"), None),
            )?;

        handle_empty_response(response).await
    }

    // ─── Memory CRUD ─────────────────────────────────────────────────────

    /// Create a memory in a store.
    pub async fn create_memory(
        &self,
        store_id: &str,
        params: CreateMemoryParams,
    ) -> Result<Memory> {
        let url = self.build_url(&format!("memory_stores/{store_id}/memories"));
        let response = self
            .client
            .post(&url)
            .headers((*self.cached_headers).clone())
            .json(&params)
            .send()
            .await
            .map_err(|e| Error::connection(format!("failed to create memory: {e}"), None))?;

        handle_response(response).await
    }

    /// List memories in a store.
    pub async fn list_memories(&self, store_id: &str) -> Result<Vec<Memory>> {
        let url = self.build_url(&format!("memory_stores/{store_id}/memories"));
        let response =
            self.client
                .get(&url)
                .headers((*self.cached_headers).clone())
                .send()
                .await
                .map_err(|e| Error::connection(format!("failed to list memories: {e}"), None))?;

        let list: MemoryListResponse<Memory> = handle_response(response).await?;
        Ok(list.data)
    }

    /// Get a memory by ID.
    pub async fn get_memory(&self, store_id: &str, memory_id: &str) -> Result<Memory> {
        let url = self.build_url(&format!("memory_stores/{store_id}/memories/{memory_id}"));
        let response =
            self.client
                .get(&url)
                .headers((*self.cached_headers).clone())
                .send()
                .await
                .map_err(|e| Error::connection(format!("failed to get memory: {e}"), None))?;

        handle_response(response).await
    }

    /// Update a memory (content, path, or both).
    pub async fn update_memory(
        &self,
        store_id: &str,
        memory_id: &str,
        params: UpdateMemoryParams,
    ) -> Result<Memory> {
        let url = self.build_url(&format!("memory_stores/{store_id}/memories/{memory_id}"));
        let response = self
            .client
            .post(&url)
            .headers((*self.cached_headers).clone())
            .json(&params)
            .send()
            .await
            .map_err(|e| Error::connection(format!("failed to update memory: {e}"), None))?;

        handle_response(response).await
    }

    /// Delete a memory.
    pub async fn delete_memory(&self, store_id: &str, memory_id: &str) -> Result<()> {
        let url = self.build_url(&format!("memory_stores/{store_id}/memories/{memory_id}"));
        let response = self
            .client
            .delete(&url)
            .headers((*self.cached_headers).clone())
            .send()
            .await
            .map_err(|e| Error::connection(format!("failed to delete memory: {e}"), None))?;

        handle_empty_response(response).await
    }

    // ─── Memory Versions ─────────────────────────────────────────────────

    /// List memory versions (audit trail).
    pub async fn list_memory_versions(&self, store_id: &str) -> Result<Vec<MemoryVersion>> {
        let url = self.build_url(&format!("memory_stores/{store_id}/memory_versions"));
        let response =
            self.client.get(&url).headers((*self.cached_headers).clone()).send().await.map_err(
                |e| Error::connection(format!("failed to list memory versions: {e}"), None),
            )?;

        let list: MemoryListResponse<MemoryVersion> = handle_response(response).await?;
        Ok(list.data)
    }

    /// Get a specific memory version.
    pub async fn get_memory_version(
        &self,
        store_id: &str,
        version_id: &str,
    ) -> Result<MemoryVersion> {
        let url = self.build_url(&format!("memory_stores/{store_id}/memory_versions/{version_id}"));
        let response =
            self.client.get(&url).headers((*self.cached_headers).clone()).send().await.map_err(
                |e| Error::connection(format!("failed to get memory version: {e}"), None),
            )?;

        handle_response(response).await
    }

    /// Redact a memory version (scrub content, preserve audit trail).
    pub async fn redact_memory_version(&self, store_id: &str, version_id: &str) -> Result<()> {
        let url = self
            .build_url(&format!("memory_stores/{store_id}/memory_versions/{version_id}/redact"));
        let response = self
            .client
            .post(&url)
            .headers((*self.cached_headers).clone())
            .json(&serde_json::json!({}))
            .send()
            .await
            .map_err(|e| {
                Error::connection(format!("failed to redact memory version: {e}"), None)
            })?;

        handle_empty_response(response).await
    }
}

// ─── Session Threads (Multiagent) ────────────────────────────────────────────

impl ManagedAgentsClient {
    /// List all threads in a multiagent session.
    pub async fn list_threads(&self, session_id: &str) -> Result<Vec<SessionThread>> {
        let url = self.build_url(&format!("sessions/{session_id}/threads"));
        let response =
            self.client
                .get(&url)
                .headers((*self.cached_headers).clone())
                .send()
                .await
                .map_err(|e| Error::connection(format!("failed to list threads: {e}"), None))?;

        let list: ListResponse<SessionThread> = handle_response(response).await?;
        Ok(list.data)
    }

    /// Stream events from a specific session thread.
    pub async fn stream_thread_events(
        &self,
        session_id: &str,
        thread_id: &str,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<SessionEvent>> + Send>>> {
        let url = format!(
            "{}?beta=true",
            self.build_url(&format!("sessions/{session_id}/threads/{thread_id}/stream"))
        );
        let mut headers = (*self.cached_headers).clone();
        headers.insert(reqwest::header::ACCEPT, HeaderValue::from_static("text/event-stream"));
        let response =
            self.client.get(&url).headers(headers).send().await.map_err(|e| {
                Error::connection(format!("failed to open thread stream: {e}"), None)
            })?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(map_api_error(status, &body));
        }

        let byte_stream = response.bytes_stream();
        Ok(process_managed_agents_sse(byte_stream, self.sse_timeout))
    }

    /// Archive a session thread (frees up against the 25-thread limit).
    ///
    /// The thread must be idle. If running, interrupt it first.
    pub async fn archive_thread(&self, session_id: &str, thread_id: &str) -> Result<()> {
        let url = self.build_url(&format!("sessions/{session_id}/threads/{thread_id}/archive"));
        let response = self
            .client
            .post(&url)
            .headers((*self.cached_headers).clone())
            .send()
            .await
            .map_err(|e| Error::connection(format!("failed to archive thread: {e}"), None))?;

        handle_empty_response(response).await
    }

    /// Interrupt a specific thread in a multiagent session.
    ///
    /// Sends `user.interrupt` with `session_thread_id` to target a specific thread.
    pub async fn interrupt_thread(&self, session_id: &str, thread_id: &str) -> Result<()> {
        let url = format!("{}?beta=true", self.build_url(&format!("sessions/{session_id}/events")));
        let body = serde_json::json!({
            "events": [{
                "type": "user.interrupt",
                "session_thread_id": thread_id,
            }]
        });
        let response = self
            .client
            .post(&url)
            .headers((*self.cached_headers).clone())
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::connection(format!("failed to interrupt thread: {e}"), None))?;

        handle_empty_response(response).await
    }
}

// ─── Self-Hosted Environment Work Queue ──────────────────────────────────────

impl ManagedAgentsClient {
    /// Get work queue stats for a self-hosted environment.
    ///
    /// Returns queue depth, pending items, oldest queued timestamp, and
    /// number of active workers. Use this for monitoring and autoscaling.
    pub async fn get_work_stats(&self, environment_id: &str) -> Result<serde_json::Value> {
        let url = self.build_url(&format!("environments/{environment_id}/work/stats"));
        let response =
            self.client
                .get(&url)
                .headers((*self.cached_headers).clone())
                .send()
                .await
                .map_err(|e| Error::connection(format!("failed to get work stats: {e}"), None))?;

        handle_response(response).await
    }

    /// Stop a work item (session) on a self-hosted environment.
    ///
    /// Asks the worker to shut down the session cleanly. Pass `force: true`
    /// in the body to interrupt immediately.
    pub async fn stop_work(&self, environment_id: &str, work_id: &str, force: bool) -> Result<()> {
        let url = self.build_url(&format!("environments/{environment_id}/work/{work_id}/stop"));
        let body = if force { serde_json::json!({"force": true}) } else { serde_json::json!({}) };
        let response = self
            .client
            .post(&url)
            .headers((*self.cached_headers).clone())
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::connection(format!("failed to stop work: {e}"), None))?;

        handle_empty_response(response).await
    }
}

// ─── Dreams API ──────────────────────────────────────────────────────────────

impl ManagedAgentsClient {
    /// Create a dream (asynchronous memory curation job).
    ///
    /// Dreams require the additional `dreaming-2026-04-21` beta header.
    /// This method adds it automatically.
    pub async fn create_dream(&self, params: CreateDreamParams) -> Result<Dream> {
        let url = self.build_url("dreams");
        let mut headers = (*self.cached_headers).clone();
        // Dreams require an additional beta header
        headers.insert(
            "anthropic-beta",
            HeaderValue::from_static("managed-agents-2026-04-01,dreaming-2026-04-21"),
        );
        let response = self
            .client
            .post(&url)
            .headers(headers)
            .json(&params)
            .send()
            .await
            .map_err(|e| Error::connection(format!("failed to create dream: {e}"), None))?;

        handle_response(response).await
    }

    /// Get a dream by ID.
    pub async fn get_dream(&self, dream_id: &str) -> Result<Dream> {
        let url = self.build_url(&format!("dreams/{dream_id}"));
        let mut headers = (*self.cached_headers).clone();
        headers.insert(
            "anthropic-beta",
            HeaderValue::from_static("managed-agents-2026-04-01,dreaming-2026-04-21"),
        );
        let response = self
            .client
            .get(&url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| Error::connection(format!("failed to get dream: {e}"), None))?;

        handle_response(response).await
    }

    /// List dreams in the workspace.
    pub async fn list_dreams(&self) -> Result<Vec<Dream>> {
        let url = self.build_url("dreams");
        let mut headers = (*self.cached_headers).clone();
        headers.insert(
            "anthropic-beta",
            HeaderValue::from_static("managed-agents-2026-04-01,dreaming-2026-04-21"),
        );
        let response = self
            .client
            .get(&url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| Error::connection(format!("failed to list dreams: {e}"), None))?;

        let list: DreamListResponse = handle_response(response).await?;
        Ok(list.data)
    }

    /// Cancel a pending or running dream.
    pub async fn cancel_dream(&self, dream_id: &str) -> Result<()> {
        let url = self.build_url(&format!("dreams/{dream_id}/cancel"));
        let mut headers = (*self.cached_headers).clone();
        headers.insert(
            "anthropic-beta",
            HeaderValue::from_static("managed-agents-2026-04-01,dreaming-2026-04-21"),
        );
        let response = self
            .client
            .post(&url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| Error::connection(format!("failed to cancel dream: {e}"), None))?;

        handle_empty_response(response).await
    }

    /// Archive a completed/failed/canceled dream.
    pub async fn archive_dream(&self, dream_id: &str) -> Result<()> {
        let url = self.build_url(&format!("dreams/{dream_id}/archive"));
        let mut headers = (*self.cached_headers).clone();
        headers.insert(
            "anthropic-beta",
            HeaderValue::from_static("managed-agents-2026-04-01,dreaming-2026-04-21"),
        );
        let response = self
            .client
            .post(&url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| Error::connection(format!("failed to archive dream: {e}"), None))?;

        handle_empty_response(response).await
    }
}

// ─── File Upload (Managed Agents) ────────────────────────────────────────────

impl ManagedAgentsClient {
    /// Upload a file for use in managed agent sessions.
    ///
    /// Uses the managed-agents beta header so the file is accessible
    /// when mounted in session resources.
    pub async fn upload_file(
        &self,
        filename: impl Into<String>,
        data: Vec<u8>,
    ) -> Result<serde_json::Value> {
        let url = self.build_url("files");
        let filename = filename.into();

        let mime = infer_mime(&filename);
        let part =
            reqwest::multipart::Part::bytes(data).file_name(filename).mime_str(mime).map_err(
                |e| Error::BadRequest { message: format!("invalid mime type: {e}"), param: None },
            )?;
        let form = reqwest::multipart::Form::new().part("file", part);

        // Use headers without content-type (reqwest sets multipart boundary)
        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", self.cached_headers.get("x-api-key").unwrap().clone());
        headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
        headers.insert("anthropic-beta", HeaderValue::from_static("managed-agents-2026-04-01"));

        let response = self
            .client
            .post(&url)
            .headers(headers)
            .multipart(form)
            .send()
            .await
            .map_err(|e| Error::connection(format!("failed to upload file: {e}"), None))?;

        handle_response(response).await
    }

    /// Download a file from a session (files created by the agent).
    pub async fn download_file(&self, file_id: &str) -> Result<Vec<u8>> {
        let url = self.build_url(&format!("files/{file_id}/content"));
        let response =
            self.client
                .get(&url)
                .headers((*self.cached_headers).clone())
                .send()
                .await
                .map_err(|e| Error::connection(format!("failed to download file: {e}"), None))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(map_api_error(status, &body));
        }

        response.bytes().await.map(|b| b.to_vec()).map_err(|e| Error::Connection {
            message: format!("failed to read file content: {e}"),
            source: None,
        })
    }

    /// List files scoped to a session.
    pub async fn list_session_files(&self, session_id: &str) -> Result<Vec<serde_json::Value>> {
        let url = format!("{}?scope_id={session_id}", self.build_url("files"));
        let response =
            self.client.get(&url).headers((*self.cached_headers).clone()).send().await.map_err(
                |e| Error::connection(format!("failed to list session files: {e}"), None),
            )?;

        let body: serde_json::Value = handle_response(response).await?;
        Ok(body.get("data").and_then(|d| d.as_array()).cloned().unwrap_or_default())
    }
}

fn infer_mime(filename: &str) -> &'static str {
    let ext = filename.rsplit('.').next().unwrap_or("").to_lowercase();
    match ext.as_str() {
        "pdf" => "application/pdf",
        "txt" | "text" | "md" => "text/plain",
        "csv" => "text/csv",
        "json" => "application/json",
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => "application/octet-stream",
    }
}

// ─── Session Resources (File Mounting) ───────────────────────────────────────

impl ManagedAgentsClient {
    /// Add a file resource to a session.
    ///
    /// Mounts the file in the session's sandbox. Returns the resource with
    /// its assigned `id` (used for deletion).
    pub async fn add_session_resource(
        &self,
        session_id: &str,
        resource: serde_json::Value,
    ) -> Result<SessionResourceResponse> {
        let url = self.build_url(&format!("sessions/{session_id}/resources"));
        let response = self
            .client
            .post(&url)
            .headers((*self.cached_headers).clone())
            .json(&resource)
            .send()
            .await
            .map_err(|e| Error::connection(format!("failed to add session resource: {e}"), None))?;

        handle_response(response).await
    }

    /// List all resources attached to a session.
    pub async fn list_session_resources(
        &self,
        session_id: &str,
    ) -> Result<Vec<SessionResourceResponse>> {
        let url = self.build_url(&format!("sessions/{session_id}/resources"));
        let response =
            self.client.get(&url).headers((*self.cached_headers).clone()).send().await.map_err(
                |e| Error::connection(format!("failed to list session resources: {e}"), None),
            )?;

        let list: ListResponse<SessionResourceResponse> = handle_response(response).await?;
        Ok(list.data)
    }

    /// Remove a resource from a session.
    ///
    /// The `resource_id` is the `id` returned when the resource was added
    /// (e.g., `"sesrsc_01ABC..."`).
    pub async fn delete_session_resource(&self, session_id: &str, resource_id: &str) -> Result<()> {
        let url = self.build_url(&format!("sessions/{session_id}/resources/{resource_id}"));
        let response =
            self.client.delete(&url).headers((*self.cached_headers).clone()).send().await.map_err(
                |e| Error::connection(format!("failed to delete session resource: {e}"), None),
            )?;

        handle_empty_response(response).await
    }
}

// ─── Response Handling ───────────────────────────────────────────────────────

/// Handle an API response that returns a JSON body on success.
///
/// Checks the HTTP status code and either deserializes the response body
/// or maps the error status to an appropriate `Error` variant.
async fn handle_response<T: serde::de::DeserializeOwned>(response: reqwest::Response) -> Result<T> {
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(map_api_error(status, &body));
    }
    let body = response.text().await.map_err(|e| Error::Serialization {
        message: format!("failed to read response body: {e}"),
        source: None,
    })?;
    serde_json::from_str::<T>(&body).map_err(|e| Error::Serialization {
        message: format!("failed to deserialize response: {e}\nBody: {body}"),
        source: None,
    })
}

/// Handle an API response that returns an empty body on success (e.g., 204 No Content).
///
/// Checks the HTTP status code and returns `Ok(())` on success or maps the
/// error status to an appropriate `Error` variant.
async fn handle_empty_response(response: reqwest::Response) -> Result<()> {
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(map_api_error(status, &body));
    }
    Ok(())
}

/// Map an HTTP error status code to the appropriate `Error` variant.
///
/// Attempts to parse the response body as a JSON error object with `error.message`
/// and `error.type` fields (Anthropic's standard error format). Falls back to
/// using the raw body text if parsing fails.
fn map_api_error(status: reqwest::StatusCode, body: &str) -> Error {
    // Try to extract error message from Anthropic's standard error format:
    // { "error": { "type": "...", "message": "..." } }
    let (error_type, message) = parse_error_body(body);

    match status.as_u16() {
        400 => Error::BadRequest { message, param: None },
        401 => Error::Authentication { message },
        403 => Error::Permission { message },
        404 => Error::NotFound { message, resource_type: None, resource_id: None },
        408 => Error::Timeout { message, duration: None },
        429 => Error::RateLimit { message, retry_after: None },
        500 => Error::InternalServer { message, request_id: None },
        502..=504 => Error::ServiceUnavailable { message, retry_after: None },
        _ => Error::Api {
            status_code: status.as_u16(),
            error_type: Some(error_type),
            message,
            request_id: None,
        },
    }
}

/// Parse the error body from an Anthropic API error response.
///
/// Returns `(error_type, message)`. If parsing fails, returns a generic
/// error type and the raw body text.
fn parse_error_body(body: &str) -> (String, String) {
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(body) {
        let error_obj = json.get("error").unwrap_or(&json);
        let error_type =
            error_obj.get("type").and_then(|v| v.as_str()).unwrap_or("api_error").to_string();
        let message = error_obj.get("message").and_then(|v| v.as_str()).unwrap_or(body).to_string();
        (error_type, message)
    } else {
        ("api_error".to_string(), body.to_string())
    }
}

/// Build the pre-cached headers for all API requests.
///
/// Every request to the Managed Agents API includes:
/// - `x-api-key`: The API key for authentication
/// - `anthropic-version`: The API version (`2023-06-01`)
/// - `anthropic-beta`: The beta feature flag (`managed-agents-2026-04-01`)
/// - `content-type`: JSON content type
fn build_headers(api_key: &str) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-api-key",
        HeaderValue::from_str(api_key).map_err(|e| Error::Authentication {
            message: format!("invalid API key header value: {e}"),
        })?,
    );
    headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
    headers.insert("anthropic-beta", HeaderValue::from_static("managed-agents-2026-04-01"));
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    Ok(headers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_creates_client_with_defaults() {
        let client = ManagedAgentsClient::new("test-api-key").unwrap();
        assert_eq!(client.base_url, "https://api.anthropic.com");
        assert_eq!(client.sse_timeout, Duration::from_secs(300));
        assert_eq!(client.api_key, "test-api-key");
    }

    #[test]
    fn test_with_base_url_overrides_default() {
        let client = ManagedAgentsClient::new("test-api-key")
            .unwrap()
            .with_base_url("https://custom.example.com");
        assert_eq!(client.base_url, "https://custom.example.com");
    }

    #[test]
    fn test_with_sse_timeout_overrides_default() {
        let client = ManagedAgentsClient::new("test-api-key")
            .unwrap()
            .with_sse_timeout(Duration::from_secs(600));
        assert_eq!(client.sse_timeout, Duration::from_secs(600));
    }

    #[test]
    fn test_build_url_constructs_correct_path() {
        let client = ManagedAgentsClient::new("test-api-key").unwrap();
        assert_eq!(client.build_url("agents"), "https://api.anthropic.com/v1/agents");
        assert_eq!(
            client.build_url("sessions/sess_123/events"),
            "https://api.anthropic.com/v1/sessions/sess_123/events"
        );
    }

    #[test]
    fn test_build_url_trims_trailing_slash() {
        let client = ManagedAgentsClient::new("test-api-key")
            .unwrap()
            .with_base_url("https://api.anthropic.com/");
        assert_eq!(client.build_url("agents"), "https://api.anthropic.com/v1/agents");
    }

    #[test]
    fn test_build_headers_includes_required_headers() {
        let headers = build_headers("test-key").unwrap();
        assert_eq!(headers.get("x-api-key").unwrap(), "test-key");
        assert_eq!(headers.get("anthropic-version").unwrap(), "2023-06-01");
        assert_eq!(headers.get("anthropic-beta").unwrap(), "managed-agents-2026-04-01");
        assert_eq!(headers.get("content-type").unwrap(), "application/json");
    }

    #[test]
    fn test_from_env_missing_key_returns_authentication_error() {
        // Temporarily unset the env var to test the error case
        let original = std::env::var("ANTHROPIC_API_KEY").ok();
        // SAFETY: This test is single-threaded and restores the variable afterward.
        unsafe {
            std::env::remove_var("ANTHROPIC_API_KEY");
        }

        let result = ManagedAgentsClient::from_env();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.is_authentication());

        // Restore if it was set
        if let Some(val) = original {
            // SAFETY: Restoring the original environment variable.
            unsafe {
                std::env::set_var("ANTHROPIC_API_KEY", val);
            }
        }
    }
}
