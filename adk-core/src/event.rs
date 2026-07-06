use crate::context::{ToolConfirmationDecision, ToolConfirmationRequest};
use crate::model::LlmResponse;
use crate::types::Content;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

// State scope prefixes
/// Key prefix for application-scoped state (persists across sessions).
pub const KEY_PREFIX_APP: &str = "app:";
/// Key prefix for temporary state (cleared each turn).
pub const KEY_PREFIX_TEMP: &str = "temp:";
/// Key prefix for user-scoped state (persists across sessions).
pub const KEY_PREFIX_USER: &str = "user:";

/// Event-level `provider_metadata` key marking a tool-progress event and naming
/// its output stream (e.g. `"stdout"`, `"stderr"`). Present only on events
/// produced by [`ToolContext::emit_progress`](crate::ToolContext::emit_progress).
pub const TOOL_PROGRESS_STREAM_KEY: &str = "adk.tool_progress.stream";

/// Event-level `provider_metadata` key carrying the originating tool's
/// function-call id on a tool-progress event.
pub const TOOL_PROGRESS_CALL_ID_KEY: &str = "adk.tool_progress.call_id";

/// Event represents a single interaction in a conversation.
/// This struct embeds LlmResponse to match ADK-Go's design pattern.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    /// Unique identifier for this event.
    pub id: String,
    /// When this event was created.
    pub timestamp: DateTime<Utc>,
    /// The invocation that produced this event.
    pub invocation_id: String,
    /// The conversation branch this event belongs to.
    pub branch: String,
    /// The agent or role that authored this event.
    pub author: String,
    /// The LLM response containing content and metadata.
    /// Access content via `event.llm_response.content`.
    #[serde(flatten)]
    pub llm_response: LlmResponse,
    /// Actions to apply (state changes, transfers, confirmations).
    pub actions: EventActions,
    /// IDs of long-running tools associated with this event.
    #[serde(default)]
    pub long_running_tool_ids: Vec<String>,
    /// LLM request data for UI display (JSON string)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm_request: Option<String>,
    /// Provider-specific metadata (e.g., GCP Vertex, Azure OpenAI).
    /// Keeps the core Event struct provider-agnostic.
    /// Serialized as `"event_metadata"` to avoid collision with
    /// [`LlmResponse::provider_metadata`](crate::LlmResponse) when flattened.
    #[serde(default, skip_serializing_if = "HashMap::is_empty", rename = "event_metadata")]
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

/// Actions to apply as side effects of an event.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EventActions {
    /// State key-value changes to apply.
    pub state_delta: HashMap<String, serde_json::Value>,
    /// Artifact version changes.
    pub artifact_delta: HashMap<String, i64>,
    /// Whether to skip summarization for this event.
    pub skip_summarization: bool,
    /// Agent name to transfer control to.
    pub transfer_to_agent: Option<String>,
    /// Whether to escalate to a human operator.
    pub escalate: bool,
    /// Tool confirmation request awaiting human approval.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_confirmation: Option<ToolConfirmationRequest>,
    /// Decision for a pending tool confirmation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_confirmation_decision: Option<ToolConfirmationDecision>,
    /// Present when this event is a compaction summary replacing older events.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compaction: Option<EventCompaction>,
    /// Target node names for dynamic route dispatch in graph workflows.
    /// When non-empty, the graph executor routes to these nodes instead of
    /// following static edges.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<Vec<String>>,
}

/// A typed, borrowed view of a single tool call carried by an [`Event`].
///
/// Produced by [`Event::tool_calls`]. Lets UI/event consumers render the tool a
/// model requested without matching on [`Part::FunctionCall`](crate::Part::FunctionCall)
/// internals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolCallView<'a> {
    /// Provider-assigned call id (OpenAI-style). `None` for providers that omit
    /// it (e.g. Gemini); fall back to [`name`](Self::name) for correlation.
    pub call_id: Option<&'a str>,
    /// The tool/function name the model requested.
    pub name: &'a str,
    /// The call arguments as raw JSON.
    pub args: &'a serde_json::Value,
}

/// A typed, borrowed view of a single tool result carried by an [`Event`].
///
/// Produced by [`Event::tool_results`]. Surfaces a completed tool's output
/// generically so any tool — streaming or not — can be rendered from the event
/// stream without walking [`Part::FunctionResponse`](crate::Part::FunctionResponse)
/// internals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolResultView<'a> {
    /// Provider-assigned call id (OpenAI-style), correlating this result with
    /// its originating [`ToolCallView`] and progress chunks. `None` for
    /// providers that omit it (e.g. Gemini); fall back to [`name`](Self::name).
    pub call_id: Option<&'a str>,
    /// The tool/function name that produced this result.
    pub name: &'a str,
    /// The tool's JSON response payload.
    pub response: &'a serde_json::Value,
}

