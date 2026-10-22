use adk_core::{Result, Tool, ToolContext, ToolEffect};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;

/// DeepSeek's server-side web search, declared on its Responses endpoint as
/// `{"type": "web_search"}` (spec 3.1 §7.20).
///
/// DeepSeek runs the search with `deepseek-v4-pro`, adds the results to the context and
/// continues the response; `deepseek-flash` ignores the tool, and the Chat Completions
/// endpoint accepts function tools only. DeepSeek documents no options for it — no domain
/// allowlist, location or context size — so the declaration carries none
/// (<https://api-docs.deepseek.com/guides/responses_api>).
///
/// Declare it to an `adk_model::openai::OpenAIResponsesClient` whose base URL is
/// `https://api.deepseek.com`; the client reports
/// [`NativeWebSearchTool::DeepSeekWebSearch`](adk_core::NativeWebSearchTool::DeepSeekWebSearch)
/// from `Llm::native_web_search` for `deepseek-v4-pro`.
///
/// # Example
///
/// ```
/// use adk_core::Tool;
/// use adk_tool::DeepSeekWebSearchTool;
///
/// let tool = DeepSeekWebSearchTool::new();
/// assert!(tool.is_builtin());
/// assert_eq!(tool.declaration()["x-adk-openai-tool"], serde_json::json!({ "type": "web_search" }));
/// ```
#[derive(Debug, Clone, Copy, Default)]
pub struct DeepSeekWebSearchTool;

impl DeepSeekWebSearchTool {
    /// Creates the tool.
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for DeepSeekWebSearchTool {
    fn name(&self) -> &str {
        "deepseek_web_search"
    }

    fn description(&self) -> &str {
        "Searches the web for current information (DeepSeek server-side)."
    }

    fn is_builtin(&self) -> bool {
        true
    }

    // Runs on the provider; a policy sees this effect before the tool is attached.
    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    fn declaration(&self) -> Value {
        json!({
            "name": self.name(),
            "description": self.description(),
            "x-adk-openai-tool": { "type": "web_search" },
        })
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        Err(adk_core::AdkError::tool("DeepSeek web search is handled by DeepSeek's Responses API"))
    }
}
