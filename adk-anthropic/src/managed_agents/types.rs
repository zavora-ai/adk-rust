//! Resource types for the Managed Agents API (Agent, Environment, Session, Tool, McpServerConfig).
//!
//! These types are aligned with the official Anthropic Managed Agents API documentation.
//! See: <https://platform.claude.com/docs/en/managed-agents/overview>

use std::fmt;

use serde::{Deserialize, Serialize};

// ─── Agent ───────────────────────────────────────────────────────────────────

/// A managed agent configuration returned by the Anthropic Managed Agents API.
///
/// The API returns the full agent object including server-assigned fields like
/// `id`, `version`, `created_at`, etc.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Agent {
    /// Unique identifier assigned by the API.
    pub id: String,
    /// Human-readable name for the agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The model configuration. The API returns this as an object `{"id": "...", "speed": "..."}`.
    pub model: ModelConfig,
    /// System prompt that defines the agent's behavior.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// Description of what the agent does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Tools available to this agent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<serde_json::Value>,
    /// MCP servers configured for this agent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_servers: Vec<McpServerConfig>,
    /// Skills attached to this agent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<serde_json::Value>,
    /// Agent version (increments on each update).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u64>,
    /// ISO 8601 timestamp of creation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// ISO 8601 timestamp of last update.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// ISO 8601 timestamp of archival (null if active).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<String>,
    /// Additional fields from the API response.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Parameters for creating a new managed agent.
///
/// The `model` field accepts either a plain string (e.g., `"claude-sonnet-4-6"`)
/// or an object with `id` and optional `speed` fields.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CreateAgentParams {
    /// Required. Human-readable name for the agent.
    pub name: String,
    /// Required. The model to use. Can be a string or `{"id": "...", "speed": "..."}`.
    pub model: serde_json::Value,
    /// System prompt that defines the agent's behavior.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// Description of what the agent does.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Tools available to this agent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<serde_json::Value>,
    /// MCP servers configured for this agent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_servers: Vec<serde_json::Value>,
    /// Skills to attach.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<serde_json::Value>,
    /// Multiagent coordinator configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multiagent: Option<serde_json::Value>,
    /// Metadata key-value pairs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Map<String, serde_json::Value>>,
}

/// Model configuration for a managed agent (as returned by the API).
///
/// The API always returns model as `{"id": "claude-...", "speed": "standard"|"fast"}`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelConfig {
    /// The model identifier (e.g., `"claude-sonnet-4-6"`, `"claude-opus-4-8"`).
    pub id: String,
    /// Speed mode. Defaults to `"standard"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<String>,
}

impl ModelConfig {
    /// Create a new model configuration with the given model ID.
    pub fn new(id: impl Into<String>) -> Self {
        Self { id: id.into(), speed: None }
    }
}

// ─── Tool Configuration ──────────────────────────────────────────────────────

/// Tool configuration helpers for building agent tool arrays.
///
/// The API uses a flexible JSON format for tools. These helpers produce
/// the correct JSON values.
pub struct ToolConfig;

impl ToolConfig {
    /// Create the standard agent toolset that enables all built-in tools.
    ///
    /// Equivalent to `{"type": "agent_toolset_20260401"}`.
    pub fn agent_toolset() -> serde_json::Value {
        serde_json::json!({"type": "agent_toolset_20260401"})
    }

    /// Create a custom tool definition.
    ///
    /// Custom tools are executed by the caller when the agent invokes them.
    pub fn custom(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: serde_json::Value,
    ) -> serde_json::Value {
        serde_json::json!({
            "type": "custom",
            "name": name.into(),
            "description": description.into(),
            "input_schema": input_schema,
        })
    }

    /// Create an MCP toolset reference.
    ///
    /// The `mcp_server_name` must match a name in the agent's `mcp_servers` array.
    pub fn mcp_toolset(mcp_server_name: impl Into<String>) -> serde_json::Value {
        serde_json::json!({
            "type": "mcp_toolset",
            "mcp_server_name": mcp_server_name.into(),
        })
    }

    /// Create an MCP toolset with a permission policy.
    ///
    /// Use `"always_allow"` to auto-approve all tools, or `"always_ask"` (default)
    /// to require confirmation before each tool call.
    pub fn mcp_toolset_with_policy(
        mcp_server_name: impl Into<String>,
        policy: impl Into<String>,
    ) -> serde_json::Value {
        serde_json::json!({
            "type": "mcp_toolset",
            "mcp_server_name": mcp_server_name.into(),
            "default_config": {
                "permission_policy": {"type": policy.into()}
            },
        })
    }

    /// Create an MCP toolset with specific tools enabled/disabled.
    ///
    /// Pass `configs` as a vec of `{"name": "tool_name", "enabled": true/false}`.
    pub fn mcp_toolset_with_configs(
        mcp_server_name: impl Into<String>,
        configs: Vec<serde_json::Value>,
    ) -> serde_json::Value {
        serde_json::json!({
            "type": "mcp_toolset",
            "mcp_server_name": mcp_server_name.into(),
            "configs": configs,
        })
    }

