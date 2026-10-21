//! Event types — user events (client→agent) and session events (agent→client).

use serde::{Deserialize, Serialize};

use super::session::Usage;

/// A client-to-agent event.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum UserEvent {
    /// Send a text message to the agent.
    #[serde(rename = "user.message")]
    Message { content: Vec<ContentBlock> },

    /// Interrupt the agent's current turn.
    #[serde(rename = "user.interrupt")]
    Interrupt,

    /// Allow or deny a pending tool use.
    #[serde(rename = "user.tool_confirmation")]
    ToolConfirmation {
        tool_use_id: String,
        result: ConfirmationResult,
        #[serde(skip_serializing_if = "Option::is_none")]
        deny_message: Option<String>,
    },

    /// Provide a custom tool result.
    #[serde(rename = "user.custom_tool_result")]
    CustomToolResult { custom_tool_use_id: String, content: Vec<ContentBlock> },

    /// Define an outcome for the session.
    #[serde(rename = "user.define_outcome")]
    DefineOutcome { criteria: String },
}

/// Confirmation result for tool use (allow or deny).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ConfirmationResult {
    Allow,
    Deny,
}

impl UserEvent {
    /// Create a `user.message` event with the given text.
    pub fn message(text: impl Into<String>) -> Self {
        Self::Message { content: vec![ContentBlock::text(text)] }
    }

    /// Create a `user.interrupt` event.
    pub fn interrupt() -> Self {
        Self::Interrupt
    }

    /// Create a `user.tool_confirmation` event that allows a pending tool use.
    pub fn allow_tool(tool_use_id: impl Into<String>) -> Self {
        Self::ToolConfirmation {
            tool_use_id: tool_use_id.into(),
            result: ConfirmationResult::Allow,
            deny_message: None,
        }
    }

    /// Create a `user.tool_confirmation` event that denies a pending tool use.
    pub fn deny_tool(tool_use_id: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::ToolConfirmation {
            tool_use_id: tool_use_id.into(),
            result: ConfirmationResult::Deny,
            deny_message: Some(reason.into()),
        }
    }

    /// Create a `user.custom_tool_result` event with the tool use ID and content blocks.
    pub fn custom_tool_result(
        custom_tool_use_id: impl Into<String>,
        content: Vec<ContentBlock>,
    ) -> Self {
        Self::CustomToolResult { custom_tool_use_id: custom_tool_use_id.into(), content }
    }

    /// Create a `user.define_outcome` event with the given success criteria.
    pub fn define_outcome(criteria: impl Into<String>) -> Self {
        Self::DefineOutcome { criteria: criteria.into() }
    }
}

/// An agent-to-client event received from the SSE stream.
///
/// Each variant carries a monotonic `seq` field used for SSE reconnection
/// (via `Last-Event-ID`). The `Unknown` catch-all ensures forward compatibility
/// with new event types added by the platform.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum SessionEvent {
    /// Agent sent a text message.
    #[serde(rename = "agent.message")]
    Message { seq: u64, content: Vec<ContentBlock> },

    /// Agent is using a built-in tool.
    #[serde(rename = "agent.tool_use")]
    ToolUse { seq: u64, tool_use_id: String, name: String, input: serde_json::Value },

    /// Agent is using a custom tool (requires client to provide result).
    #[serde(rename = "agent.custom_tool_use")]
    CustomToolUse { seq: u64, custom_tool_use_id: String, name: String, input: serde_json::Value },

    /// Agent is using an MCP tool.
    #[serde(rename = "agent.mcp_tool_use")]
    McpToolUse {
        seq: u64,
        // A self-hosted `adk-managed` runtime names the ID `tool_use_id` and sends no server.
        #[serde(alias = "tool_use_id")]
        mcp_tool_use_id: String,
        #[serde(default)]
        server_name: String,
        name: String,
        input: serde_json::Value,
    },

    /// Session status changed to idle (turn ended).
    #[serde(rename = "status.idle")]
    StatusIdle {
        seq: u64,
        #[serde(default)]
        stop_reason: Option<StopReason>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
    },

    /// Session status changed to running.
    #[serde(rename = "status.running")]
    StatusRunning { seq: u64 },

    /// An error occurred during the session.
    #[serde(rename = "agent.error", alias = "error")]
    Error { seq: u64, message: String, code: Option<String> },

    /// Unknown event type (forward-compatible catch-all).
    #[serde(other)]
    Unknown,
}

/// Why a turn ended.
///
/// Serialized with a `type` tag. Both `type` and `reason` tags are read, so the events
/// of a self-hosted `adk-managed` runtime parse as the service's do.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum StopReason {
    /// The agent completed its turn naturally.
    EndTurn,
    /// The agent requires user action (e.g., tool confirmation).
    RequiresAction { event_ids: Vec<String> },
    /// The agent hit the maximum token limit.
    MaxTokens,
}

impl<'de> Deserialize<'de> for StopReason {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Tagged {
            #[serde(rename = "type", alias = "reason")]
            kind: String,
            #[serde(default)]
            event_ids: Option<Vec<String>>,
        }
        let tagged = Tagged::deserialize(deserializer)?;
        match (tagged.kind.as_str(), tagged.event_ids) {
            ("end_turn", _) => Ok(Self::EndTurn),
            ("max_tokens", _) => Ok(Self::MaxTokens),
            ("requires_action", Some(event_ids)) => Ok(Self::RequiresAction { event_ids }),
            ("requires_action", None) => Err(serde::de::Error::missing_field("event_ids")),
            (other, _) => Err(serde::de::Error::unknown_variant(
                other,
                &["end_turn", "requires_action", "max_tokens"],
            )),
        }
    }
}

/// A content block within a message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image { source: String },
    #[serde(rename = "file")]
    File { file_id: String },
}

impl ContentBlock {
    /// Create a text content block.
    pub fn text(s: impl Into<String>) -> Self {
        Self::Text { text: s.into() }
    }

    /// Create a file content block.
    pub fn file(id: impl Into<String>) -> Self {
        Self::File { file_id: id.into() }
    }
}

/// A stored event from the event history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredEvent {
    pub seq: u64,
    pub direction: EventDirection,
    pub payload: serde_json::Value,
    pub created_at: String,
}

/// Direction of a stored event.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EventDirection {
    User,
    Agent,
}