impl Event {
    /// Creates a new event with a generated UUID and current timestamp.
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

    /// Creates a streaming tool-progress event.
    ///
    /// Tools emit these via [`ToolContext::emit_progress`](crate::ToolContext::emit_progress)
    /// to push intermediate stdout/stderr to the client *while the tool is still
    /// running*. The event carries the chunk as partial text content (role
    /// `"tool"`) and is tagged with [`TOOL_PROGRESS_STREAM_KEY`] /
    /// [`TOOL_PROGRESS_CALL_ID_KEY`] so consumers can distinguish it from a
    /// final tool result and route it to the right terminal widget.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_core::Event;
    ///
    /// let event = Event::tool_progress("inv-1", "agent", "call-7", "stdout", "compiling...\n");
    /// assert_eq!(event.tool_progress_stream(), Some("stdout"));
    /// assert!(event.llm_response.partial);
    /// ```
    pub fn tool_progress(
        invocation_id: impl Into<String>,
        author: impl Into<String>,
        function_call_id: impl Into<String>,
        stream: impl Into<String>,
        chunk: impl Into<String>,
    ) -> Self {
        let mut event = Event::new(invocation_id);
        event.author = author.into();
        event.llm_response.content = Some(Content {
            role: "tool".to_string(),
            parts: vec![crate::types::Part::Text { text: chunk.into() }],
        });
        // Partial so downstream aggregation/persistence treats it as a streaming
        // chunk, never as the agent's final response.
        event.llm_response.partial = true;
        event.provider_metadata.insert(TOOL_PROGRESS_STREAM_KEY.to_string(), stream.into());
        event
            .provider_metadata
            .insert(TOOL_PROGRESS_CALL_ID_KEY.to_string(), function_call_id.into());
        event
    }

    /// Returns the progress stream name (`"stdout"`, `"stderr"`, …) if this is a
    /// tool-progress event produced by [`ToolContext::emit_progress`](crate::ToolContext::emit_progress),
    /// otherwise `None`.
    pub fn tool_progress_stream(&self) -> Option<&str> {
        self.provider_metadata.get(TOOL_PROGRESS_STREAM_KEY).map(String::as_str)
    }

