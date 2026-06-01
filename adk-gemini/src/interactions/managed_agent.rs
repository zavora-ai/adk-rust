//! Managed agent types for the Gemini Interactions API.
//!
//! A managed agent is a server-side agent configuration persisted via the
//! `/v1beta/agents` resource. Once saved, a managed agent can be invoked by
//! passing its ID as the `agent` field on an interaction request.
//!
//! This module provides:
//! - [`SavedAgent`] — the server-side representation of a saved agent
//! - [`CreateAgentRequest`] — the request body for creating a new agent
//! - [`ListAgentsResponse`] — paginated list of saved agents
//! - [`ManagedAgentBuilder`] — fluent builder for creating and saving agents

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::client::{Error, GeminiClient};
use crate::interactions::environment::EnvironmentConfig;

// ══════════════════════════════════════════════════════════════════════
// SavedAgent
// ══════════════════════════════════════════════════════════════════════

/// A managed-agent configuration persisted via `/v1beta/agents`.
///
/// Represents a reusable agent setup with a base agent, system instruction,
/// and optional base environment. Once saved, the agent can be invoked by
/// passing its `id` as the `agent` field on an interaction request.
///
/// # Example
///
/// ```rust
/// use adk_gemini::interactions::managed_agent::SavedAgent;
///
/// // Deserialize from a server response
/// let json = r#"{
///     "id": "my-coding-agent",
///     "base_agent": "antigravity-preview-05-2026",
///     "system_instruction": "You are a Rust expert.",
///     "created": "2026-06-01T10:00:00Z",
///     "updated": "2026-06-01T10:00:00Z"
/// }"#;
/// let agent: SavedAgent = serde_json::from_str(json).unwrap();
/// assert_eq!(agent.id.as_deref(), Some("my-coding-agent"));
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedAgent {
    /// Server-assigned agent ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,

    /// The base managed agent (e.g. `"antigravity-preview-05-2026"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_agent: Option<String>,

    /// System instruction for the saved agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_instruction: Option<String>,

    /// Base environment configuration (sources + network).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_environment: Option<EnvironmentConfig>,

    /// ISO 8601 creation time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,

    /// ISO 8601 last-updated time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated: Option<String>,
}

// ══════════════════════════════════════════════════════════════════════
// CreateAgentRequest
// ══════════════════════════════════════════════════════════════════════

/// Request body for `POST /v1beta/agents`.
///
/// Contains the required fields (`id`, `base_agent`) and optional configuration
/// for creating a new managed-agent on the server.
///
/// # Example
///
/// ```rust
/// use adk_gemini::interactions::managed_agent::CreateAgentRequest;
///
/// let request = CreateAgentRequest::new("my-coding-agent", "antigravity-preview-05-2026")
///     .with_system_instruction("You are a Rust expert.");
///
/// let json = serde_json::to_string(&request).unwrap();
/// assert!(json.contains("my-coding-agent"));
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateAgentRequest {
    /// Caller-chosen agent ID.
    pub id: String,

    /// The base managed agent.
    pub base_agent: String,

    /// System instruction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_instruction: Option<String>,

    /// Base environment configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_environment: Option<EnvironmentConfig>,
}

impl CreateAgentRequest {
    /// Create a new request with the required fields.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_gemini::interactions::managed_agent::CreateAgentRequest;
    ///
    /// let request = CreateAgentRequest::new("my-agent", "antigravity-preview-05-2026");
    /// assert_eq!(request.id, "my-agent");
    /// assert_eq!(request.base_agent, "antigravity-preview-05-2026");
    /// ```
    pub fn new(id: impl Into<String>, base_agent: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            base_agent: base_agent.into(),
            system_instruction: None,
            base_environment: None,
        }
    }

    /// Set the system instruction.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_gemini::interactions::managed_agent::CreateAgentRequest;
    ///
    /// let request = CreateAgentRequest::new("my-agent", "antigravity-preview-05-2026")
    ///     .with_system_instruction("You are a Rust expert.");
    /// assert_eq!(request.system_instruction.as_deref(), Some("You are a Rust expert."));
    /// ```
    pub fn with_system_instruction(mut self, text: impl Into<String>) -> Self {
        self.system_instruction = Some(text.into());
        self
    }

    /// Set the base environment configuration.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_gemini::interactions::managed_agent::CreateAgentRequest;
    /// use adk_gemini::interactions::EnvironmentConfig;
    ///
    /// let request = CreateAgentRequest::new("my-agent", "antigravity-preview-05-2026")
    ///     .with_base_environment(EnvironmentConfig::new());
    /// assert!(request.base_environment.is_some());
    /// ```
    pub fn with_base_environment(mut self, config: EnvironmentConfig) -> Self {
        self.base_environment = Some(config);
        self
    }
}

