use crate::builtin::bypass::BypassMultiToolsLimit;
use adk_core::{Result, Tool, ToolContext, ToolEffect};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;

/// Contextual Google Maps location used by Gemini retrieval config.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GoogleMapsContext {
    latitude: f64,
    longitude: f64,
}

impl GoogleMapsContext {
    /// Create a new contextual location.
    pub fn new(latitude: f64, longitude: f64) -> Self {
        Self { latitude, longitude }
    }

    fn to_json(self) -> Value {
        json!({
            "retrievalConfig": {
                "latLng": {
                    "latitude": self.latitude,
                    "longitude": self.longitude,
                }
            }
        })
    }
}

/// Gemini built-in Google Maps grounding tool.
#[derive(Debug, Clone, Default)]
pub struct GoogleMapsTool {
    enable_widget: bool,
    context: Option<GoogleMapsContext>,
}

impl GoogleMapsTool {
    /// Create a new `GoogleMapsTool` with default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Enable or disable the interactive widget in the response.
    pub fn with_widget(mut self, enable_widget: bool) -> Self {
        self.enable_widget = enable_widget;
        self
    }

    /// Set the contextual location for map queries.
    pub fn with_context(mut self, context: GoogleMapsContext) -> Self {
        self.context = Some(context);
        self
    }
}

#[async_trait]
impl Tool for GoogleMapsTool {
    fn name(&self) -> &str {
        "google_maps"
    }

    fn description(&self) -> &str {
        "Grounds responses with Google Maps data for places, routes, and local context."
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
                "google_maps": {
                    "enable_widget": self.enable_widget.then_some(true),
                }
            },
            "x-adk-gemini-tool-config": self.context.map(GoogleMapsContext::to_json),
        })
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        Err(adk_core::AdkError::tool("GoogleMaps is handled internally by Gemini"))
    }
}

/// Gemini built-in code execution tool.
#[derive(Debug, Clone, Default)]
pub struct GeminiCodeExecutionTool;

impl GeminiCodeExecutionTool {
    /// Create a new `GeminiCodeExecutionTool`.
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for GeminiCodeExecutionTool {
    fn name(&self) -> &str {
        "gemini_code_execution"
    }

    fn description(&self) -> &str {
        "Allows Gemini to write and execute Python code server-side."
    }

    fn is_builtin(&self) -> bool {
        true
    }

    // Runs or acts outside the agent; never assumed safe to repeat.
    fn effect(&self) -> ToolEffect {
        ToolEffect::NonIdempotent
    }

    fn declaration(&self) -> Value {
        json!({
            "name": self.name(),
            "description": self.description(),
            "x-adk-gemini-tool": {
                "code_execution": {}
            }
        })
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        Err(adk_core::AdkError::tool("Gemini code execution is handled internally by Gemini"))
    }
}

/// Gemini built-in file search tool.
#[derive(Debug, Clone)]
pub struct GeminiFileSearchTool {
    file_search_store_names: Vec<String>,
}

impl GeminiFileSearchTool {
    /// Create a new `GeminiFileSearchTool` with the given store names.
    pub fn new(file_search_store_names: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            file_search_store_names: file_search_store_names.into_iter().map(Into::into).collect(),
        }
    }
}

#[async_trait]
impl Tool for GeminiFileSearchTool {
    fn name(&self) -> &str {
        "gemini_file_search"
    }

    fn description(&self) -> &str {
        "Searches Gemini File Search stores for relevant documents."
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
                "file_search": {
                    "file_search_store_names": self.file_search_store_names
                }
            }
        })
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        Err(adk_core::AdkError::tool("Gemini file search is handled internally by Gemini"))
    }
}

/// Bypass support: convert the built-in Gemini File Search tool into a
/// function-calling tool so it can be used alongside custom function tools
/// under the Gemini Interactions API.
///
/// The converted tool declares a `query: string` function schema and performs
/// the document search by delegating to an internal single-turn agent (an
/// `LlmAgent` configured with [`GeminiFileSearchTool`] and a Gemini model).
///
/// # Example
///
/// ```rust,ignore
/// use adk_tool::{BypassMultiToolsLimit, GeminiFileSearchTool};
/// use std::sync::Arc;
///
/// // `file_search_agent` is an LlmAgent with GeminiFileSearchTool + a Gemini model.
/// let tool = GeminiFileSearchTool::new(["my-store"])
///     .with_bypass_multi_tools_limit(Arc::new(file_search_agent));
/// assert!(!tool.is_builtin());
/// ```
impl BypassMultiToolsLimit for GeminiFileSearchTool {
    fn bypass_name(&self) -> String {
        self.name().to_string()
    }

    fn bypass_description(&self) -> String {
        "Searches Gemini File Search stores for documents relevant to a query.".to_string()
    }

    fn bypass_parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The query used to search the File Search stores."
                }
            },
            "required": ["query"]
        })
    }

    fn bypass_query_field(&self) -> String {
        "query".to_string()
    }
}

/// Target environment for Gemini computer use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeminiComputerEnvironment {
    /// Browser-based environment.
    Browser,
}

impl GeminiComputerEnvironment {
    fn as_wire(self) -> &'static str {
        match self {
            Self::Browser => "ENVIRONMENT_BROWSER",
        }
    }
}

/// Gemini built-in computer use tool declaration.
#[derive(Debug, Clone)]
pub struct GeminiComputerUseTool {
    environment: GeminiComputerEnvironment,
    excluded_predefined_functions: Vec<String>,
}

impl Default for GeminiComputerUseTool {
    fn default() -> Self {
        Self {
            environment: GeminiComputerEnvironment::Browser,
            excluded_predefined_functions: Vec::new(),
        }
    }
}

impl GeminiComputerUseTool {
    /// Create a new `GeminiComputerUseTool` for the given environment.
    pub fn new(environment: GeminiComputerEnvironment) -> Self {
        Self { environment, ..Default::default() }
    }

    /// Exclude specific predefined functions from computer use.
    pub fn with_excluded_functions(
        mut self,
        excluded_predefined_functions: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.excluded_predefined_functions =
            excluded_predefined_functions.into_iter().map(Into::into).collect();
        self
    }
}

#[async_trait]
impl Tool for GeminiComputerUseTool {
    fn name(&self) -> &str {
        "gemini_computer_use"
    }

    fn description(&self) -> &str {
        "Enables Gemini computer use, which emits predefined UI action function calls."
    }

    fn is_builtin(&self) -> bool {
        true
    }

    // Runs or acts outside the agent; never assumed safe to repeat.
    fn effect(&self) -> ToolEffect {
        ToolEffect::NonIdempotent
    }

    fn declaration(&self) -> Value {
        json!({
            "name": self.name(),
            "description": self.description(),
            "x-adk-gemini-tool": {
                "computer_use": {
                    "environment": self.environment.as_wire(),
                    "excluded_predefined_functions": (!self.excluded_predefined_functions.is_empty())
                        .then_some(self.excluded_predefined_functions.clone()),
                }
            }
        })
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        Err(adk_core::AdkError::tool("Gemini computer use actions must be executed client-side"))
    }
}
