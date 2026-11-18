//! On-demand compaction: the `compaction` request parameter and the signed `compaction`
//! block Claude returns in place of a reply (beta `compact-2026-09-04`).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The beta header every request that asks for a compaction or carries a
/// [`CompactionBlock`] sends.
pub const COMPACTION_BETA: &str = "compact-2026-09-04";

/// Asks Claude for a summary of the request's conversation in place of a reply.
///
/// Sent as the top-level `compaction` parameter. The response holds one
/// [`CompactionBlock`] and ends with `stop_reason` `"compaction"`.
///
/// # Example
///
/// ```rust
/// use adk_anthropic::CompactionParam;
///
/// let param = CompactionParam::summarize()
///     .with_instructions("Keep every file name. Do not call tools; reply with the summary only.");
/// assert_eq!(
///     serde_json::to_value(&param).unwrap(),
///     serde_json::json!({
///         "type": "summarize",
///         "instructions": "Keep every file name. Do not call tools; reply with the summary only."
///     })
/// );
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum CompactionParam {
    /// Summarizes every message the request carries.
    Summarize {
        /// A summarization prompt that replaces the API's own, up to 16,384 characters.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instructions: Option<String>,
    },
}

impl CompactionParam {
    /// A summary written with the API's own summarization prompt.
    pub fn summarize() -> Self {
        Self::Summarize { instructions: None }
    }

    /// Replaces the API's summarization prompt with `instructions`.
    pub fn with_instructions(self, instructions: impl Into<String>) -> Self {
        match self {
            Self::Summarize { .. } => Self::Summarize { instructions: Some(instructions.into()) },
        }
    }
}

/// The summary Claude writes in place of the messages it summarizes.
///
/// The block goes first in `messages` on every later request, in place of the messages it
/// summarizes, exactly as the API returned it: the `signature` covers its `content`, and the
/// API rejects a block with fields it did not return. Fields this client does not model,
/// such as `tool_changes`, are kept in [`extra`](Self::extra) and sent back unchanged.
///
/// # Example
///
/// ```rust
/// use adk_anthropic::{CompactionBlock, ContentBlock};
///
/// let wire = serde_json::json!({
///     "type": "compaction",
///     "content": "The user is designing a recipe app.",
///     "signature": "EuYBCkQY"
/// });
/// let block: ContentBlock = serde_json::from_value(wire.clone()).unwrap();
/// assert_eq!(block.as_compaction().map(|b| b.content.as_str()), Some("The user is designing a recipe app."));
/// assert_eq!(serde_json::to_value(&block).unwrap(), wire);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompactionBlock {
    /// The summary, as text a person can read.
    pub content: String,
    /// The signature that lets the API trust the block when it comes back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// Fields the API returned that this client does not model, sent back as received.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl CompactionBlock {
    /// A block with `content` and `signature`, as the API returns one.
    pub fn new(content: impl Into<String>, signature: impl Into<String>) -> Self {
        Self { content: content.into(), signature: Some(signature.into()), extra: Map::new() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ContentBlock;
    use serde_json::json;

    #[test]
    fn a_block_with_fields_this_client_does_not_model_round_trips_unchanged() {
        let wire = json!({
            "type": "compaction",
            "content": "Summary of the conversation.",
            "signature": "sig-1",
            "tool_changes": [{"type": "tool_addition", "tool": {"type": "tool_reference", "name": "get_forecast"}}]
        });
        let block: ContentBlock = serde_json::from_value(wire.clone()).unwrap();
        let ContentBlock::Compaction(compaction) = &block else {
            panic!("expected a compaction block, got {block:?}");
        };
        assert_eq!(compaction.content, "Summary of the conversation.");
        assert_eq!(compaction.signature.as_deref(), Some("sig-1"));
        assert!(compaction.extra.contains_key("tool_changes"));
        assert!(!compaction.extra.contains_key("type"));
        assert_eq!(serde_json::to_value(&block).unwrap(), wire);
    }

    #[test]
    fn the_parameter_leaves_out_instructions_it_does_not_have() {
        assert_eq!(
            serde_json::to_value(CompactionParam::summarize()).unwrap(),
            json!({"type": "summarize"})
        );
    }
}
