use crate::context::{ToolConfirmationDecision, ToolConfirmationRequest};
use crate::model::LlmResponse;
use crate::types::Content;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

// State scope prefixes
pub const KEY_PREFIX_APP: &str = "app:";
pub const KEY_PREFIX_TEMP: &str = "temp:";
pub const KEY_PREFIX_USER: &str = "user:";

/// Event represents a single interaction in a conversation.
/// This struct embeds LlmResponse to match ADK-Go's design pattern.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub invocation_id: String,
    pub branch: String,
    pub author: String,
    /// The LLM response containing content and metadata.
    /// Access content via `event.llm_response.content`.
    #[serde(flatten)]
    pub llm_response: LlmResponse,
    pub actions: EventActions,
    /// IDs of long-running tools associated with this event.
    #[serde(default)]
    pub long_running_tool_ids: Vec<String>,
    /// LLM request data for UI display (JSON string)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm_request: Option<String>,
    /// Provider-specific metadata (e.g., GCP Vertex, Azure OpenAI).
    /// Keeps the core Event struct provider-agnostic.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub provider_metadata: HashMap<String, String>,
}

/// Metadata for a compacted (summarized) event.
/// When context compaction is enabled, older events are summarized into a single
/// compacted event containing this metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventCompaction {
    /// Timestamp of the earliest event that was compacted.
    pub start_timestamp: DateTime<Utc>,
    /// Timestamp of the latest event that was compacted.
    pub end_timestamp: DateTime<Utc>,
    /// The summarized content replacing the original events.
    pub compacted_content: Content,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EventActions {
    pub state_delta: HashMap<String, serde_json::Value>,
    pub artifact_delta: HashMap<String, i64>,
    pub skip_summarization: bool,
    pub transfer_to_agent: Option<String>,
    pub escalate: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_confirmation: Option<ToolConfirmationRequest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_confirmation_decision: Option<ToolConfirmationDecision>,
    /// Present when this event is a compaction summary replacing older events.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compaction: Option<EventCompaction>,
}

