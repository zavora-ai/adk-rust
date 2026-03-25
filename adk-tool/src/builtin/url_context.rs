use adk_core::{Result, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

/// UrlContext is a built-in tool that is automatically invoked by Gemini
/// models to fetch and analyze content from URLs.
/// The tool operates internally within the model and does not require or
/// perform local code execution.
#[derive(Default)]
pub struct UrlContextTool;

impl UrlContextTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for UrlContextTool {
    fn name(&self) -> &str {
        "url_context"
    }

    fn description(&self) -> &str {
        "Fetches and analyzes content from URLs to provide context."
    }

    fn is_builtin(&self) -> bool {
        true
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        Err(adk_core::AdkError::tool("UrlContext is handled internally by Gemini"))
    }
}
