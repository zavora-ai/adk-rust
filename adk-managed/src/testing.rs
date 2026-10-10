//! Testing utilities for the managed agent runtime.
//!
//! This module provides deterministic test doubles for the managed agent runtime
//! pipeline. These are **not mocks** — they implement the real traits and exercise
//! the full runtime pipeline (parking, checkpoints, replay, event mapping). Only
//! the LLM provider API call is replaced with pre-scripted deterministic responses.
//!
//! # Architecture
//!
//! ```text
//! ScriptedLlm (deterministic responses)
//!   │
//!   ▼
//! Full runtime pipeline (SessionLoop, CheckpointManager, ToolParkingLot, etc.)
//!   │
//!   ▼
//! SessionEvent stream (byte-identical assertions possible)
//! ```
//!
//! # Usage
//!
//! ```rust,ignore
//! use adk_managed::testing::{ScriptedLlm, ScriptedTurn, ScriptedToolCall};
//! use serde_json::json;
//!
//! let turns = vec![
//!     ScriptedTurn {
//!         text: Some("Hello! How can I help you?".to_string()),
//!         tool_calls: vec![],
//!     },
//!     ScriptedTurn {
//!         text: None,
//!         tool_calls: vec![ScriptedToolCall {
//!             name: "web_search".to_string(),
//!             input: json!({"query": "rust async"}),
//!             id: Some("tc_001".to_string()),
//!         }],
//!     },
//! ];
//!
//! let llm = ScriptedLlm::new("scripted-model", turns);
//! // Use llm in place of any Arc<dyn Llm> in the runtime pipeline
//! ```

use adk_core::{
    Llm, LlmRequest, LlmResponse, LlmResponseStream, Result as AdkResult, types::Content,
};
use async_stream::stream;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicUsize, Ordering};

/// A pre-scripted turn that the [`ScriptedLlm`] will return.
///
/// Each turn represents one complete LLM response. It can contain text content,
/// tool calls, or both — mirroring real LLM behavior where a response may
/// include reasoning text followed by tool invocations.
///
/// # Wire Format
///
/// Serializes to/from JSON for use in fixture files:
///
/// ```json
/// {
///   "text": "Let me search for that.",
///   "tool_calls": [
///     { "name": "web_search", "input": {"query": "rust"}, "id": "tc_001" }
///   ]
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptedTurn {
    /// Text response content (if any).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Tool calls to make (if any).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ScriptedToolCall>,
}

/// A scripted tool call within a [`ScriptedTurn`].
///
/// Represents a function call that the LLM "decides" to make.
/// The `id` field maps to the tool_use_id / function call ID used
/// for round-trip correlation with tool results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptedToolCall {
    /// Name of the tool to call.
    pub name: String,
    /// Input arguments as JSON.
    pub input: serde_json::Value,
    /// Optional tool call ID. If not provided, a deterministic ID is generated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

/// A deterministic LLM double with pre-scripted responses.
///
/// `ScriptedLlm` implements the real [`Llm`] trait and exercises the full
/// runtime pipeline. Only the provider API call is replaced — everything else
/// (session loop, checkpoints, parking, event mapping) runs exactly as it would
/// with a real provider.
///
/// This is explicitly **NOT a mock**. It:
/// - Implements the full `Llm` trait contract
/// - Returns complete `LlmResponse` objects with proper `Content` and `Part` types
/// - Supports tool calls (function calls) in responses
/// - Advances through turns deterministically (FIFO order)
/// - Is thread-safe (`Send + Sync` via `AtomicUsize`)
///
/// # Panics
///
/// If more turns are requested than were scripted, the LLM returns an empty
/// response with `turn_complete = true` rather than panicking.
pub struct ScriptedLlm {
    /// Model name identifier.
    name: String,
    /// Pre-scripted turns in FIFO order.
    turns: Vec<ScriptedTurn>,
    /// Current turn index (atomic for thread safety).
    current_turn: AtomicUsize,
}

impl ScriptedLlm {
    /// Create a new `ScriptedLlm` with the given name and pre-scripted turns.
    ///
    /// Turns are consumed in FIFO order — each call to `generate_content`
    /// advances to the next turn.
    pub fn new(name: impl Into<String>, turns: Vec<ScriptedTurn>) -> Self {
        Self { name: name.into(), turns, current_turn: AtomicUsize::new(0) }
    }

    /// Returns the number of turns that have been consumed so far.
    pub fn turns_consumed(&self) -> usize {
        self.current_turn.load(Ordering::Relaxed)
    }

    /// Returns the total number of scripted turns.
    pub fn total_turns(&self) -> usize {
        self.turns.len()
    }