impl Event {
    pub fn new(invocation_id: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            invocation_id: invocation_id.into(),
            branch: String::new(),
            author: String::new(),
            llm_response: LlmResponse::default(),
            actions: EventActions::default(),
            long_running_tool_ids: Vec::new(),
            llm_request: None,
            provider_metadata: HashMap::new(),
        }
    }

    /// Create an event with a specific ID.
    /// Use this for streaming events where all chunks should share the same event ID.
    pub fn with_id(id: impl Into<String>, invocation_id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            timestamp: Utc::now(),
            invocation_id: invocation_id.into(),
            branch: String::new(),
            author: String::new(),
            llm_response: LlmResponse::default(),
            actions: EventActions::default(),
            long_running_tool_ids: Vec::new(),
            llm_request: None,
            provider_metadata: HashMap::new(),
        }
    }

    /// Convenience method to access content directly.
    pub fn content(&self) -> Option<&Content> {
        self.llm_response.content.as_ref()
    }

    /// Convenience method to set content directly.
    pub fn set_content(&mut self, content: Content) {
        self.llm_response.content = Some(content);
    }

    /// Returns whether the event is the final response of an agent.
    ///
    /// An event is considered final if:
    /// - It has skip_summarization set, OR
    /// - It has long_running_tool_ids (indicating async operations), OR
    /// - It has no function calls, no function responses, is not partial,
    ///   and has no trailing code execution results.
    ///
    /// Note: When multiple agents participate in one invocation, there could be
    /// multiple events with is_final_response() as true, for each participating agent.
    pub fn is_final_response(&self) -> bool {
        // If skip_summarization is set or we have long-running tools, it's final
        if self.actions.skip_summarization || !self.long_running_tool_ids.is_empty() {
            return true;
        }

        // Check content for function calls/responses
        let has_function_calls = self.has_function_calls();
        let has_function_responses = self.has_function_responses();
        let is_partial = self.llm_response.partial;
        let has_trailing_code_result = self.has_trailing_code_execution_result();

        !has_function_calls && !has_function_responses && !is_partial && !has_trailing_code_result
    }

    /// Returns true if the event content contains function calls.
    fn has_function_calls(&self) -> bool {
        if let Some(content) = &self.llm_response.content {
            for part in &content.parts {
                if matches!(part, crate::Part::FunctionCall { .. }) {
                    return true;
                }
            }
        }
        false
    }

    /// Returns true if the event content contains function responses.
    fn has_function_responses(&self) -> bool {
        if let Some(content) = &self.llm_response.content {
            for part in &content.parts {
                if matches!(part, crate::Part::FunctionResponse { .. }) {
                    return true;
                }
            }
        }
        false
    }

    /// Returns true if the event has a trailing code execution result.
    #[allow(clippy::match_like_matches_macro)]
    fn has_trailing_code_execution_result(&self) -> bool {
        if let Some(content) = &self.llm_response.content {
            if let Some(last_part) = content.parts.last() {
                // FunctionResponse as the last part indicates a code execution result
                // that the model still needs to process.
                return matches!(last_part, crate::Part::FunctionResponse { .. });
            }
        }
        false
    }

    /// Extracts function call IDs from this event's content.
    /// Used to identify which function calls are associated with long-running tools.
    pub fn function_call_ids(&self) -> Vec<String> {
        let mut ids = Vec::new();
        if let Some(content) = &self.llm_response.content {
            for part in &content.parts {
                if let crate::Part::FunctionCall { name, id, .. } = part {
                    // Use the actual call ID when available (OpenAI-style),
                    // fall back to name for providers that don't emit IDs (Gemini).
                    ids.push(id.as_deref().unwrap_or(name).to_string());
                }
            }
        }
        ids
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Part;

    #[test]
    fn test_event_creation() {
        let event = Event::new("inv-123");
        assert_eq!(event.invocation_id, "inv-123");
        assert!(!event.id.is_empty());
    }

    #[test]
    fn test_event_actions_default() {
        let actions = EventActions::default();
        assert!(actions.state_delta.is_empty());
        assert!(!actions.skip_summarization);
        assert!(actions.tool_confirmation.is_none());
        assert!(actions.tool_confirmation_decision.is_none());
    }

    #[test]
    fn test_state_prefixes() {
        assert_eq!(KEY_PREFIX_APP, "app:");
        assert_eq!(KEY_PREFIX_TEMP, "temp:");
        assert_eq!(KEY_PREFIX_USER, "user:");
    }

    #[test]
    fn test_is_final_response_no_content() {
        let event = Event::new("inv-123");
        // No content, no function calls -> final
        assert!(event.is_final_response());
    }

    #[test]
    fn test_is_final_response_text_only() {
        let mut event = Event::new("inv-123");
        event.llm_response.content = Some(Content {
            role: "model".to_string(),
            parts: vec![Part::Text { text: "Hello!".to_string() }],
        });
        // Text only, no function calls -> final
        assert!(event.is_final_response());
    }

    #[test]
    fn test_is_final_response_with_function_call() {
        let mut event = Event::new("inv-123");
        event.llm_response.content = Some(Content {
            role: "model".to_string(),
            parts: vec![Part::FunctionCall {
                name: "get_weather".to_string(),
                args: serde_json::json!({"city": "NYC"}),
                id: Some("call_123".to_string()),
                thought_signature: None,
            }],
        });
        // Has function call -> NOT final (need to execute it)
        assert!(!event.is_final_response());
    }

    #[test]
    fn test_is_final_response_with_function_response() {
        let mut event = Event::new("inv-123");
        event.llm_response.content = Some(Content {
            role: "function".to_string(),
            parts: vec![Part::FunctionResponse {
                function_response: crate::FunctionResponseData {
                    name: "get_weather".to_string(),
                    response: serde_json::json!({"temp": 72}),
                },
                id: Some("call_123".to_string()),
            }],
        });
        // Has function response -> NOT final (model needs to respond)
        assert!(!event.is_final_response());
    }

    #[test]
    fn test_is_final_response_partial() {
        let mut event = Event::new("inv-123");
        event.llm_response.partial = true;
        event.llm_response.content = Some(Content {
            role: "model".to_string(),
            parts: vec![Part::Text { text: "Hello...".to_string() }],
        });
        // Partial response -> NOT final
        assert!(!event.is_final_response());
    }

    #[test]
    fn test_is_final_response_skip_summarization() {
        let mut event = Event::new("inv-123");
        event.actions.skip_summarization = true;
        event.llm_response.content = Some(Content {
            role: "function".to_string(),
            parts: vec![Part::FunctionResponse {
                function_response: crate::FunctionResponseData {
                    name: "tool".to_string(),
                    response: serde_json::json!({"result": "done"}),
                },
                id: Some("call_tool".to_string()),
            }],
        });
        // Even with function response, skip_summarization makes it final
        assert!(event.is_final_response());
    }

    #[test]
    fn test_is_final_response_long_running_tool_ids() {
        let mut event = Event::new("inv-123");
        event.long_running_tool_ids = vec!["process_video".to_string()];
        event.llm_response.content = Some(Content {
            role: "model".to_string(),
            parts: vec![Part::FunctionCall {
                name: "process_video".to_string(),
                args: serde_json::json!({"file": "video.mp4"}),
                id: Some("call_process".to_string()),
                thought_signature: None,
            }],
        });
        // Has long_running_tool_ids -> final (async operation started)
        assert!(event.is_final_response());
    }

    #[test]
    fn test_function_call_ids() {
        let mut event = Event::new("inv-123");
        event.llm_response.content = Some(Content {
            role: "model".to_string(),
            parts: vec![
                Part::FunctionCall {
                    name: "get_weather".to_string(),
                    args: serde_json::json!({}),
                    id: Some("call_1".to_string()),
                    thought_signature: None,
                },
                Part::Text { text: "I'll check the weather".to_string() },
                Part::FunctionCall {
                    name: "get_time".to_string(),
                    args: serde_json::json!({}),
                    id: Some("call_2".to_string()),
                    thought_signature: None,
                },
            ],
        });

        let ids = event.function_call_ids();
        assert_eq!(ids.len(), 2);
        // Should use actual call IDs, not function names
        assert!(ids.contains(&"call_1".to_string()));
        assert!(ids.contains(&"call_2".to_string()));
    }

    #[test]
    fn test_function_call_ids_falls_back_to_name() {
        let mut event = Event::new("inv-123");
        event.llm_response.content = Some(Content {
            role: "model".to_string(),
            parts: vec![Part::FunctionCall {
                name: "get_weather".to_string(),
                args: serde_json::json!({}),
                id: None, // Gemini-style: no explicit ID
                thought_signature: None,
            }],
        });

        let ids = event.function_call_ids();
        assert_eq!(ids, vec!["get_weather".to_string()]);
    }

    #[test]
    fn test_function_call_ids_empty() {
        let event = Event::new("inv-123");
        let ids = event.function_call_ids();
        assert!(ids.is_empty());
    }

    #[test]
    fn test_is_final_response_trailing_function_response() {
        // Text followed by a function response as the last part —
        // has_trailing_code_execution_result should catch this even though
        // has_function_responses also catches it.
        let mut event = Event::new("inv-123");
        event.llm_response.content = Some(Content {
            role: "model".to_string(),
            parts: vec![
                Part::Text { text: "Running code...".to_string() },
                Part::FunctionResponse {
                    function_response: crate::FunctionResponseData {
                        name: "code_exec".to_string(),
                        response: serde_json::json!({"output": "42"}),
                    },
                    id: Some("call_exec".to_string()),
                },
            ],
        });
        // Trailing function response -> NOT final
        assert!(!event.is_final_response());
    }

    #[test]
    fn test_is_final_response_text_after_function_response() {
        // Function response followed by text — the trailing part is text,
        // so has_trailing_code_execution_result is false, but
        // has_function_responses is still true.
        let mut event = Event::new("inv-123");
        event.llm_response.content = Some(Content {
            role: "model".to_string(),
            parts: vec![
                Part::FunctionResponse {
                    function_response: crate::FunctionResponseData {
                        name: "tool".to_string(),
                        response: serde_json::json!({}),
                    },
                    id: Some("call_1".to_string()),
                },
                Part::Text { text: "Done".to_string() },
            ],
        });
        // Still has function responses -> NOT final
        assert!(!event.is_final_response());
    }
}
