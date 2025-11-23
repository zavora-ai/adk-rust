use adk_core::{Result, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

/// GoogleSearch is a built-in tool that is automatically invoked by Gemini
/// models to retrieve search results from Google Search.
/// The tool operates internally within the model and does not require or
/// perform local code execution.
pub struct GoogleSearchTool;

impl GoogleSearchTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for GoogleSearchTool {
    fn name(&self) -> &str {
        "google_search"
    }

    fn description(&self) -> &str {
        "Performs a Google search to retrieve information from the web."
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        // Google Search is handled internally by Gemini models
        // This should not be called directly
        Err(adk_core::AdkError::Tool(
            "GoogleSearch is handled internally by Gemini".to_string(),
        ))
    }
}
