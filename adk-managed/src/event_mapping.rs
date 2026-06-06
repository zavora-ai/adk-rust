//! Provider-neutral event mapping from Runner output to SessionEvent.
//!
//! This module defines the [`RunnerOutput`] enum representing raw Runner events
//! and the [`map_runner_output`] function that maps them uniformly to
//! [`SessionEvent`] variants — guaranteeing identical type sequences regardless
//! of which LLM provider powered the turn.
//!
//! # Provider Parity
//!
//! The mapping is the key enforcement point for Requirement 5.1 and 5.5:
//! an identical `ManagedAgentDef` run against any provider MUST produce
//! byte-identical `SessionEvent` type sequences. The provider-specific
//! differences (tool call format, stop reasons, streaming deltas) are
//! normalized here before entering the session event stream.
//!
//! # Architecture
//!
//! ```text
//! Runner (provider-specific events)
//!   │
//!   ▼
//! RunnerOutput (normalized intermediate)
//!   │
//!   ▼
//! map_runner_output(output, seq) → SessionEvent (provider-neutral)
//! ```
//!
//! The session loop calls [`map_runner_output`] for each event emitted by the
//! Runner, producing a uniform stream that is then checkpointed and broadcast.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::types::{ContentBlock, SessionEvent, StopReason};

/// Represents a Runner output event that needs to be mapped to a SessionEvent.
///
/// This is the internal representation that normalizes provider-specific event
/// formats before they become provider-neutral session events. The session loop
/// constructs `RunnerOutput` values from the raw Runner event stream.
///
/// # Variants
///
/// - [`TextContent`](RunnerOutput::TextContent): LLM generated text → `agent.message`
/// - [`BuiltinToolCall`](RunnerOutput::BuiltinToolCall): Built-in tool invocation → `agent.tool_use`
/// - [`CustomToolCall`](RunnerOutput::CustomToolCall): Custom (client-executed) tool → `agent.custom_tool_use`
/// - [`McpToolCall`](RunnerOutput::McpToolCall): MCP tool invocation → `agent.mcp_tool_use`
/// - [`TurnComplete`](RunnerOutput::TurnComplete): Turn finished with a stop reason → `status.idle`
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum RunnerOutput {
    /// LLM generated text content.
    TextContent {
        /// The generated text.
        text: String,
    },

    /// LLM requested a built-in tool call (executes server-side in sandbox).
    BuiltinToolCall {
        /// Unique identifier for this tool invocation.
        tool_use_id: String,
        /// Name of the built-in tool.
        name: String,
        /// Tool input parameters as JSON.
        input: Value,
    },

    /// LLM requested a custom (client-executed) tool call.
    /// The session loop will park until the client delivers a result.
    CustomToolCall {
        /// Unique identifier for this custom tool invocation.
        custom_tool_use_id: String,
        /// Name of the custom tool.
        name: String,
        /// Tool input parameters as JSON.
        input: Value,
    },

    /// LLM requested an MCP tool call.
    McpToolCall {
        /// Unique identifier for this MCP tool invocation.
        tool_use_id: String,
        /// Name of the MCP tool.
        name: String,
        /// Tool input parameters as JSON.
        input: Value,
    },

    /// Turn completed with a stop reason.
    TurnComplete {
        /// Why the turn ended.
        stop_reason: StopReason,
    },
}

/// Tool classification used to determine which `RunnerOutput` variant to produce.
///
/// The session loop uses this to classify a tool call from the Runner before
/// constructing the appropriate `RunnerOutput` variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolKind {
    /// Built-in tool (bash, filesystem, web_search, etc.) — executes server-side.
    Builtin,
    /// Custom tool — client-executed, requires parking.
    Custom,
    /// MCP tool — executed via MCP server.
    Mcp,
}

/// Maps a [`RunnerOutput`] to a [`SessionEvent`] with the given sequence number.
///
/// This function is the provider parity enforcement point. Regardless of which
/// LLM provider produced the raw events, this mapping produces identical
/// `SessionEvent` variants with identical structure.
///
/// # Arguments
///
/// * `output` - The normalized Runner output event.
/// * `seq` - The monotonically increasing sequence number to assign.
///
/// # Returns
///
/// A `SessionEvent` ready for checkpointing and broadcast.
///
/// # Example
///
/// ```rust
/// use adk_managed::event_mapping::{RunnerOutput, map_runner_output};
/// use serde_json::json;
///
/// let output = RunnerOutput::TextContent {
///     text: "Hello, world!".to_string(),
/// };
/// let event = map_runner_output(output, 42);
/// // event is SessionEvent::Message { content: [...], seq: 42 }
/// ```
pub fn map_runner_output(output: RunnerOutput, seq: u64) -> SessionEvent {
    match output {
        RunnerOutput::TextContent { text } => SessionEvent::Message {
            content: vec![ContentBlock::Text { text }],
            seq,
        },
        RunnerOutput::BuiltinToolCall {
            tool_use_id,
            name,
            input,
        } => SessionEvent::ToolUse {
            tool_use_id,
            name,
            input,
            seq,
        },
        RunnerOutput::CustomToolCall {
            custom_tool_use_id,
            name,
            input,
        } => SessionEvent::CustomToolUse {
            custom_tool_use_id,
            name,
            input,
            seq,
        },
        RunnerOutput::McpToolCall {
            tool_use_id,
            name,
            input,
        } => SessionEvent::McpToolUse {
            tool_use_id,
            name,
            input,
            seq,
        },
        RunnerOutput::TurnComplete { stop_reason } => SessionEvent::StatusIdle {
            seq,
            stop_reason: Some(stop_reason),
            usage: None,
        },
    }
}

