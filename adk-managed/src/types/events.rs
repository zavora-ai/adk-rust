//! Event types for the managed agent runtime.
//!
//! Defines [`UserEvent`] (client → agent) and [`SessionEvent`] (agent → client),
//! conforming to CANON §3.4 wire shapes. Both enums are `#[non_exhaustive]`
//! for forward-compatible additive evolution.

use serde::{Deserialize, Serialize};

use super::content::ContentBlock;

// ─── UserEvent ───────────────────────────────────────────────────────────────

/// Client-to-agent event. Discriminated by `type` field for wire serialization.
///
/// # Wire Shapes (CANON §3.4)
///
/// ```json
/// {"type": "user.message", "content": [{"type": "text", "text": "Hello"}]}
/// {"type": "user.interrupt"}
/// ```
///
/// # Example
///
/// ```rust
/// use adk_managed::types::{UserEvent, ContentBlock};
///
/// let event = UserEvent::Message {
///     content: vec![ContentBlock::Text { text: "Hello".to_string() }],
/// };
/// let json = serde_json::to_string(&event).unwrap();
/// assert!(json.contains(r#""type":"user.message""#));
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum UserEvent {
    /// Send a message turn.
    #[serde(rename = "user.message")]
    Message {
        /// The message content blocks.
        content: Vec<ContentBlock>,
    },

    /// Interrupt the current turn.
    #[serde(rename = "user.interrupt")]
    Interrupt {},

    /// Approve or deny a tool confirmation request.
    #[serde(rename = "user.tool_confirmation")]
    ToolConfirmation {
        /// The tool use ID being confirmed.
        tool_use_id: String,
        /// Whether to allow or deny.
        result: ConfirmationResult,
        /// Optional message explaining why the tool was denied.
        #[serde(skip_serializing_if = "Option::is_none")]
        deny_message: Option<String>,
    },

    /// Return results for a client-executed custom tool.
    #[serde(rename = "user.custom_tool_result")]
    CustomToolResult {
        /// The custom tool use ID this result corresponds to.
        custom_tool_use_id: String,
        /// The result content blocks.
        content: Vec<ContentBlock>,
    },

    /// Return results for a built-in tool (self-hosted only).
    /// In hosted topology, built-in tools execute server-side in the sandbox.
    #[serde(rename = "user.tool_result")]
    ToolResult {
        /// The tool use ID this result corresponds to.
        tool_use_id: String,
        /// The result content blocks.
        content: Vec<ContentBlock>,
    },

    /// Define success criteria for the session.
    #[serde(rename = "user.define_outcome")]
    DefineOutcome {
        /// The success criteria description.
        criteria: String,
    },
}

/// Result of a tool confirmation request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ConfirmationResult {
    /// Allow the tool to execute.
    Allow,
    /// Deny the tool execution.
    Deny,
}

// ─── SessionEvent ────────────────────────────────────────────────────────────

/// Agent-to-client event. Each carries a monotonic `seq` per session.
///
/// # Wire Shapes (CANON §3.4)
///
/// ```json
/// {"type": "agent.message", "content": [{"type": "text", "text": "Hi"}], "seq": 1}
/// {"type": "status.running", "seq": 2}
/// {"type": "status.idle", "seq": 3, "stop_reason": {"reason": "end_turn"}}
/// ```
///
/// # Example
///
/// ```rust
/// use adk_managed::types::{SessionEvent, ContentBlock};
///
/// let event = SessionEvent::Message {
///     content: vec![ContentBlock::Text { text: "Hello".to_string() }],
///     seq: 1,
/// };
/// let json = serde_json::to_string(&event).unwrap();
/// assert!(json.contains(r#""type":"agent.message""#));
/// assert!(json.contains(r#""seq":1"#));
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SessionEvent {
    /// Assistant message content.
    #[serde(rename = "agent.message")]
    Message {
        /// The message content blocks.
        content: Vec<ContentBlock>,
        /// Monotonically increasing sequence number.
        seq: u64,
    },

    /// Built-in tool invocation (executes server-side in sandbox).
    #[serde(rename = "agent.tool_use")]
    ToolUse {
        /// Unique identifier for this tool use.
        tool_use_id: String,
        /// Tool name.
        name: String,
        /// Tool input parameters.
        input: serde_json::Value,
        /// Monotonically increasing sequence number.
        seq: u64,
    },

    /// Custom tool invocation (client must execute and return result).
    /// The loop PARKS until `user.custom_tool_result` with matching ID arrives.
    #[serde(rename = "agent.custom_tool_use")]
    CustomToolUse {
        /// Unique identifier for this custom tool use.
        custom_tool_use_id: String,
        /// Tool name.
        name: String,
        /// Tool input parameters.
        input: serde_json::Value,
        /// Monotonically increasing sequence number.
        seq: u64,
    },

    /// MCP tool invocation.
    #[serde(rename = "agent.mcp_tool_use")]
    McpToolUse {
        /// Unique identifier for this MCP tool use.
        tool_use_id: String,
        /// Tool name.
        name: String,
        /// Tool input parameters.
        input: serde_json::Value,
        /// Monotonically increasing sequence number.
        seq: u64,
    },

    /// Session became active (processing a turn).
    #[serde(rename = "status.running")]
    StatusRunning {
        /// Monotonically increasing sequence number.
        seq: u64,
    },

    /// Turn complete; awaiting next event.
    /// Includes `stop_reason` to tell the caller WHY the turn ended,
    /// and `usage` reporting token consumption for billing/metering.
    #[serde(rename = "status.idle")]
    StatusIdle {
        /// Monotonically increasing sequence number.
        seq: u64,
        /// Why the turn ended. Enables the client to decide what to do next.
        stop_reason: Option<StopReason>,
        /// Token usage for this turn (input/output/total).
        /// Present when the LLM reports usage metadata; `None` on error turns.
        #[serde(skip_serializing_if = "Option::is_none")]
        usage: Option<crate::usage::UsageReport>,
    },

    /// Error during execution.
    #[serde(rename = "error")]
    Error {
        /// Error code identifier.
        code: String,
        /// Human-readable error message.
        message: String,
        /// Monotonically increasing sequence number.
        seq: u64,
    },
}

