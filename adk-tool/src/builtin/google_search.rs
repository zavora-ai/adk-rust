use crate::builtin::bypass::BypassMultiToolsLimit;
use adk_core::{Result, Tool, ToolContext, ToolEffect};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;

/// GoogleSearch is a built-in tool that is automatically invoked by Gemini
/// models to retrieve search results from Google Search.
/// The tool operates internally within the model and does not require or
/// perform local code execution.
#[derive(Default)]
pub struct GoogleSearchTool;

impl GoogleSearchTool {
    /// Create a new `GoogleSearchTool`.
    pub fn new() -> Self {
        Self
    }
}

/// Bypass support: convert the built-in Google Search tool into a
/// function-calling tool so it can be used alongside custom function tools.
///
/// The Gemini Interactions API forbids mixing built-in (server-side) tools with
/// custom function tools in one request. Implementing [`BypassMultiToolsLimit`]
/// mirrors ADK-Python's `bypass_multi_tools_limit=True`: the converted tool
/// reports `is_builtin() == false`, declares a normal `query: string` function
/// schema, and performs grounded search by delegating to an internal
/// single-turn agent.
///
/// Because `adk-tool` cannot depend on `adk-agent`, the internal
/// grounded-search agent is supplied by the caller. It is expected to be an
/// `LlmAgent` configured with [`GoogleSearchTool`] and a Gemini model so that
/// the grounding happens server-side.
///
/// # Example
///
/// ```rust,ignore
/// use adk_tool::{BypassMultiToolsLimit, GoogleSearchTool};
/// use std::sync::Arc;
///
/// // `search_agent` is an LlmAgent with GoogleSearchTool + a Gemini model.
/// let tool = GoogleSearchTool::new().with_bypass_multi_tools_limit(Arc::new(search_agent));
/// assert!(!tool.is_builtin());
/// ```
impl BypassMultiToolsLimit for GoogleSearchTool {
    fn bypass_name(&self) -> String {
        self.name().to_string()
    }

    fn bypass_description(&self) -> String {
        "Performs a Google search to retrieve information from the web.".to_string()
    }

    fn bypass_parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query to look up on Google."
                }
            },
            "required": ["query"]
        })
    }

    fn bypass_query_field(&self) -> String {
        "query".to_string()
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
            "x-adk-gemini-tool": {
                "google_search": {}
            }
        })
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        // Google Search is handled internally by Gemini models
        // This should not be called directly
        Err(adk_core::AdkError::tool("GoogleSearch is handled internally by Gemini"))
    }
}