/// Returns `true` if the given `RunnerOutput` represents a custom tool call
/// that requires parking (waiting for client response).
///
/// This helper is used by the session loop to determine whether to park after
/// emitting the corresponding `SessionEvent`.
pub fn requires_parking(output: &RunnerOutput) -> bool {
    matches!(output, RunnerOutput::CustomToolCall { .. })
}

/// Extracts the custom tool use ID from a `RunnerOutput`, if it's a custom tool call.
///
/// Returns `None` for all other variants.
pub fn custom_tool_use_id(output: &RunnerOutput) -> Option<&str> {
    match output {
        RunnerOutput::CustomToolCall {
            custom_tool_use_id, ..
        } => Some(custom_tool_use_id),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_text_content_maps_to_agent_message() {
        let output = RunnerOutput::TextContent {
            text: "Hello from the model".to_string(),
        };
        let event = map_runner_output(output, 5);

        match event {
            SessionEvent::Message { content, seq } => {
                assert_eq!(seq, 5);
                assert_eq!(content.len(), 1);
                match &content[0] {
                    ContentBlock::Text { text } => {
                        assert_eq!(text, "Hello from the model");
                    }
                    _ => panic!("expected Text content block"),
                }
            }
            _ => panic!("expected Message event"),
        }
    }

    #[test]
    fn test_builtin_tool_call_maps_to_tool_use() {
        let output = RunnerOutput::BuiltinToolCall {
            tool_use_id: "tu_001".to_string(),
            name: "web_search".to_string(),
            input: json!({"query": "rust async"}),
        };
        let event = map_runner_output(output, 10);

        match event {
            SessionEvent::ToolUse {
                tool_use_id,
                name,
                input,
                seq,
            } => {
                assert_eq!(seq, 10);
                assert_eq!(tool_use_id, "tu_001");
                assert_eq!(name, "web_search");
                assert_eq!(input["query"], "rust async");
            }
            _ => panic!("expected ToolUse event"),
        }
    }

    #[test]
    fn test_custom_tool_call_maps_to_custom_tool_use() {
        let output = RunnerOutput::CustomToolCall {
            custom_tool_use_id: "ctu_002".to_string(),
            name: "deploy".to_string(),
            input: json!({"target": "production"}),
        };
        let event = map_runner_output(output, 20);

        match event {
            SessionEvent::CustomToolUse {
                custom_tool_use_id,
                name,
                input,
                seq,
            } => {
                assert_eq!(seq, 20);
                assert_eq!(custom_tool_use_id, "ctu_002");
                assert_eq!(name, "deploy");
                assert_eq!(input["target"], "production");
            }
            _ => panic!("expected CustomToolUse event"),
        }
    }

    #[test]
    fn test_mcp_tool_call_maps_to_mcp_tool_use() {
        let output = RunnerOutput::McpToolCall {
            tool_use_id: "mcp_003".to_string(),
            name: "file_read".to_string(),
            input: json!({"path": "/tmp/data.txt"}),
        };
        let event = map_runner_output(output, 30);

        match event {
            SessionEvent::McpToolUse {
                tool_use_id,
                name,
                input,
                seq,
            } => {
                assert_eq!(seq, 30);
                assert_eq!(tool_use_id, "mcp_003");
                assert_eq!(name, "file_read");
                assert_eq!(input["path"], "/tmp/data.txt");
            }
            _ => panic!("expected McpToolUse event"),
        }
    }

    #[test]
    fn test_turn_complete_maps_to_status_idle() {
        let output = RunnerOutput::TurnComplete {
            stop_reason: StopReason::EndTurn,
        };
        let event = map_runner_output(output, 40);

        match event {
            SessionEvent::StatusIdle { seq, stop_reason, .. } => {
                assert_eq!(seq, 40);
                assert!(matches!(stop_reason, Some(StopReason::EndTurn)));
            }
            _ => panic!("expected StatusIdle event"),
        }
    }

    #[test]
    fn test_turn_complete_requires_action() {
        let output = RunnerOutput::TurnComplete {
            stop_reason: StopReason::RequiresAction {
                event_ids: vec!["evt_1".to_string(), "evt_2".to_string()],
            },
        };
        let event = map_runner_output(output, 50);

        match event {
            SessionEvent::StatusIdle { seq, stop_reason, .. } => {
                assert_eq!(seq, 50);
                match stop_reason {
                    Some(StopReason::RequiresAction { event_ids }) => {
                        assert_eq!(event_ids, vec!["evt_1", "evt_2"]);
                    }
                    _ => panic!("expected RequiresAction stop reason"),
                }
            }
            _ => panic!("expected StatusIdle event"),
        }
    }

    #[test]
    fn test_turn_complete_max_tokens() {
        let output = RunnerOutput::TurnComplete {
            stop_reason: StopReason::MaxTokens,
        };
        let event = map_runner_output(output, 60);

        match event {
            SessionEvent::StatusIdle { seq, stop_reason, .. } => {
                assert_eq!(seq, 60);
                assert!(matches!(stop_reason, Some(StopReason::MaxTokens)));
            }
            _ => panic!("expected StatusIdle event"),
        }
    }

    #[test]
    fn test_requires_parking_custom_tool() {
        let output = RunnerOutput::CustomToolCall {
            custom_tool_use_id: "ctu_park".to_string(),
            name: "deploy".to_string(),
            input: json!({}),
        };
        assert!(requires_parking(&output));
    }

    #[test]
    fn test_requires_parking_other_variants() {
        let text = RunnerOutput::TextContent {
            text: "hi".to_string(),
        };
        let builtin = RunnerOutput::BuiltinToolCall {
            tool_use_id: "tu".to_string(),
            name: "search".to_string(),
            input: json!({}),
        };
        let mcp = RunnerOutput::McpToolCall {
            tool_use_id: "mcp".to_string(),
            name: "read".to_string(),
            input: json!({}),
        };
        let complete = RunnerOutput::TurnComplete {
            stop_reason: StopReason::EndTurn,
        };

        assert!(!requires_parking(&text));
        assert!(!requires_parking(&builtin));
        assert!(!requires_parking(&mcp));
        assert!(!requires_parking(&complete));
    }

    #[test]
    fn test_custom_tool_use_id_extraction() {
        let output = RunnerOutput::CustomToolCall {
            custom_tool_use_id: "ctu_extract".to_string(),
            name: "deploy".to_string(),
            input: json!({}),
        };
        assert_eq!(custom_tool_use_id(&output), Some("ctu_extract"));

        let text = RunnerOutput::TextContent {
            text: "hi".to_string(),
        };
        assert_eq!(custom_tool_use_id(&text), None);
    }

    #[test]
    fn test_provider_parity_identical_inputs_produce_identical_outputs() {
        // Simulate the same tool call from different providers — all should
        // map to the exact same SessionEvent.
        let from_gemini = RunnerOutput::BuiltinToolCall {
            tool_use_id: "tu_parity".to_string(),
            name: "web_search".to_string(),
            input: json!({"query": "weather"}),
        };
        let from_openai = RunnerOutput::BuiltinToolCall {
            tool_use_id: "tu_parity".to_string(),
            name: "web_search".to_string(),
            input: json!({"query": "weather"}),
        };
        let from_anthropic = RunnerOutput::BuiltinToolCall {
            tool_use_id: "tu_parity".to_string(),
            name: "web_search".to_string(),
            input: json!({"query": "weather"}),
        };

        let ev1 = map_runner_output(from_gemini, 0);
        let ev2 = map_runner_output(from_openai, 0);
        let ev3 = map_runner_output(from_anthropic, 0);

        // Byte-identical JSON serialization.
        let json1 = serde_json::to_string(&ev1).unwrap();
        let json2 = serde_json::to_string(&ev2).unwrap();
        let json3 = serde_json::to_string(&ev3).unwrap();

        assert_eq!(json1, json2);
        assert_eq!(json2, json3);
    }

    #[test]
    fn test_mapping_preserves_seq_exactly() {
        // Verify that the seq value is passed through without modification.
        let outputs = vec![
            RunnerOutput::TextContent {
                text: "a".to_string(),
            },
            RunnerOutput::BuiltinToolCall {
                tool_use_id: "t".to_string(),
                name: "n".to_string(),
                input: json!({}),
            },
            RunnerOutput::CustomToolCall {
                custom_tool_use_id: "c".to_string(),
                name: "n".to_string(),
                input: json!({}),
            },
            RunnerOutput::McpToolCall {
                tool_use_id: "m".to_string(),
                name: "n".to_string(),
                input: json!({}),
            },
            RunnerOutput::TurnComplete {
                stop_reason: StopReason::EndTurn,
            },
        ];

        for (i, output) in outputs.into_iter().enumerate() {
            let seq = (i as u64) * 100 + 7;
            let event = map_runner_output(output, seq);

            let event_seq = match &event {
                SessionEvent::Message { seq, .. } => *seq,
                SessionEvent::ToolUse { seq, .. } => *seq,
                SessionEvent::CustomToolUse { seq, .. } => *seq,
                SessionEvent::McpToolUse { seq, .. } => *seq,
                SessionEvent::StatusIdle { seq, .. } => *seq,
                SessionEvent::StatusRunning { seq } => *seq,
                SessionEvent::Error { seq, .. } => *seq,
            };
            assert_eq!(event_seq, seq);
        }
    }
}