/// Why a turn ended. Included in `status.idle` events.
///
/// # Wire Shapes
///
/// ```json
/// {"reason": "end_turn"}
/// {"reason": "requires_action", "event_ids": ["evt_001", "evt_002"]}
/// {"reason": "max_tokens"}
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
#[non_exhaustive]
pub enum StopReason {
    /// The LLM naturally ended its turn (end_turn stop reason from provider).
    EndTurn,
    /// The agent emitted custom tool calls that require client action.
    /// The caller must send `user.custom_tool_result` for each listed event_id.
    RequiresAction {
        /// IDs of events requiring action.
        event_ids: Vec<String>,
    },
    /// The LLM hit its maximum token limit mid-generation.
    MaxTokens,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ─── UserEvent Tests ─────────────────────────────────────────────────────

    #[test]
    fn test_user_message_serialization() {
        let event = UserEvent::Message {
            content: vec![ContentBlock::Text {
                text: "Hello".to_string(),
            }],
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "user.message");
        assert_eq!(value["content"][0]["type"], "text");
        assert_eq!(value["content"][0]["text"], "Hello");
    }

    #[test]
    fn test_user_interrupt_serialization() {
        let event = UserEvent::Interrupt {};
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "user.interrupt");
    }

    #[test]
    fn test_user_tool_confirmation_serialization() {
        let event = UserEvent::ToolConfirmation {
            tool_use_id: "tu_123".to_string(),
            result: ConfirmationResult::Allow,
            deny_message: None,
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "user.tool_confirmation");
        assert_eq!(value["tool_use_id"], "tu_123");
        assert_eq!(value["result"], "allow");
        assert!(value.get("deny_message").is_none());
    }

    #[test]
    fn test_user_tool_confirmation_with_deny() {
        let event = UserEvent::ToolConfirmation {
            tool_use_id: "tu_456".to_string(),
            result: ConfirmationResult::Deny,
            deny_message: Some("Not authorized".to_string()),
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "user.tool_confirmation");
        assert_eq!(value["result"], "deny");
        assert_eq!(value["deny_message"], "Not authorized");
    }

    #[test]
    fn test_user_custom_tool_result_serialization() {
        let event = UserEvent::CustomToolResult {
            custom_tool_use_id: "ctu_789".to_string(),
            content: vec![ContentBlock::Text {
                text: "result data".to_string(),
            }],
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "user.custom_tool_result");
        assert_eq!(value["custom_tool_use_id"], "ctu_789");
    }

    #[test]
    fn test_user_tool_result_serialization() {
        let event = UserEvent::ToolResult {
            tool_use_id: "tu_self_001".to_string(),
            content: vec![ContentBlock::Text {
                text: "tool output".to_string(),
            }],
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "user.tool_result");
        assert_eq!(value["tool_use_id"], "tu_self_001");
    }

    #[test]
    fn test_user_define_outcome_serialization() {
        let event = UserEvent::DefineOutcome {
            criteria: "Task is completed successfully".to_string(),
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "user.define_outcome");
        assert_eq!(value["criteria"], "Task is completed successfully");
    }

    #[test]
    fn test_user_event_unknown_type_rejected() {
        let json_str = r#"{"type": "user.unknown", "data": "something"}"#;
        let result: Result<UserEvent, _> = serde_json::from_str(json_str);
        assert!(result.is_err(), "Unknown type should be rejected");
    }

    #[test]
    fn test_user_event_round_trip() {
        let event = UserEvent::Message {
            content: vec![ContentBlock::Text {
                text: "Round trip".to_string(),
            }],
        };
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: UserEvent = serde_json::from_str(&json).unwrap();
        match deserialized {
            UserEvent::Message { content } => {
                assert_eq!(content.len(), 1);
                match &content[0] {
                    ContentBlock::Text { text } => assert_eq!(text, "Round trip"),
                    _ => panic!("Expected Text content block"),
                }
            }
            _ => panic!("Expected Message variant"),
        }
    }

    // ─── SessionEvent Tests ──────────────────────────────────────────────────

    #[test]
    fn test_session_message_serialization() {
        let event = SessionEvent::Message {
            content: vec![ContentBlock::Text {
                text: "Hi there".to_string(),
            }],
            seq: 1,
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "agent.message");
        assert_eq!(value["seq"], 1);
        assert_eq!(value["content"][0]["text"], "Hi there");
    }

    #[test]
    fn test_session_tool_use_serialization() {
        let event = SessionEvent::ToolUse {
            tool_use_id: "tu_001".to_string(),
            name: "search".to_string(),
            input: json!({"query": "rust async"}),
            seq: 2,
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "agent.tool_use");
        assert_eq!(value["tool_use_id"], "tu_001");
        assert_eq!(value["name"], "search");
        assert_eq!(value["input"]["query"], "rust async");
        assert_eq!(value["seq"], 2);
    }

    #[test]
    fn test_session_custom_tool_use_serialization() {
        let event = SessionEvent::CustomToolUse {
            custom_tool_use_id: "ctu_001".to_string(),
            name: "deploy".to_string(),
            input: json!({"target": "production"}),
            seq: 3,
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "agent.custom_tool_use");
        assert_eq!(value["custom_tool_use_id"], "ctu_001");
        assert_eq!(value["name"], "deploy");
        assert_eq!(value["input"]["target"], "production");
        assert_eq!(value["seq"], 3);
    }

    #[test]
    fn test_session_mcp_tool_use_serialization() {
        let event = SessionEvent::McpToolUse {
            tool_use_id: "mcp_001".to_string(),
            name: "file_read".to_string(),
            input: json!({"path": "/tmp/data.txt"}),
            seq: 4,
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "agent.mcp_tool_use");
        assert_eq!(value["tool_use_id"], "mcp_001");
        assert_eq!(value["name"], "file_read");
        assert_eq!(value["input"]["path"], "/tmp/data.txt");
        assert_eq!(value["seq"], 4);
    }

    #[test]
    fn test_session_status_running_serialization() {
        let event = SessionEvent::StatusRunning { seq: 5 };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "status.running");
        assert_eq!(value["seq"], 5);
    }

    #[test]
    fn test_session_status_idle_serialization_no_stop_reason() {
        let event = SessionEvent::StatusIdle {
            seq: 6,
            stop_reason: None,
            usage: None,
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "status.idle");
        assert_eq!(value["seq"], 6);
        assert_eq!(value["stop_reason"], json!(null));
    }

    #[test]
    fn test_session_status_idle_with_end_turn() {
        let event = SessionEvent::StatusIdle {
            seq: 7,
            stop_reason: Some(StopReason::EndTurn),
            usage: None,
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "status.idle");
        assert_eq!(value["seq"], 7);
        assert_eq!(value["stop_reason"]["reason"], "end_turn");
    }

    #[test]
    fn test_session_status_idle_with_requires_action() {
        let event = SessionEvent::StatusIdle {
            seq: 8,
            stop_reason: Some(StopReason::RequiresAction {
                event_ids: vec!["evt_001".to_string(), "evt_002".to_string()],
            }),
            usage: None,
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "status.idle");
        assert_eq!(value["seq"], 8);
        assert_eq!(value["stop_reason"]["reason"], "requires_action");
        assert_eq!(
            value["stop_reason"]["event_ids"],
            json!(["evt_001", "evt_002"])
        );
    }

    #[test]
    fn test_session_status_idle_with_max_tokens() {
        let event = SessionEvent::StatusIdle {
            seq: 9,
            stop_reason: Some(StopReason::MaxTokens),
            usage: None,
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "status.idle");
        assert_eq!(value["seq"], 9);
        assert_eq!(value["stop_reason"]["reason"], "max_tokens");
    }

    #[test]
    fn test_session_error_serialization() {
        let event = SessionEvent::Error {
            code: "provider_error".to_string(),
            message: "Model unavailable".to_string(),
            seq: 10,
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "error");
        assert_eq!(value["code"], "provider_error");
        assert_eq!(value["message"], "Model unavailable");
        assert_eq!(value["seq"], 10);
    }

    #[test]
    fn test_session_event_seq_strictly_increasing() {
        // Simulate a sequence of events with strictly increasing seq values
        let events = vec![
            SessionEvent::StatusRunning { seq: 0 },
            SessionEvent::Message {
                content: vec![ContentBlock::Text {
                    text: "Hello".to_string(),
                }],
                seq: 1,
            },
            SessionEvent::ToolUse {
                tool_use_id: "tu_1".to_string(),
                name: "search".to_string(),
                input: json!({}),
                seq: 2,
            },
            SessionEvent::CustomToolUse {
                custom_tool_use_id: "ctu_1".to_string(),
                name: "deploy".to_string(),
                input: json!({}),
                seq: 3,
            },
            SessionEvent::McpToolUse {
                tool_use_id: "mcp_1".to_string(),
                name: "read".to_string(),
                input: json!({}),
                seq: 4,
            },
            SessionEvent::StatusIdle {
                seq: 5,
                stop_reason: Some(StopReason::EndTurn),
                usage: None,
            },
        ];

        // Extract seq values and verify strict monotonic increase
        let seqs: Vec<u64> = events
            .iter()
            .map(|e| match e {
                SessionEvent::StatusRunning { seq }
                | SessionEvent::Message { seq, .. }
                | SessionEvent::ToolUse { seq, .. }
                | SessionEvent::CustomToolUse { seq, .. }
                | SessionEvent::McpToolUse { seq, .. }
                | SessionEvent::StatusIdle { seq, .. }
                | SessionEvent::Error { seq, .. } => *seq,
            })
            .collect();

        for window in seqs.windows(2) {
            assert!(
                window[1] > window[0],
                "seq must be strictly increasing: {} should be > {}",
                window[1],
                window[0]
            );
        }
    }

    #[test]
    fn test_session_event_round_trip() {
        let event = SessionEvent::CustomToolUse {
            custom_tool_use_id: "ctu_rt".to_string(),
            name: "execute".to_string(),
            input: json!({"command": "ls -la"}),
            seq: 42,
        };
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: SessionEvent = serde_json::from_str(&json).unwrap();
        match deserialized {
            SessionEvent::CustomToolUse {
                custom_tool_use_id,
                name,
                input,
                seq,
            } => {
                assert_eq!(custom_tool_use_id, "ctu_rt");
                assert_eq!(name, "execute");
                assert_eq!(input["command"], "ls -la");
                assert_eq!(seq, 42);
            }
            _ => panic!("Expected CustomToolUse variant"),
        }
    }

    // ─── StopReason Tests ────────────────────────────────────────────────────

    #[test]
    fn test_stop_reason_end_turn_serialization() {
        let reason = StopReason::EndTurn;
        let value = serde_json::to_value(&reason).unwrap();
        assert_eq!(value, json!({"reason": "end_turn"}));
    }

    #[test]
    fn test_stop_reason_requires_action_serialization() {
        let reason = StopReason::RequiresAction {
            event_ids: vec!["evt_a".to_string(), "evt_b".to_string()],
        };
        let value = serde_json::to_value(&reason).unwrap();
        assert_eq!(
            value,
            json!({"reason": "requires_action", "event_ids": ["evt_a", "evt_b"]})
        );
    }

    #[test]
    fn test_stop_reason_max_tokens_serialization() {
        let reason = StopReason::MaxTokens;
        let value = serde_json::to_value(&reason).unwrap();
        assert_eq!(value, json!({"reason": "max_tokens"}));
    }

    #[test]
    fn test_stop_reason_round_trip() {
        let reasons = vec![
            StopReason::EndTurn,
            StopReason::RequiresAction {
                event_ids: vec!["id1".to_string()],
            },
            StopReason::MaxTokens,
        ];
        for reason in reasons {
            let json = serde_json::to_string(&reason).unwrap();
            let deserialized: StopReason = serde_json::from_str(&json).unwrap();
            let re_serialized = serde_json::to_string(&deserialized).unwrap();
            assert_eq!(json, re_serialized);
        }
    }
}