    /// Create an agent toolset with a permission policy.
    ///
    /// Use `"always_allow"` or `"always_ask"`.
    pub fn agent_toolset_with_policy(policy: impl Into<String>) -> serde_json::Value {
        serde_json::json!({
            "type": "agent_toolset_20260401",
            "default_config": {
                "permission_policy": {"type": policy.into()}
            },
        })
    }
}

/// Helper for building MCP server declarations in `CreateAgentParams.mcp_servers`.
pub struct McpServer;

impl McpServer {
    /// Create an MCP server declaration.
    ///
    /// Use this in `CreateAgentParams.mcp_servers`. The `name` must match
    /// the `mcp_server_name` in the corresponding `ToolConfig::mcp_toolset()`.
    pub fn url(name: impl Into<String>, url: impl Into<String>) -> serde_json::Value {
        serde_json::json!({
            "type": "url",
            "name": name.into(),
            "url": url.into(),
        })
    }
}

// ─── MCP Server Configuration ────────────────────────────────────────────────

/// Configuration for an MCP (Model Context Protocol) server attached to an agent.
///
/// The `Debug` implementation redacts authentication credentials to prevent
/// accidental exposure in logs.
#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct McpServerConfig {
    /// Must be `"url"`.
    #[serde(rename = "type", default = "default_mcp_type")]
    pub server_type: String,
    /// A human-readable name for this MCP server.
    pub name: String,
    /// The URL of the MCP server.
    pub url: String,
}

fn default_mcp_type() -> String {
    "url".to_string()
}

/// Authentication configuration for an MCP server.
///
/// The `Debug` implementation redacts the token value to prevent accidental
/// exposure in logs.
#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct McpAuth {
    /// The authentication type (e.g., `"bearer"`).
    #[serde(rename = "type")]
    pub auth_type: String,
    /// The authentication token.
    pub token: String,
}

impl fmt::Debug for McpServerConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpServerConfig")
            .field("type", &self.server_type)
            .field("name", &self.name)
            .field("url", &self.url)
            .finish()
    }
}

impl fmt::Debug for McpAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpAuth")
            .field("auth_type", &self.auth_type)
            .field("token", &"[REDACTED]")
            .finish()
    }
}

// ─── Environment ─────────────────────────────────────────────────────────────

/// A sandbox environment for running managed agent sessions.
///
/// The API returns many fields; we capture the essential ones and flatten the rest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Environment {
    /// Unique identifier assigned by the API.
    pub id: String,
    /// Human-readable name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The environment configuration (cloud/self-hosted with networking, packages, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Value>,
    /// Current state (e.g., `"active"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// ISO 8601 timestamp of creation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// ISO 8601 timestamp of last update.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// ISO 8601 timestamp of archival (null if active).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<String>,
    /// Additional fields from the API response.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Parameters for creating a new environment.
///
/// The `config` field specifies the sandbox type and networking.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CreateEnvironmentParams {
    /// Required. Human-readable name (must be unique within org/workspace).
    pub name: String,
    /// The sandbox configuration.
    pub config: serde_json::Value,
}

impl CreateEnvironmentParams {
    /// Create params for a cloud environment with unrestricted networking.
    pub fn cloud(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            config: serde_json::json!({
                "type": "cloud",
                "networking": {"type": "unrestricted"}
            }),
        }
    }

    /// Create params for a cloud environment with custom config.
    pub fn cloud_with_config(name: impl Into<String>, config: serde_json::Value) -> Self {
        Self { name: name.into(), config }
    }

    /// Create params for a self-hosted environment.
    ///
    /// Self-hosted environments run tool execution on your infrastructure.
    /// You'll need to run an environment worker that polls the work queue.
    pub fn self_hosted(name: impl Into<String>) -> Self {
        Self { name: name.into(), config: serde_json::json!({"type": "self_hosted"}) }
    }
}

// ─── Session ─────────────────────────────────────────────────────────────────

/// A stateful, long-running agent execution context.
///
/// The API returns the full agent object embedded in the session response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Session {
    /// Unique identifier assigned by the API.
    pub id: String,
    /// The agent configuration (returned as a full object by the API).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<serde_json::Value>,
    /// The ID of the environment this session runs in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_id: Option<String>,
    /// The current lifecycle status of the session.
    pub status: SessionStatus,
    /// Token usage tracking for this session.
    #[serde(default)]
    pub usage: UsageTracking,
    /// ISO 8601 timestamp of creation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// ISO 8601 timestamp of last update.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// Additional fields from the API response.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// The lifecycle status of a managed agent session.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    /// The session is idle and waiting for input.
    Idle,
    /// The session is actively processing.
    Running,
    /// Transient error, retrying automatically.
    Rescheduling,
    /// The session has ended due to an unrecoverable error.
    Terminated,
}

/// Token usage tracking for a session.
///
/// The `cache_creation` field is an object in the API response (not a scalar),
/// so we represent it as `Option<serde_json::Value>`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct UsageTracking {
    /// Number of uncached input tokens consumed.
    #[serde(default)]
    pub input_tokens: u64,
    /// Number of output tokens generated.
    #[serde(default)]
    pub output_tokens: u64,
    /// Number of tokens read from cache.
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    /// Cache creation token details (object with ephemeral token counts, or null).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation: Option<serde_json::Value>,
    /// Legacy/alternative field for cache creation input tokens.
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
}