    /// Returns the tool calls carried by this event, as a typed, render-ready view.
    ///
    /// A UI consuming the agent's `EventStream` can call this on every event to
    /// detect when the model requested one or more tools, without matching on
    /// [`Part::FunctionCall`](crate::Part::FunctionCall) internals. Pair it with
    /// [`tool_results`](Self::tool_results) and [`tool_progress_stream`](Self::tool_progress_stream)
    /// to render a complete tool lifecycle (call → live progress → result).
    ///
    /// Returns an empty vector for events that contain no tool calls.
    ///
    /// # Correlation
    ///
    /// Use [`ToolCallView::call_id`] to correlate a call with its progress chunks
    /// and final result. For providers that omit call ids (e.g. Gemini), fall
    /// back to [`ToolCallView::name`].
    ///
    /// # Example
    ///
    /// ```
    /// use adk_core::{Content, Event, Part};
    ///
    /// let mut event = Event::new("inv-1");
    /// event.llm_response.content = Some(Content {
    ///     role: "model".to_string(),
    ///     parts: vec![Part::FunctionCall {
    ///         name: "bash".to_string(),
    ///         args: serde_json::json!({ "command": "ls" }),
    ///         id: Some("call_1".to_string()),
    ///         thought_signature: None,
    ///     }],
    /// });
    ///
    /// let calls = event.tool_calls();
    /// assert_eq!(calls.len(), 1);
    /// assert_eq!(calls[0].name, "bash");
    /// assert_eq!(calls[0].call_id, Some("call_1"));
    /// ```
    pub fn tool_calls(&self) -> Vec<ToolCallView<'_>> {
        let Some(content) = &self.llm_response.content else {
            return Vec::new();
        };
        content
            .parts
            .iter()
            .filter_map(|part| match part {
                crate::types::Part::FunctionCall { name, args, id, .. } => {
                    Some(ToolCallView { call_id: id.as_deref(), name, args })
                }
                _ => None,
            })
            .collect()
    }

    /// Returns the tool results carried by this event, as a typed, render-ready view.
    ///
    /// After a tool executes, the agent yields its result on the same
    /// `EventStream` as everything else, as a `function`-role event holding a
    /// [`Part::FunctionResponse`](crate::Part::FunctionResponse). This accessor
    /// surfaces those results generically so a UI can render the output of *any*
    /// tool — streaming or not — without walking part internals.
    ///
    /// Returns an empty vector for events that contain no tool results.
    ///
    /// # Correlation
    ///
    /// Use [`ToolResultView::call_id`] to attach a result to the originating
    /// [`tool_calls`](Self::tool_calls) entry and its progress chunks. For
    /// providers that omit call ids, fall back to [`ToolResultView::name`].
    ///
    /// # Example
    ///
    /// ```
    /// use adk_core::{Content, Event, FunctionResponseData, Part};
    ///
    /// let mut event = Event::new("inv-1");
    /// event.llm_response.content = Some(Content {
    ///     role: "function".to_string(),
    ///     parts: vec![Part::FunctionResponse {
    ///         function_response: FunctionResponseData::new(
    ///             "bash",
    ///             serde_json::json!({ "stdout": "ok\n", "exit_code": 0 }),
    ///         ),
    ///         id: Some("call_1".to_string()),
    ///     }],
    /// });
    ///
    /// let results = event.tool_results();
    /// assert_eq!(results.len(), 1);
    /// assert_eq!(results[0].name, "bash");
    /// assert_eq!(results[0].call_id, Some("call_1"));
    /// assert_eq!(results[0].response["exit_code"], 0);
    /// ```
    pub fn tool_results(&self) -> Vec<ToolResultView<'_>> {
        let Some(content) = &self.llm_response.content else {
            return Vec::new();
        };
        content
            .parts
            .iter()
            .filter_map(|part| match part {
                crate::types::Part::FunctionResponse { function_response, id } => {
                    Some(ToolResultView {
                        call_id: id.as_deref(),
                        name: &function_response.name,
                        response: &function_response.response,
                    })
                }
                _ => None,
            })
            .collect()
    }

    /// Convenience method to access content directly.
    pub fn content(&self) -> Option<&Content> {
        self.llm_response.content.as_ref()
    }

    /// Convenience method to set content directly.
    pub fn set_content(&mut self, content: Content) {
        self.llm_response.content = Some(content);
    }

    /// Returns the Interactions API interaction id for this event, if present.
    ///
    /// Reads the id from the flattened [`LlmResponse`], mirroring ADK-Python's
    /// `event.interaction_id`. Returns `None` for events produced by the
    /// generateContent transport and non-Gemini providers.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_core::Event;
    ///
    /// let mut event = Event::new("inv-123");
    /// assert_eq!(event.interaction_id(), None);
    ///
    /// event.llm_response.interaction_id = Some("v1_abc".to_string());
    /// assert_eq!(event.interaction_id(), Some("v1_abc"));
    /// ```
    pub fn interaction_id(&self) -> Option<&str> {
        self.llm_response.interaction_id.as_deref()
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
        if let Some(content) = &self.llm_response.content
            && let Some(last_part) = content.parts.last()
        {
            // FunctionResponse as the last part indicates a code execution result
            // that the model still needs to process.
            return matches!(last_part, crate::Part::FunctionResponse { .. });
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
                function_response: crate::FunctionResponseData::new(
                    "get_weather",
                    serde_json::json!({"temp": 72}),
                ),
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
                function_response: crate::FunctionResponseData::new(
                    "tool",
                    serde_json::json!({"result": "done"}),
                ),
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
                    function_response: crate::FunctionResponseData::new(
                        "code_exec",
                        serde_json::json!({"output": "42"}),
                    ),
                    id: Some("call_exec".to_string()),
                },
            ],
        });
        // Trailing function response -> NOT final
        assert!(!event.is_final_response());
    }

    #[test]
    fn test_event_roundtrip_with_both_provider_metadata() {
        let mut event = Event::new("inv-1");
        event.provider_metadata.insert("adk.tool_progress.stream".into(), "stdout".into());
        event.provider_metadata.insert("adk.tool_progress.call_id".into(), "call-7".into());
        event.llm_response.provider_metadata = Some(serde_json::json!({"response_id": "resp-xyz"}));

        let json = serde_json::to_string(&event).expect("serialize");
        // Round-trip must succeed — regression test for the duplicate
        // `provider_metadata` flatten collision with LlmResponse.
        let back: Event = serde_json::from_str(&json)
            .expect("round-trip must succeed without duplicate field error");

        assert_eq!(
            back.provider_metadata.get("adk.tool_progress.stream").map(String::as_str),
            Some("stdout"),
        );
        assert_eq!(
            back.provider_metadata.get("adk.tool_progress.call_id").map(String::as_str),
            Some("call-7"),
        );
        assert_eq!(
            back.llm_response.provider_metadata,
            Some(serde_json::json!({"response_id": "resp-xyz"})),
        );
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
                    function_response: crate::FunctionResponseData::new(
                        "tool",
                        serde_json::json!({}),
                    ),
                    id: Some("call_1".to_string()),
                },
                Part::Text { text: "Done".to_string() },
            ],
        });
        // Still has function responses -> NOT final
        assert!(!event.is_final_response());
    }
}
