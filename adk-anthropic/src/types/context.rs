use serde::{Deserialize, Serialize};

/// Server-side context management configuration.
///
/// Contains an array of editing strategies applied before the prompt reaches Claude.
/// Requires beta header `context-management-2025-06-27`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextManagement {
    /// Editing strategies to apply. When combining strategies,
    /// `clear_thinking_20251015` must come first.
    pub edits: Vec<ContextEdit>,
}

/// A single context editing strategy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContextEdit {
    /// Clear old tool use/result pairs when context grows too large.
    #[serde(rename = "clear_tool_uses_20250919")]
    ClearToolUses {
        /// When to activate (defaults to 100k input tokens).
        #[serde(skip_serializing_if = "Option::is_none")]
        trigger: Option<TokenThreshold>,
        /// How many recent tool use/result pairs to keep (defaults to 3).
        #[serde(skip_serializing_if = "Option::is_none")]
        keep: Option<TokenThreshold>,
        /// Minimum tokens to clear each activation.
        #[serde(skip_serializing_if = "Option::is_none")]
        clear_at_least: Option<TokenThreshold>,
        /// Tool names whose results should never be cleared.
        #[serde(skip_serializing_if = "Option::is_none")]
        exclude_tools: Option<Vec<String>>,
        /// Also clear tool call parameters (not just results).
        #[serde(skip_serializing_if = "Option::is_none")]
        clear_tool_inputs: Option<bool>,
    },
    /// Clear thinking blocks from earlier turns.
    #[serde(rename = "clear_thinking_20251015")]
    ClearThinking {
        /// How many recent thinking turns to keep. Defaults to 1.
        /// Use `"all"` to keep everything (maximises cache hits).
        #[serde(skip_serializing_if = "Option::is_none")]
        keep: Option<ThinkingKeep>,
    },
}

/// Threshold specification used in trigger / keep / clear_at_least.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenThreshold {
    /// `"input_tokens"` or `"tool_uses"`.
    #[serde(rename = "type")]
    pub threshold_type: String,
    /// The numeric value.
    pub value: u32,
}

/// How many thinking turns to keep.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ThinkingKeep {
    /// Keep all thinking blocks.
    All(String),
    /// Keep the last N assistant turns with thinking.
    Turns(TokenThreshold),
}

/// Response field showing which edits were applied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextManagementResponse {
    /// Edits that were applied to this request.
    #[serde(default)]
    pub applied_edits: Vec<AppliedEdit>,
}

/// A single applied edit in the response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AppliedEdit {
    #[serde(rename = "clear_tool_uses_20250919")]
    ClearToolUses { cleared_tool_uses: u32, cleared_input_tokens: u32 },
    #[serde(rename = "clear_thinking_20251015")]
    ClearThinking { cleared_thinking_turns: u32, cleared_input_tokens: u32 },
}

// ── Convenience constructors ──────────────────────────────────

impl ContextManagement {
    /// Create with a single tool-use clearing strategy (defaults).
    pub fn clear_tool_uses() -> Self {
        Self {
            edits: vec![ContextEdit::ClearToolUses {
                trigger: None,
                keep: None,
                clear_at_least: None,
                exclude_tools: None,
                clear_tool_inputs: None,
            }],
        }
    }

    /// Create with a single thinking-block clearing strategy (defaults).
    pub fn clear_thinking() -> Self {
        Self { edits: vec![ContextEdit::ClearThinking { keep: None }] }
    }
}

impl TokenThreshold {
    pub fn input_tokens(value: u32) -> Self {
        Self { threshold_type: "input_tokens".to_string(), value }
    }
    pub fn tool_uses(value: u32) -> Self {
        Self { threshold_type: "tool_uses".to_string(), value }
    }
}

impl ThinkingKeep {
    pub fn all() -> Self {
        Self::All("all".to_string())
    }
    pub fn turns(n: u32) -> Self {
        Self::Turns(TokenThreshold { threshold_type: "thinking_turns".to_string(), value: n })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn clear_tool_uses_minimal() {
        let cm = ContextManagement::clear_tool_uses();
        let j = serde_json::to_value(&cm).unwrap();
        assert_eq!(j, json!({"edits": [{"type": "clear_tool_uses_20250919"}]}));
    }

    #[test]
    fn clear_tool_uses_full() {
        let cm = ContextManagement {
            edits: vec![ContextEdit::ClearToolUses {
                trigger: Some(TokenThreshold::input_tokens(30000)),
                keep: Some(TokenThreshold::tool_uses(3)),
                clear_at_least: Some(TokenThreshold::input_tokens(5000)),
                exclude_tools: Some(vec!["web_search".to_string()]),
                clear_tool_inputs: Some(true),
            }],
        };
        let j = serde_json::to_value(&cm).unwrap();
        assert_eq!(j["edits"][0]["trigger"]["value"], 30000);
        assert_eq!(j["edits"][0]["exclude_tools"][0], "web_search");
    }

    #[test]
    fn clear_thinking_with_turns() {
        let cm = ContextManagement {
            edits: vec![ContextEdit::ClearThinking { keep: Some(ThinkingKeep::turns(2)) }],
        };
        let j = serde_json::to_value(&cm).unwrap();
        assert_eq!(
            j,
            json!({"edits": [{"type": "clear_thinking_20251015", "keep": {"type": "thinking_turns", "value": 2}}]})
        );
    }

    #[test]
    fn clear_thinking_keep_all() {
        let cm = ContextManagement {
            edits: vec![ContextEdit::ClearThinking { keep: Some(ThinkingKeep::all()) }],
        };
        let j = serde_json::to_value(&cm).unwrap();
        assert_eq!(j, json!({"edits": [{"type": "clear_thinking_20251015", "keep": "all"}]}));
    }

    #[test]
    fn applied_edit_roundtrip() {
        let j = json!({"type": "clear_tool_uses_20250919", "cleared_tool_uses": 8, "cleared_input_tokens": 50000});
        let edit: AppliedEdit = serde_json::from_value(j.clone()).unwrap();
        assert_eq!(serde_json::to_value(&edit).unwrap(), j);
    }

    #[test]
    fn combined_strategies() {
        let cm = ContextManagement {
            edits: vec![
                ContextEdit::ClearThinking { keep: Some(ThinkingKeep::turns(2)) },
                ContextEdit::ClearToolUses {
                    trigger: Some(TokenThreshold::input_tokens(50000)),
                    keep: Some(TokenThreshold::tool_uses(5)),
                    clear_at_least: None,
                    exclude_tools: None,
                    clear_tool_inputs: None,
                },
            ],
        };
        let j = serde_json::to_value(&cm).unwrap();
        assert_eq!(j["edits"].as_array().unwrap().len(), 2);
        assert_eq!(j["edits"][0]["type"], "clear_thinking_20251015");
        assert_eq!(j["edits"][1]["type"], "clear_tool_uses_20250919");
    }
}

/// Metadata about a compaction event (emitted during SSE streaming).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionMetadata {
    /// Number of tokens that were compacted.
    pub compacted_token_count: u32,
    /// Number of tokens in the summary.
    pub summary_token_count: u32,
    /// Remaining tokens in the context window.
    pub context_window_remaining: u32,
}