/// Parameters for creating a new session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CreateSessionParams {
    /// The agent ID (string) or agent reference object.
    pub agent: serde_json::Value,
    /// The ID of the environment to run this session in.
    pub environment_id: String,
    /// Optional session title.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Optional vault IDs for MCP authentication.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vault_ids: Vec<String>,
    /// Files, memory stores, and other resources to mount in the session sandbox.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resources: Vec<serde_json::Value>,
    /// Arbitrary metadata (useful for self-hosted environments to pass context to workers).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Map<String, serde_json::Value>>,
}

impl CreateSessionParams {
    /// Create session params with an agent ID string and environment ID.
    pub fn new(agent_id: impl Into<String>, environment_id: impl Into<String>) -> Self {
        Self {
            agent: serde_json::Value::String(agent_id.into()),
            environment_id: environment_id.into(),
            title: None,
            vault_ids: vec![],
            resources: vec![],
            metadata: None,
        }
    }

    /// Add metadata (useful for self-hosted environments to pass context to workers).
    pub fn with_metadata(mut self, metadata: serde_json::Map<String, serde_json::Value>) -> Self {
        self.metadata = Some(metadata);
        self
    }
}

/// Parameters for listing sessions with optional filters.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ListSessionsParams {
    /// Filter sessions by agent ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// Maximum number of sessions to return.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// Wrapper for list API responses that return `{"data": [...]}`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct ListResponse<T> {
    pub data: Vec<T>,
}

// ─── Session Resources (File Mounting) ───────────────────────────────────────

/// A resource to mount in a session sandbox.
///
/// Used in `CreateSessionParams.resources` to mount files at creation time,
/// or via the session resources API to add/remove files on a running session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionResource {
    /// Must be `"file"`.
    #[serde(rename = "type")]
    pub resource_type: String,
    /// The file ID from the Files API.
    pub file_id: String,
    /// Optional mount path in the sandbox (e.g., `/workspace/data.csv`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mount_path: Option<String>,
}

impl SessionResource {
    /// Create a file resource to mount in a session.
    pub fn file(file_id: impl Into<String>) -> serde_json::Value {
        serde_json::json!({
            "type": "file",
            "file_id": file_id.into(),
        })
    }

    /// Create a file resource with a specific mount path.
    pub fn file_at(file_id: impl Into<String>, mount_path: impl Into<String>) -> serde_json::Value {
        serde_json::json!({
            "type": "file",
            "file_id": file_id.into(),
            "mount_path": mount_path.into(),
        })
    }

    /// Create a GitHub repository resource.
    pub fn github_repo(
        url: impl Into<String>,
        mount_path: impl Into<String>,
        token: impl Into<String>,
    ) -> serde_json::Value {
        serde_json::json!({
            "type": "github_repository",
            "url": url.into(),
            "mount_path": mount_path.into(),
            "authorization_token": token.into(),
        })
    }
}

/// A resource attached to a session (returned by the API).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionResourceResponse {
    /// Resource ID (e.g., `"sesrsc_01ABC..."`).
    pub id: String,
    /// Resource type.
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub resource_type: Option<String>,
    /// The file ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_id: Option<String>,
    /// Mount path in the sandbox.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mount_path: Option<String>,
    /// Additional fields.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

// ─── Multiagent Orchestration ────────────────────────────────────────────────

/// Helper for building multiagent coordinator configurations.
pub struct Multiagent;

impl Multiagent {
    /// Create a coordinator configuration with a roster of agents.
    ///
    /// Pass agent references built with `Multiagent::agent_ref()` or `Multiagent::self_ref()`.
    pub fn coordinator(agents: Vec<serde_json::Value>) -> serde_json::Value {
        serde_json::json!({
            "type": "coordinator",
            "agents": agents,
        })
    }

    /// Reference a previously created agent by ID (uses latest version).
    pub fn agent_ref(agent_id: impl Into<String>) -> serde_json::Value {
        serde_json::json!({
            "type": "agent",
            "id": agent_id.into(),
        })
    }

    /// Reference a specific version of an agent.
    pub fn agent_ref_versioned(agent_id: impl Into<String>, version: u64) -> serde_json::Value {
        serde_json::json!({
            "type": "agent",
            "id": agent_id.into(),
            "version": version,
        })
    }

    /// Allow the coordinator to spawn copies of itself.
    pub fn self_ref() -> serde_json::Value {
        serde_json::json!({"type": "self"})
    }
}

// ─── Session Threads ─────────────────────────────────────────────────────────

/// A session thread (context-isolated event stream for a specific agent).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionThread {
    /// Thread ID (e.g., `"sth_01ABC..."`).
    pub id: String,
    /// Thread status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// The agent running in this thread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<serde_json::Value>,
    /// Parent thread ID (null for the primary thread).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_thread_id: Option<String>,
    /// Additional fields.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}
