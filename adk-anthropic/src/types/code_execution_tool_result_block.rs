use serde::{Deserialize, Serialize};

use crate::types::CacheControlEphemeral;

/// Result of a server-side code execution call.
///
/// Carried by the `code_execution_tool_result`, `bash_code_execution_tool_result` and
/// `text_editor_code_execution_tool_result` content blocks, which the code execution
/// tool returns and the `web_search_20260209` / `web_fetch_20260209` tools return when
/// they filter results with code. `content` holds the result or error object as the
/// API sends it.
///
/// # Example
///
/// ```
/// use adk_anthropic::{CodeExecutionToolResultBlock, ContentBlock};
///
/// let block: ContentBlock = serde_json::from_value(serde_json::json!({
///     "type": "code_execution_tool_result",
///     "tool_use_id": "srvtoolu_01",
///     "content": {"type": "code_execution_result", "stdout": "4\n", "stderr": "", "return_code": 0, "content": []}
/// }))
/// .unwrap();
/// let ContentBlock::CodeExecutionToolResult(result) = block else { unreachable!() };
/// assert_eq!(result.tool_use_id, "srvtoolu_01");
/// assert_eq!(result.content["stdout"], "4\n");
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeExecutionToolResultBlock {
    /// ID of the `server_tool_use` block this result answers.
    pub tool_use_id: String,
    /// Result or error object, kept as sent by the API.
    pub content: serde_json::Value,
    /// Cache control for this block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControlEphemeral>,
}

impl CodeExecutionToolResultBlock {
    /// Creates a result block for `tool_use_id`.
    pub fn new(tool_use_id: impl Into<String>, content: serde_json::Value) -> Self {
        Self { tool_use_id: tool_use_id.into(), content, cache_control: None }
    }
}

#[cfg(test)]
mod tests {
    use crate::types::ContentBlock;
    use serde_json::json;

    #[test]
    fn every_code_execution_result_type_round_trips() {
        for kind in [
            "code_execution_tool_result",
            "bash_code_execution_tool_result",
            "text_editor_code_execution_tool_result",
        ] {
            let wire = json!({
                "type": kind,
                "tool_use_id": "srvtoolu_01",
                "content": {"type": "code_execution_tool_result_error", "error_code": "unavailable"}
            });
            let block: ContentBlock = serde_json::from_value(wire.clone()).unwrap();
            assert_eq!(serde_json::to_value(&block).unwrap(), wire, "{kind}");
        }
    }
}
