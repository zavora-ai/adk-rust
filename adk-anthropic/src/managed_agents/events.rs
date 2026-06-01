//! Event types for the Managed Agents API (UserEvent, SessionEvent).
//!
//! Aligned with the official Anthropic Managed Agents API documentation.
//! See: <https://platform.claude.com/docs/en/managed-agents/events-and-streaming>

use serde::{Deserialize, Serialize};

/// Events sent by the client to a managed agent session.
///
/// Each variant serializes with a `type` discriminator field matching the
/// Anthropic Managed Agents API event type (e.g., `"user.message"`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UserEvent {
    /// Send a message to the agent.
    #[serde(rename = "user.message")]
    Message { content: Vec<ContentBlock> },

    /// Interrupt the agent's current execution.
    #[serde(rename = "user.interrupt")]
    Interrupt {},

    /// Return the result of a custom tool execution.
    ///
    /// The `custom_tool_use_id` must match the event ID from the
    /// `agent.custom_tool_use` event that triggered this response.
    #[serde(rename = "user.custom_tool_result")]
    CustomToolResult {
        /// The event ID of the `agent.custom_tool_use` event.
        custom_tool_use_id: String,
        /// The result content blocks.
        content: Vec<ContentBlock>,
    },

    /// Approve or deny a tool confirmation request.
    ///
    /// The `tool_use_id` must match the event ID from the blocking
    /// `agent.tool_use` or `agent.mcp_tool_use` event.
    #[serde(rename = "user.tool_confirmation")]
    ToolConfirmation {
        /// The event ID of the tool use event requiring confirmation.
        tool_use_id: String,
        /// `"allow"` or `"deny"`.
        result: String,
        /// Optional explanation when denying.
        #[serde(skip_serializing_if = "Option::is_none")]
        deny_message: Option<String>,
    },

    /// Define success criteria for the session.
    #[serde(rename = "user.define_outcome")]
    DefineOutcome { criteria: String },

    /// Return the result of a built-in tool execution (self-hosted environments only).
    #[serde(rename = "user.tool_result")]
    ToolResult { tool_use_id: String, content: Vec<ContentBlock> },
}

/// Events received from the server via SSE during a managed agent session.
///
/// Each variant serializes with a `type` discriminator field matching the
/// Anthropic Managed Agents API event type (e.g., `"agent.message"`).
///
/// The `Unknown` variant provides forward compatibility — any unrecognized
/// event type deserializes to `Unknown` rather than producing an error.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEvent {
    /// Agent response containing text content blocks.
    #[serde(rename = "agent.message")]
    AgentMessage {
        /// Content blocks (array of `{"type": "text", "text": "..."}` objects).
        #[serde(default)]
        content: serde_json::Value,
        /// Event ID.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },

    /// Agent invokes a pre-built agent tool.
    #[serde(rename = "agent.tool_use")]
    AgentToolUse {
        /// Event ID (used for tool confirmation).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        /// Tool name.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        /// Tool input parameters.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input: Option<serde_json::Value>,
    },

    /// Agent invokes a custom tool (caller must execute and return result).
    #[serde(rename = "agent.custom_tool_use")]
    AgentCustomToolUse {
        /// Event ID (pass as `custom_tool_use_id` in `user.custom_tool_result`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        /// Tool name.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        /// Tool input parameters.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input: Option<serde_json::Value>,
    },

    /// Agent invokes an MCP server tool.
    #[serde(rename = "agent.mcp_tool_use")]
    AgentMcpToolUse {
        /// Event ID.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        /// MCP server name.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        server_name: Option<String>,
        /// Tool name.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        /// Tool input parameters.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input: Option<serde_json::Value>,
    },

    /// The session has transitioned to idle status.
    /// Includes `stop_reason` indicating why the agent stopped.
    #[serde(rename = "session.status_idle")]
    StatusIdle {
        /// Why the agent stopped (e.g., `{"type": "end_turn"}` or
        /// `{"type": "requires_action", "event_ids": [...]}`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stop_reason: Option<serde_json::Value>,
    },

    /// The session has transitioned to running status.
    #[serde(rename = "session.status_running")]
    StatusRunning {},

    /// A session error occurred.
    #[serde(rename = "session.error")]
    Error {
        /// Error details.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<serde_json::Value>,
        /// Legacy: error message string.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },

    /// Catch-all for unknown event types (forward compatibility).
    #[serde(other)]
    Unknown,
}

/// A content block within a message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    /// A text content block.
    Text { text: String },
}

impl ContentBlock {
    /// Create a text content block.
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }
}

impl UserEvent {
    /// Create a message event with a single text content block.
    pub fn message(text: impl Into<String>) -> Self {
        Self::Message { content: vec![ContentBlock::text(text)] }
    }

    /// Create a custom tool result event.
    pub fn custom_tool_result(
        custom_tool_use_id: impl Into<String>,
        result_text: impl Into<String>,
    ) -> Self {
        Self::CustomToolResult {
            custom_tool_use_id: custom_tool_use_id.into(),
            content: vec![ContentBlock::text(result_text)],
        }
    }

    /// Create a tool confirmation event (allow).
    pub fn allow_tool(tool_use_id: impl Into<String>) -> Self {
        Self::ToolConfirmation {
            tool_use_id: tool_use_id.into(),
            result: "allow".to_string(),
            deny_message: None,
        }
    }

    /// Create a tool confirmation event (deny).
    pub fn deny_tool(tool_use_id: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::ToolConfirmation {
            tool_use_id: tool_use_id.into(),
            result: "deny".to_string(),
            deny_message: Some(reason.into()),
        }
    }
}

/// Wrapper for sending events to the API.
///
/// The API expects events to be wrapped in `{"events": [...]}`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct SendEventsRequest {
    pub events: Vec<UserEvent>,
}
