use adk_core::{Result, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

/// WebSearch is a built-in tool for Anthropic Claude models that enables
/// server-side web search. The model searches the web internally and returns
/// results as ServerToolUse / WebSearchToolResult content blocks.
#[derive(Default)]
pub struct WebSearchTool;

impl WebSearchTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn description(&self) -> &str {
        "Searches the web for current information (server-side)."
    }

    fn is_builtin(&self) -> bool {
        true
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        Err(adk_core::AdkError::tool("WebSearch is handled internally by Anthropic"))
    }
}
