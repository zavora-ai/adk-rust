//! Testing utilities for the managed agent runtime.
//!
//! [`ScriptedLlm`] is the scripted model double from `adk-model`: it answers each model call
//! with the next scripted reply and records every request. It implements the real
//! [`Llm`](adk_core::Llm) trait, so the full runtime pipeline (durable runs, checkpoints, replay, event mapping) runs
//! exactly as it does with a real provider; only the provider API call is replaced.
//!
//! [`ScriptedTurn`] is the serializable form of one reply, used by the golden fixtures, and
//! converts into a [`ScriptedResponse`].
//!
//! # Architecture
//!
//! ```text
//! ScriptedLlm (one scripted reply per call, requests recorded)
//!   │
//!   ▼
//! Full runtime pipeline (SessionLoop, CheckpointManager, durable runs, etc.)
//!   │
//!   ▼
//! SessionEvent stream (byte-identical assertions possible)
//! ```
//!
//! # Usage
//!
//! ```rust
//! use adk_managed::testing::{ScriptedLlm, ScriptedToolCall, ScriptedTurn};
//! use serde_json::json;
//!
//! let turns = vec![
//!     ScriptedTurn {
//!         text: None,
//!         tool_calls: vec![ScriptedToolCall {
//!             name: "web_search".to_string(),
//!             input: json!({"query": "rust async"}),
//!             id: Some("tc_001".to_string()),
//!         }],
//!     },
//!     ScriptedTurn { text: Some("Here is what I found.".to_string()), tool_calls: vec![] },
//! ];
//!
//! let llm = ScriptedLlm::new("scripted-model", turns);
//! assert_eq!(llm.remaining(), 2);
//! // Use llm in place of any Arc<dyn Llm> in the runtime pipeline, then assert on
//! // `llm.requests()` to see what the runtime sent the model on each call.
//! ```

use adk_core::{Content, FinishReason, LlmResponse, Part};
pub use adk_model::{SCRIPT_EXHAUSTED_CODE, ScriptedLlm, ScriptedResponse};
use serde::{Deserialize, Serialize};

/// One scripted model reply, in the serializable form the fixture files use.
///
/// Each turn represents one complete LLM response. It can contain text content,
/// tool calls, or both — mirroring real LLM behavior where a response may
/// include reasoning text followed by tool invocations. It converts into a
/// [`ScriptedResponse`], so a `Vec<ScriptedTurn>` scripts a [`ScriptedLlm`].
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
    /// Optional tool call ID. Without one, [`ScriptedLlm`] assigns
    /// `scripted_tc_{call}_{index}`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

impl From<ScriptedTurn> for ScriptedResponse {
    /// A final model response: the turn's text first, then its tool calls in order. A turn with
    /// neither is a response with no content.
    fn from(turn: ScriptedTurn) -> Self {
        let parts: Vec<Part> = turn
            .text
            .map(|text| Part::Text { text })
            .into_iter()
            .chain(turn.tool_calls.into_iter().map(|call| Part::FunctionCall {
                name: call.name,
                args: call.input,
                id: call.id,
                thought_signature: None,
            }))
            .collect();
        let mut response = LlmResponse::new(Content { role: "model".to_string(), parts });
        if response.content.as_ref().is_some_and(|content| content.parts.is_empty()) {
            response.content = None;
        }
        response.finish_reason = Some(FinishReason::Stop);
        response.turn_complete = true;
        ScriptedResponse::response(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::{Llm, LlmRequest};
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

        assert_eq!(llm.requests().len(), 3);
        assert_eq!(llm.remaining(), 0);
    }

    #[tokio::test]
    async fn test_scripted_llm_handles_exhaustion() {
        let turns = vec![ScriptedTurn { text: Some("Only one".to_string()), tool_calls: vec![] }];
        let llm = ScriptedLlm::new("exhausted", turns);

        // Consume the only turn.
        let request = LlmRequest::new("exhausted", vec![]);
        let mut stream = llm.generate_content(request, false).await.unwrap();
        let _ = stream.next().await.unwrap().unwrap();

        // A call past the script fails instead of inventing a reply.
        let request = LlmRequest::new("exhausted", vec![]);
        let Err(err) = llm.generate_content(request, false).await else {
            panic!("the script is used up");
        };
        assert_eq!(err.code, SCRIPT_EXHAUSTED_CODE);
    }

    #[tokio::test]
    async fn test_scripted_llm_records_requests() {
        let llm = ScriptedLlm::new(
            "recording",
            vec![
                ScriptedTurn { text: Some("one".to_string()), tool_calls: vec![] },
                ScriptedTurn { text: Some("two".to_string()), tool_calls: vec![] },
            ],
        );
        for prompt in ["first", "second"] {
            let request =
                LlmRequest::new("recording", vec![Content::new("user").with_text(prompt)]);
            let _ = llm.generate_content(request, false).await.unwrap().next().await;
        }
        let prompts: Vec<_> = llm
            .requests()
            .iter()
            .map(|request| request.contents[0].parts[0].text().unwrap_or_default().to_string())
            .collect();
        assert_eq!(prompts, ["first", "second"]);
    }

    #[tokio::test]
    async fn test_empty_turn_has_no_content() {
        let llm = ScriptedLlm::new("empty", vec![ScriptedTurn { text: None, tool_calls: vec![] }]);
        let request = LlmRequest::new("empty", vec![]);
        let response = llm.generate_content(request, false).await.unwrap().next().await;
        let response = response.unwrap().unwrap();
        assert!(response.content.is_none());
        assert!(response.turn_complete);
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