// ══════════════════════════════════════════════════════════════════════
// ListAgentsResponse
// ══════════════════════════════════════════════════════════════════════

/// Response from `GET /v1beta/agents`.
///
/// Contains a paginated list of saved agent configurations.
///
/// # Example
///
/// ```rust
/// use adk_gemini::interactions::managed_agent::ListAgentsResponse;
///
/// let json = r#"{"agents": [], "next_page_token": null}"#;
/// let response: ListAgentsResponse = serde_json::from_str(json).unwrap();
/// assert!(response.agents.is_empty());
/// assert!(response.next_page_token.is_none());
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListAgentsResponse {
    /// The list of saved agents.
    #[serde(default)]
    pub agents: Vec<SavedAgent>,

    /// Token for fetching the next page, if more results exist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_page_token: Option<String>,
}

// ══════════════════════════════════════════════════════════════════════
// ManagedAgentBuilder
// ══════════════════════════════════════════════════════════════════════

/// Fluent builder for creating a saved managed-agent configuration.
///
/// Obtained via [`Gemini::create_agent()`](crate::Gemini::create_agent). The
/// builder accumulates configuration and saves the agent to the server when
/// [`build_and_save()`](Self::build_and_save) is called.
///
/// This is a direct-client capability and is not wired into the `adk-runner`
/// `Agent` trait.
///
/// # Example
///
/// ```rust,ignore
/// use adk_gemini::Gemini;
///
/// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
/// let gemini = Gemini::new("YOUR_API_KEY")?;
///
/// let agent = gemini.create_agent()
///     .id("my-coding-agent")
///     .base_agent("antigravity-preview-05-2026")
///     .system_instruction("You are a Rust expert.")
///     .build_and_save()
///     .await?;
///
/// println!("Created agent: {:?}", agent.id);
/// # Ok(())
/// # }
/// ```
pub struct ManagedAgentBuilder {
    client: Arc<GeminiClient>,
    request: CreateAgentRequest,
}

impl ManagedAgentBuilder {
    /// Create a new builder with the given client.
    pub(crate) fn new(client: Arc<GeminiClient>) -> Self {
        Self { client, request: CreateAgentRequest::new("", "") }
    }

    /// Set the caller-chosen agent ID.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let builder = gemini.create_agent().id("my-agent");
    /// ```
    pub fn id(mut self, id: impl Into<String>) -> Self {
        self.request.id = id.into();
        self
    }

    /// Set the base managed agent (e.g. `"antigravity-preview-05-2026"`).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let builder = gemini.create_agent()
    ///     .base_agent("antigravity-preview-05-2026");
    /// ```
    pub fn base_agent(mut self, agent: impl Into<String>) -> Self {
        self.request.base_agent = agent.into();
        self
    }

    /// Set the system instruction for the saved agent.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let builder = gemini.create_agent()
    ///     .system_instruction("You are a Rust expert.");
    /// ```
    pub fn system_instruction(mut self, text: impl Into<String>) -> Self {
        self.request.system_instruction = Some(text.into());
        self
    }

    /// Set the base environment configuration (sources + network).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_gemini::interactions::EnvironmentConfig;
    ///
    /// let config = EnvironmentConfig::new();
    /// let builder = gemini.create_agent()
    ///     .base_environment(config);
    /// ```
    pub fn base_environment(mut self, config: EnvironmentConfig) -> Self {
        self.request.base_environment = Some(config);
        self
    }

    /// Save the agent configuration to the server.
    ///
    /// Sends a `POST /v1beta/agents` request with the accumulated configuration
    /// and returns the server's response as a [`SavedAgent`].
    ///
    /// # Errors
    ///
    /// Returns an error if the network request fails or the server returns a
    /// non-success status code.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let agent = gemini.create_agent()
    ///     .id("my-coding-agent")
    ///     .base_agent("antigravity-preview-05-2026")
    ///     .system_instruction("You are a Rust expert.")
    ///     .build_and_save()
    ///     .await?;
    /// ```
    pub async fn build_and_save(self) -> Result<SavedAgent, Error> {
        self.client.create_agent(self.request).await
    }
}
