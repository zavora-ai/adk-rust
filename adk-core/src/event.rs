use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

use crate::types::{Content, InvocationId};

pub const KEY_PREFIX_APP: &str = "app:";
pub const KEY_PREFIX_USER: &str = "user:";
pub const KEY_PREFIX_TEMP: &str = "temp:";

/// Actions that an event can trigger, such as state updates or compaction.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EventActions {
    /// Incremental state updates to be merged into the session state.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub state_delta: HashMap<String, serde_json::Value>,
    /// Optional compaction instructions to truncate or summarize history.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compaction: Option<EventCompaction>,
    /// Optional agent name to transfer the conversation to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transfer_to_agent: Option<String>,
    /// Whether to escalate to a human or higher-level supervisor.
    #[serde(default)]
    pub escalate: bool,
    /// Whether to skip summarization for this event during compaction.
    #[serde(default)]
    pub skip_summarization: bool,
    /// Optional tool confirmation required before proceeding.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_confirmation: Option<serde_json::Value>,
    /// Decision for a pending tool confirmation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_confirmation_decision: Option<bool>,
    /// Incremental artifact updates (name -> version).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub artifact_delta: HashMap<String, i32>,
}

/// Instructions for truncating or summarizing conversation history.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EventCompaction {
    /// The event ID after which history should be truncated (exclusive).
    #[serde(default)]
    pub truncate_before_id: String,
    /// A summary of the truncated history to be preserved as context.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// The content that summarizes the history.
    #[serde(default)]
    pub compacted_content: Content,
    /// The timestamp boundary for the compaction.
    #[serde(default = "Utc::now")]
    pub end_timestamp: DateTime<Utc>,
    /// Compatibility field for some older implementations.
    #[serde(default = "Utc::now")]
    pub start_timestamp: DateTime<Utc>,
}

/// A single event in an agent's lifecycle.
///
/// This is the primary data structure for communication between agents,
/// tools, and the host application.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Event {
    /// Unique event identifier.
    pub id: String,
    /// The invocation this event belongs to.
    pub invocation_id: InvocationId,
    /// When the event occurred.
    pub timestamp: DateTime<Utc>,
    /// The author of the event (e.g., "user", "agent_name", "tool_name").
    pub author: String,
    /// The branch this event belongs to (for multi-branch conversations).
    #[serde(default)]
    pub branch: String,
    /// Actions triggered by this event (state updates, compaction).
    #[serde(default)]
    pub actions: EventActions,
    /// The actual response from the LLM, if any.
    #[serde(flatten)]
    pub llm_response: LlmResponse,
    /// Optional structured data associated with the event.
    #[serde(default)]
    pub data: serde_json::Value,
    /// Optional metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    /// Control flags for the host or downstream agents.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub control: HashMap<String, serde_json::Value>,
    /// The raw LLM request that generated this event (debugging/telemetry).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm_request: Option<String>,
    /// Provider-specific metadata.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub provider_metadata: HashMap<String, String>,
    /// IDs of tools that are still running.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub long_running_tool_ids: Vec<String>,
}

impl Event {
    pub fn new(invocation_id: impl Into<InvocationId>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            invocation_id: invocation_id.into(),
            timestamp: Utc::now(),
            author: String::new(),
            branch: String::new(),
            actions: EventActions::default(),
            llm_response: LlmResponse::default(),
            data: serde_json::Value::Null,
            metadata: None,
            control: HashMap::new(),
            llm_request: None,
            provider_metadata: HashMap::new(),
            long_running_tool_ids: Vec::new(),
        }
    }

    pub fn with_id(id: impl Into<String>, invocation_id: impl Into<InvocationId>) -> Self {
        let mut event = Self::new(invocation_id);
        event.id = id.into();
        event
    }

    pub fn with_author(mut self, author: impl Into<String>) -> Self {
        self.author = author.into();
        self
    }

    pub fn with_data(mut self, data: serde_json::Value) -> Self {
        self.data = data;
        self
    }

    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = Some(metadata);
        self
    }

    pub fn with_control(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.control.insert(key.into(), value);
        self
    }

    pub fn content(&self) -> Option<&Content> {
        self.llm_response.content.as_ref()
    }

    pub fn set_content(&mut self, content: Content) {
        self.llm_response.content = Some(content);
    }

    pub fn with_content(mut self, content: Content) -> Self {
        self.llm_response.content = Some(content);
        self
    }

    pub fn is_final_response(&self) -> bool {
        self.llm_response.turn_complete && !self.llm_response.partial
    }

    pub fn function_call_ids(&self) -> Vec<String> {
        let mut ids = Vec::new();
        if let Some(content) = self.content() {
            for part in &content.parts {
                if let crate::Part::FunctionCall { id: Some(id), .. } = part {
                    ids.push(id.clone());
                }
            }
        }
        ids
    }
}

/// The response part of an Event, encapsulating LLM output and usage stats.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct LlmResponse {
    /// Whether this is a partial response chunk.
    #[serde(default)]
    pub partial: bool,
    /// The generated content (text, tool calls, etc.).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Content>,
    /// Token usage and other performance metrics.
    #[serde(rename = "usage", skip_serializing_if = "Option::is_none", alias = "usageMetadata")]
    pub usage_metadata: Option<UsageMetadata>,
    /// Why the model stopped generating.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<FinishReason>,
    /// Whether the generation was interrupted.
    #[serde(default)]
    pub interrupted: bool,
    /// Whether the turn is complete.
    #[serde(default)]
    pub turn_complete: bool,
    /// Optional error code from the provider.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    /// Optional error message from the provider.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}

/// Token usage metadata for an LLM request.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UsageMetadata {
    #[serde(alias = "promptTokens")]
    pub prompt_token_count: i32,
    #[serde(alias = "candidatesTokens")]
    pub candidates_token_count: i32,
    #[serde(alias = "totalTokens")]
    pub total_token_count: i32,
    #[serde(skip_serializing_if = "Option::is_none", default, alias = "promptCacheHitTokens")]
    pub cache_read_input_token_count: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none", default, alias = "promptCacheMissTokens")]
    pub cache_creation_input_token_count: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none", default, alias = "reasoningTokens")]
    pub thinking_token_count: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub audio_input_token_count: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub audio_output_token_count: Option<i32>,
}

/// Reasons why an LLM might stop generating.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    /// Model reached a natural stop point or provided stop sequence.
    Stop,
    /// Model reached the maximum token limit.
    MaxTokens,
    /// Content was flagged by safety filters.
    Safety,
    /// Content was flagged as recitation.
    Recitation,
    /// Model triggered a tool call.
    ToolCalls,
    /// Other reasons.
    Other(String),
}