    /// Build an `LlmResponse` from a `ScriptedTurn`.
    fn build_response(turn: &ScriptedTurn, turn_index: usize) -> LlmResponse {
        use adk_core::FinishReason;
        use adk_core::types::Part;

        let mut parts = Vec::new();

        // Add text part if present.
        if let Some(text) = &turn.text {
            parts.push(Part::Text { text: text.clone() });
        }

        // Add function call parts.
        for (i, tool_call) in turn.tool_calls.iter().enumerate() {
            let id =
                tool_call.id.clone().unwrap_or_else(|| format!("scripted_tc_{turn_index}_{i}"));
            parts.push(Part::FunctionCall {
                name: tool_call.name.clone(),
                args: tool_call.input.clone(),
                id: Some(id),
                thought_signature: None,
            });
        }

        let content = if parts.is_empty() {
            None
        } else {
            Some(Content { role: "model".to_string(), parts })
        };

        LlmResponse {
            content,
            usage_metadata: None,
            finish_reason: Some(FinishReason::Stop),
            citation_metadata: None,
            partial: false,
            turn_complete: true,
            interrupted: false,
            error_code: None,
            error_message: None,
            provider_metadata: None,
            interaction_id: None,
            model: None,
            provider: None,
        }
    }
}

impl std::fmt::Debug for ScriptedLlm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScriptedLlm")
            .field("name", &self.name)
            .field("turns", &self.turns.len())
            .field("current_turn", &self.current_turn.load(Ordering::Relaxed))
            .finish()
    }
}

#[async_trait]
impl Llm for ScriptedLlm {
    fn name(&self) -> &str {
        &self.name
    }

