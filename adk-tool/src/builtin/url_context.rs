use crate::builtin::bypass::BypassMultiToolsLimit;
use adk_core::{Result, Tool, ToolContext, ToolEffect};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;

/// UrlContext is a built-in tool that is automatically invoked by Gemini
/// models to fetch and analyze content from URLs.
/// The tool operates internally within the model and does not require or
/// perform local code execution.
#[derive(Default)]
pub struct UrlContextTool;

impl UrlContextTool {
    /// Create a new `UrlContextTool`.
    pub fn new() -> Self {
        Self
    }
}

/// Bypass support: convert the built-in URL-context tool into a
/// function-calling tool so it can be used alongside custom function tools
/// under the Gemini Interactions API.
///
/// The converted tool declares a `url: string` function schema and performs the
/// fetch-and-analyze behaviour by delegating to an internal single-turn agent
/// (an `LlmAgent` configured with [`UrlContextTool`] and a Gemini model).
///
/// # Example
///
/// ```rust,ignore
/// use adk_tool::{BypassMultiToolsLimit, UrlContextTool};
/// use std::sync::Arc;
///
/// // `url_agent` is an LlmAgent with UrlContextTool + a Gemini model.
/// let tool = UrlContextTool::new().with_bypass_multi_tools_limit(Arc::new(url_agent));
/// assert!(!tool.is_builtin());
/// ```
impl BypassMultiToolsLimit for UrlContextTool {
    fn bypass_name(&self) -> String {
        self.name().to_string()
    }

    fn bypass_description(&self) -> String {
        "Fetches and analyzes content from a URL to provide context.".to_string()
    }

    fn bypass_parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL whose content should be fetched and analyzed."
                }
            },
            "required": ["url"]
        })
    }

    fn bypass_query_field(&self) -> String {
        "url".to_string()
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

    // Runs on the provider; a policy sees this effect before the tool is attached.
    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    fn declaration(&self) -> Value {
        json!({
            "name": self.name(),
            "description": self.description(),
            "x-adk-gemini-tool": {
                "url_context": {}
            }
        })
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        Err(adk_core::AdkError::tool("UrlContext is handled internally by Gemini"))
    }
}
