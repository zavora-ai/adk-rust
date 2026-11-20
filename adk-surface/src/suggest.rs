//! Suggested replies (spec 3.3 §5.1): an agent offers the person up to four short replies they
//! might send next. The home shows them under the agent's latest message; tapping one fills the
//! message box, and the person still sends it. A suggestion is the person's own message once
//! sent, never an answer to a decision (§7.4, S-I4).
//!
//! Hosts give an agent the tool with [`SuggestReplies`]:
//!
//! ```rust
//! use std::sync::Arc;
//! use adk_core::Tool;
//! use adk_surface::suggest::SuggestReplies;
//!
//! let tool: Arc<dyn Tool> = Arc::new(SuggestReplies);
//! assert_eq!(tool.name(), "suggest_replies");
//! ```

use std::sync::Arc;

use adk_core::{
    AdkError, ErrorCategory, ErrorComponent, Result, TOOL_INVALID_ARGUMENTS_CODE, Tool,
    ToolContext, ToolEffect,
};
use async_trait::async_trait;
use serde_json::{Value, json};

/// The tool's name.
pub const SUGGEST_REPLIES_TOOL: &str = "suggest_replies";

/// The most replies one call offers.
pub const MAX_SUGGESTIONS: usize = 4;

/// The most characters one reply has.
pub const SUGGESTION_CHARS: usize = 80;

/// The tool an agent offers suggested replies with (spec 3.3 §5.1). It changes nothing: the
/// replies are shown, and only the person sends one.
#[derive(Debug, Clone, Copy, Default)]
pub struct SuggestReplies;

/// The replies `args` offers, checked: one to [`MAX_SUGGESTIONS`], each one line of text of at
/// most [`SUGGESTION_CHARS`] characters, none repeated.
///
/// # Errors
///
/// Why the replies are not shown.
pub fn replies_of(args: &Value) -> std::result::Result<Vec<String>, String> {
    let replies = args
        .get("replies")
        .and_then(Value::as_array)
        .ok_or_else(|| "replies must be a list of strings".to_string())?;
    if replies.is_empty() || replies.len() > MAX_SUGGESTIONS {
        return Err(format!("offer between 1 and {MAX_SUGGESTIONS} replies"));
    }
    let mut out: Vec<String> = Vec::new();
    for reply in replies {
        let text = reply.as_str().ok_or_else(|| "each reply is a string".to_string())?.trim();
        if text.is_empty() {
            return Err("a reply is empty".into());
        }
        if text.contains(['\n', '\r']) {
            return Err("a reply is one line".into());
        }
        if text.chars().count() > SUGGESTION_CHARS {
            return Err(format!("a reply has at most {SUGGESTION_CHARS} characters"));
        }
        if !out.iter().any(|seen| seen == text) {
            out.push(text.to_string());
        }
    }
    Ok(out)
}

#[async_trait]
impl Tool for SuggestReplies {
    fn name(&self) -> &str {
        SUGGEST_REPLIES_TOOL
    }

    fn description(&self) -> &str {
        "Offer the person up to 4 short replies they might send next, such as follow-up \
         questions. They appear under your message; tapping one only fills the person's message \
         box, and they still choose to send it. Use plain, specific words; never offer an \
         approval or a decision."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "replies": {
                    "type": "array",
                    "items": { "type": "string", "maxLength": SUGGESTION_CHARS },
                    "minItems": 1,
                    "maxItems": MAX_SUGGESTIONS,
                    "description": "What the person might say next, each one short line."
                }
            },
            "required": ["replies"]
        }))
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    fn is_concurrency_safe(&self) -> bool {
        true
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let replies = replies_of(&args).map_err(|error| {
            AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::InvalidInput,
                TOOL_INVALID_ARGUMENTS_CODE,
                format!("invalid arguments for '{SUGGEST_REPLIES_TOOL}': {error}. Correct them and call it again."),
            )
        })?;
        Ok(json!({ "shown": replies.len() }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takes_one_to_four_short_lines_and_drops_repeats() {
        assert_eq!(
            replies_of(
                &json!({ "replies": [" What changed at 09:12? ", "Show the logs", "Show the logs"] })
            ),
            Ok(vec!["What changed at 09:12?".to_string(), "Show the logs".to_string()])
        );
        for bad in [
            json!({}),
            json!({ "replies": [] }),
            json!({ "replies": ["a", "b", "c", "d", "e"] }),
            json!({ "replies": [" "] }),
            json!({ "replies": ["two\nlines"] }),
            json!({ "replies": [7] }),
            json!({ "replies": ["x".repeat(SUGGESTION_CHARS + 1)] }),
        ] {
            assert!(replies_of(&bad).is_err(), "{bad}");
        }
    }
}