    async fn generate_content(
        &self,
        _request: LlmRequest,
        _stream: bool,
    ) -> AdkResult<LlmResponseStream> {
        let turn_index = self.current_turn.fetch_add(1, Ordering::Relaxed);

        let response = if turn_index < self.turns.len() {
            Self::build_response(&self.turns[turn_index], turn_index)
        } else {
            // Beyond scripted turns — return empty complete response.
            LlmResponse {
                content: Some(Content {
                    role: "model".to_string(),
                    parts: vec![adk_core::types::Part::Text {
                        text: "[ScriptedLlm: no more scripted turns]".to_string(),
                    }],
                }),
                usage_metadata: None,
                finish_reason: Some(adk_core::FinishReason::Stop),
                citation_metadata: None,
                partial: false,
                turn_complete: true,
                interrupted: false,
                error_code: None,
                error_message: None,
                provider_metadata: None,
                interaction_id: None,
                model: None,
                provider: None,
            }
        };

        let response_stream = stream! {
            yield Ok(response);
        };

        Ok(Box::pin(response_stream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use serde_json::json;

    #[tokio::test]
    async fn test_scripted_llm_returns_text() {
        let turns =
            vec![ScriptedTurn { text: Some("Hello, world!".to_string()), tool_calls: vec![] }];
        let llm = ScriptedLlm::new("test-model", turns);

        assert_eq!(llm.name(), "test-model");

        let request = LlmRequest::new("test-model", vec![]);
        let mut stream = llm.generate_content(request, false).await.unwrap();

        let response = stream.next().await.unwrap().unwrap();
        assert!(response.turn_complete);
        assert!(!response.partial);

        let content = response.content.unwrap();
        assert_eq!(content.role, "model");
        assert_eq!(content.parts.len(), 1);
        match &content.parts[0] {
            adk_core::types::Part::Text { text } => {
                assert_eq!(text, "Hello, world!");
            }
            other => panic!("expected Text part, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_scripted_llm_returns_tool_calls() {
        let turns = vec![ScriptedTurn {
            text: None,
            tool_calls: vec![ScriptedToolCall {
                name: "web_search".to_string(),
                input: json!({"query": "rust async"}),
                id: Some("tc_001".to_string()),
            }],
        }];
        let llm = ScriptedLlm::new("tool-model", turns);

        let request = LlmRequest::new("tool-model", vec![]);
        let mut stream = llm.generate_content(request, false).await.unwrap();

        let response = stream.next().await.unwrap().unwrap();
        let content = response.content.unwrap();
        assert_eq!(content.parts.len(), 1);
        match &content.parts[0] {
            adk_core::types::Part::FunctionCall { name, args, id, .. } => {
                assert_eq!(name, "web_search");
                assert_eq!(args, &json!({"query": "rust async"}));
                assert_eq!(id, &Some("tc_001".to_string()));
            }
            other => panic!("expected FunctionCall part, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_scripted_llm_advances_through_turns() {
        let turns = vec![
            ScriptedTurn { text: Some("First".to_string()), tool_calls: vec![] },
            ScriptedTurn { text: Some("Second".to_string()), tool_calls: vec![] },
            ScriptedTurn { text: Some("Third".to_string()), tool_calls: vec![] },
        ];
        let llm = ScriptedLlm::new("multi-turn", turns);

        for (i, expected) in ["First", "Second", "Third"].iter().enumerate() {
            let request = LlmRequest::new("multi-turn", vec![]);
            let mut stream = llm.generate_content(request, false).await.unwrap();
            let response = stream.next().await.unwrap().unwrap();
            let content = response.content.unwrap();
            match &content.parts[0] {
                adk_core::types::Part::Text { text } => {
                    assert_eq!(text, *expected);
                }
                other => panic!("turn {i}: expected Text, got: {other:?}"),
            }
        }

        assert_eq!(llm.turns_consumed(), 3);
    }

    #[tokio::test]
    async fn test_scripted_llm_handles_exhaustion() {
        let turns = vec![ScriptedTurn { text: Some("Only one".to_string()), tool_calls: vec![] }];
        let llm = ScriptedLlm::new("exhausted", turns);

        // Consume the only turn.
        let request = LlmRequest::new("exhausted", vec![]);
        let mut stream = llm.generate_content(request, false).await.unwrap();
        let _ = stream.next().await.unwrap().unwrap();

        // Next call should return a fallback.
        let request = LlmRequest::new("exhausted", vec![]);
        let mut stream = llm.generate_content(request, false).await.unwrap();
        let response = stream.next().await.unwrap().unwrap();
        assert!(response.turn_complete);
        let content = response.content.unwrap();
        match &content.parts[0] {
            adk_core::types::Part::Text { text } => {
                assert!(text.contains("no more scripted turns"));
            }
            other => panic!("expected fallback Text, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_scripted_llm_mixed_text_and_tool_calls() {
        let turns = vec![ScriptedTurn {
            text: Some("Let me search for that.".to_string()),
            tool_calls: vec![ScriptedToolCall {
                name: "web_search".to_string(),
                input: json!({"query": "ADK Rust"}),
                id: Some("tc_mixed".to_string()),
            }],
        }];
        let llm = ScriptedLlm::new("mixed", turns);

        let request = LlmRequest::new("mixed", vec![]);
        let mut stream = llm.generate_content(request, false).await.unwrap();
        let response = stream.next().await.unwrap().unwrap();
        let content = response.content.unwrap();

        assert_eq!(content.parts.len(), 2);
        assert!(matches!(&content.parts[0], adk_core::types::Part::Text { .. }));
        assert!(matches!(&content.parts[1], adk_core::types::Part::FunctionCall { .. }));
    }

    #[tokio::test]
    async fn test_scripted_turn_serialization_roundtrip() {
        let turn = ScriptedTurn {
            text: Some("Hello".to_string()),
            tool_calls: vec![ScriptedToolCall {
                name: "search".to_string(),
                input: json!({"q": "test"}),
                id: Some("id_1".to_string()),
            }],
        };

        let json = serde_json::to_string(&turn).unwrap();
        let deserialized: ScriptedTurn = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.text, turn.text);
        assert_eq!(deserialized.tool_calls.len(), 1);
        assert_eq!(deserialized.tool_calls[0].name, "search");
        assert_eq!(deserialized.tool_calls[0].id, Some("id_1".to_string()));
    }

    #[tokio::test]
    async fn test_auto_generated_tool_call_ids() {
        let turns = vec![ScriptedTurn {
            text: None,
            tool_calls: vec![
                ScriptedToolCall {
                    name: "tool_a".to_string(),
                    input: json!({}),
                    id: None, // auto-generate
                },
                ScriptedToolCall {
                    name: "tool_b".to_string(),
                    input: json!({}),
                    id: None, // auto-generate
                },
            ],
        }];
        let llm = ScriptedLlm::new("auto-id", turns);

        let request = LlmRequest::new("auto-id", vec![]);
        let mut stream = llm.generate_content(request, false).await.unwrap();
        let response = stream.next().await.unwrap().unwrap();
        let content = response.content.unwrap();

        // Both should have deterministic IDs based on turn and index.
        match &content.parts[0] {
            adk_core::types::Part::FunctionCall { id, .. } => {
                assert_eq!(id, &Some("scripted_tc_0_0".to_string()));
            }
            other => panic!("expected FunctionCall, got: {other:?}"),
        }
        match &content.parts[1] {
            adk_core::types::Part::FunctionCall { id, .. } => {
                assert_eq!(id, &Some("scripted_tc_0_1".to_string()));
            }
            other => panic!("expected FunctionCall, got: {other:?}"),
        }
    }
}
