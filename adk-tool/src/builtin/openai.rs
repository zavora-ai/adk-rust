use adk_core::{Result, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::{Map, Value, json};
use std::sync::Arc;

/// Approximate user location for OpenAI web search.
#[derive(Debug, Clone, Default)]
pub struct OpenAIApproximateLocation {
    city: Option<String>,
    country: Option<String>,
    region: Option<String>,
    timezone: Option<String>,
}

impl OpenAIApproximateLocation {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_city(mut self, city: impl Into<String>) -> Self {
        self.city = Some(city.into());
        self
    }

    pub fn with_country(mut self, country: impl Into<String>) -> Self {
        self.country = Some(country.into());
        self
    }

    pub fn with_region(mut self, region: impl Into<String>) -> Self {
        self.region = Some(region.into());
        self
    }

    pub fn with_timezone(mut self, timezone: impl Into<String>) -> Self {
        self.timezone = Some(timezone.into());
        self
    }

    fn to_json(&self) -> Value {
        json!({
            "type": "approximate",
            "city": self.city,
            "country": self.country,
            "region": self.region,
            "timezone": self.timezone,
        })
    }
}

/// OpenAI web search tool flavor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OpenAIWebSearchVariant {
    #[default]
    Stable20250826,
    Preview20250311,
}

impl OpenAIWebSearchVariant {
    fn as_wire(self) -> &'static str {
        match self {
            Self::Stable20250826 => "web_search_2025_08_26",
            Self::Preview20250311 => "web_search_preview_2025_03_11",
        }
    }
}

/// OpenAI hosted web search tool.
#[derive(Debug, Clone, Default)]
pub struct OpenAIWebSearchTool {
    variant: OpenAIWebSearchVariant,
    allowed_domains: Option<Vec<String>>,
    user_location: Option<OpenAIApproximateLocation>,
    search_context_size: Option<String>,
}

impl OpenAIWebSearchTool {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn preview(mut self) -> Self {
        self.variant = OpenAIWebSearchVariant::Preview20250311;
        self
    }

    pub fn with_allowed_domains(
        mut self,
        domains: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.allowed_domains = Some(domains.into_iter().map(Into::into).collect());
        self
    }

    pub fn with_user_location(mut self, user_location: OpenAIApproximateLocation) -> Self {
        self.user_location = Some(user_location);
        self
    }

    pub fn with_search_context_size(mut self, size: impl Into<String>) -> Self {
        self.search_context_size = Some(size.into());
        self
    }

    fn tool_json(&self) -> Value {
        json!({
            "type": self.variant.as_wire(),
            "filters": self.allowed_domains.as_ref().map(|domains| json!({ "allowed_domains": domains })),
            "user_location": self.user_location.as_ref().map(OpenAIApproximateLocation::to_json),
            "search_context_size": self.search_context_size,
        })
    }
}

#[async_trait]
impl Tool for OpenAIWebSearchTool {
    fn name(&self) -> &str {
        "openai_web_search"
    }

    fn description(&self) -> &str {
        "Uses OpenAI hosted web search to retrieve current web information."
    }

    fn is_builtin(&self) -> bool {
        true
    }

    fn declaration(&self) -> Value {
        json!({
            "name": self.name(),
            "description": self.description(),
            "x-adk-openai-tool": self.tool_json(),
        })
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        Err(adk_core::AdkError::tool("OpenAI web search is handled by the Responses API"))
    }
}

/// OpenAI hosted file search tool.
#[derive(Debug, Clone)]
pub struct OpenAIFileSearchTool {
    vector_store_ids: Vec<String>,
    max_num_results: Option<u32>,
    filters: Option<Value>,
    ranking_options: Option<Value>,
}

impl OpenAIFileSearchTool {
    pub fn new(vector_store_ids: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            vector_store_ids: vector_store_ids.into_iter().map(Into::into).collect(),
            max_num_results: None,
            filters: None,
            ranking_options: None,
        }
    }

    pub fn with_max_num_results(mut self, max_num_results: u32) -> Self {
        self.max_num_results = Some(max_num_results);
        self
    }

    pub fn with_filters(mut self, filters: Value) -> Self {
        self.filters = Some(filters);
        self
    }

    pub fn with_ranking_options(mut self, ranking_options: Value) -> Self {
        self.ranking_options = Some(ranking_options);
        self
    }

    fn tool_json(&self) -> Value {
        json!({
            "type": "file_search",
            "vector_store_ids": self.vector_store_ids,
            "max_num_results": self.max_num_results,
            "filters": self.filters,
            "ranking_options": self.ranking_options,
        })
    }
}

#[async_trait]
impl Tool for OpenAIFileSearchTool {
    fn name(&self) -> &str {
        "openai_file_search"
    }

    fn description(&self) -> &str {
        "Uses OpenAI hosted file search against one or more vector stores."
    }

    fn is_builtin(&self) -> bool {
        true
    }

    fn declaration(&self) -> Value {
        json!({
            "name": self.name(),
            "description": self.description(),
            "x-adk-openai-tool": self.tool_json(),
        })
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        Err(adk_core::AdkError::tool("OpenAI file search is handled by the Responses API"))
    }
}

/// OpenAI hosted code interpreter tool.
#[derive(Debug, Clone, Default)]
pub struct OpenAICodeInterpreterTool {
    file_ids: Vec<String>,
    memory_limit: Option<u64>,
    container_id: Option<String>,
}

impl OpenAICodeInterpreterTool {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_file_ids(mut self, file_ids: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.file_ids = file_ids.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_memory_limit(mut self, memory_limit: u64) -> Self {
        self.memory_limit = Some(memory_limit);
        self
    }

    pub fn with_container_id(mut self, container_id: impl Into<String>) -> Self {
        self.container_id = Some(container_id.into());
        self
    }

    fn tool_json(&self) -> Value {
        let container = if let Some(container_id) = &self.container_id {
            Value::String(container_id.clone())
        } else {
            json!({
                "type": "auto",
                "file_ids": (!self.file_ids.is_empty()).then_some(self.file_ids.clone()),
                "memory_limit": self.memory_limit,
            })
        };

        json!({
            "type": "code_interpreter",
            "container": container,
        })
    }
}

#[async_trait]
impl Tool for OpenAICodeInterpreterTool {
    fn name(&self) -> &str {
        "openai_code_interpreter"
    }

    fn description(&self) -> &str {
        "Uses OpenAI hosted code interpreter to execute Python and return outputs."
    }

    fn is_builtin(&self) -> bool {
        true
    }

    fn declaration(&self) -> Value {
        json!({
            "name": self.name(),
            "description": self.description(),
            "x-adk-openai-tool": self.tool_json(),
        })
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        Err(adk_core::AdkError::tool("OpenAI code interpreter is handled by the Responses API"))
    }
}

/// OpenAI hosted image generation tool.
#[derive(Debug, Clone, Default)]
pub struct OpenAIImageGenerationTool {
    options: Map<String, Value>,
}

impl OpenAIImageGenerationTool {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_option(mut self, key: impl Into<String>, value: Value) -> Self {
        self.options.insert(key.into(), value);
        self
    }

    fn tool_json(&self) -> Value {
        let mut tool = self.options.clone();
        tool.insert("type".to_string(), Value::String("image_generation".to_string()));
        Value::Object(tool)
    }
}

#[async_trait]
impl Tool for OpenAIImageGenerationTool {
    fn name(&self) -> &str {
        "openai_image_generation"
    }

    fn description(&self) -> &str {
        "Uses OpenAI hosted image generation."
    }

    fn is_builtin(&self) -> bool {
        true
    }

    fn declaration(&self) -> Value {
        json!({
            "name": self.name(),
            "description": self.description(),
            "x-adk-openai-tool": self.tool_json(),
        })
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        Err(adk_core::AdkError::tool("OpenAI image generation is handled by the Responses API"))
    }
}

/// OpenAI computer use environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenAIComputerEnvironment {
    Browser,
    Mac,
    Windows,
    Linux,
    Ubuntu,
}

impl OpenAIComputerEnvironment {
    fn as_wire(self) -> &'static str {
        match self {
            Self::Browser => "browser",
            Self::Mac => "mac",
            Self::Windows => "windows",
            Self::Linux => "linux",
            Self::Ubuntu => "ubuntu",
        }
    }
}

/// OpenAI computer use tool declaration.
#[derive(Debug, Clone)]
pub struct OpenAIComputerUseTool {
    environment: OpenAIComputerEnvironment,
    display_width: u32,
    display_height: u32,
}

impl OpenAIComputerUseTool {
    pub fn new(
        environment: OpenAIComputerEnvironment,
        display_width: u32,
        display_height: u32,
    ) -> Self {
        Self { environment, display_width, display_height }
    }

    fn tool_json(&self) -> Value {
        json!({
            "type": "computer_use_preview",
            "environment": self.environment.as_wire(),
            "display_width": self.display_width,
            "display_height": self.display_height,
        })
    }
}

#[async_trait]
impl Tool for OpenAIComputerUseTool {
    fn name(&self) -> &str {
        "openai_computer_use"
    }

    fn description(&self) -> &str {
        "Enables OpenAI computer use tool calls."
    }

    fn is_builtin(&self) -> bool {
        true
    }

    fn declaration(&self) -> Value {
        json!({
            "name": self.name(),
            "description": self.description(),
            "x-adk-openai-tool": self.tool_json(),
        })
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        Err(adk_core::AdkError::tool("OpenAI computer use requires client-side action handling"))
    }
}

/// OpenAI remote MCP tool declaration.
#[derive(Debug, Clone)]
pub struct OpenAIMcpTool {
    server_label: String,
    definition: Map<String, Value>,
}

impl OpenAIMcpTool {
    pub fn new_with_url(server_label: impl Into<String>, server_url: impl Into<String>) -> Self {
        let server_label = server_label.into();
        let mut definition = Map::new();
        definition.insert("type".to_string(), Value::String("mcp".to_string()));
        definition.insert("server_label".to_string(), Value::String(server_label.clone()));
        definition.insert("server_url".to_string(), Value::String(server_url.into()));
        Self { server_label, definition }
    }

    pub fn new_with_connector(
        server_label: impl Into<String>,
        connector_id: impl Into<String>,
    ) -> Self {
        let server_label = server_label.into();
        let mut definition = Map::new();
        definition.insert("type".to_string(), Value::String("mcp".to_string()));
        definition.insert("server_label".to_string(), Value::String(server_label.clone()));
        definition.insert("connector_id".to_string(), Value::String(connector_id.into()));
        Self { server_label, definition }
    }

    pub fn with_allowed_tools(mut self, allowed_tools: Value) -> Self {
        self.definition.insert("allowed_tools".to_string(), allowed_tools);
        self
    }

    pub fn with_authorization(mut self, authorization: impl Into<String>) -> Self {
        self.definition.insert("authorization".to_string(), Value::String(authorization.into()));
        self
    }

    pub fn with_headers(mut self, headers: Map<String, Value>) -> Self {
        self.definition.insert("headers".to_string(), Value::Object(headers));
        self
    }

    pub fn with_require_approval(mut self, require_approval: Value) -> Self {
        self.definition.insert("require_approval".to_string(), require_approval);
        self
    }

    fn tool_json(&self) -> Value {
        Value::Object(self.definition.clone())
    }
}

#[async_trait]
impl Tool for OpenAIMcpTool {
    fn name(&self) -> &str {
        &self.server_label
    }

    fn description(&self) -> &str {
        "Grants the model access to a remote MCP server."
    }

    fn is_builtin(&self) -> bool {
        true
    }

    fn declaration(&self) -> Value {
        json!({
            "name": self.name(),
            "description": self.description(),
            "x-adk-openai-tool": self.tool_json(),
        })
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        Err(adk_core::AdkError::tool("OpenAI MCP tool calls are handled by the Responses API"))
    }
}

/// OpenAI local shell tool declaration.
#[derive(Debug, Clone, Default)]
pub struct OpenAILocalShellTool;

impl OpenAILocalShellTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for OpenAILocalShellTool {
    fn name(&self) -> &str {
        "openai_local_shell"
    }

    fn description(&self) -> &str {
        "Allows OpenAI to execute commands in the local shell tool protocol."
    }

    fn is_builtin(&self) -> bool {
        true
    }

    fn declaration(&self) -> Value {
        json!({
            "name": self.name(),
            "description": self.description(),
            "x-adk-openai-tool": {
                "type": "local_shell"
            },
        })
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        Err(adk_core::AdkError::tool(
            "OpenAI local shell outputs must be handled through the Responses API item protocol",
        ))
    }
}

/// OpenAI managed shell tool declaration.
#[derive(Debug, Clone, Default)]
pub struct OpenAIShellTool {
    environment: Option<Value>,
}

impl OpenAIShellTool {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_environment(mut self, environment: Value) -> Self {
        self.environment = Some(environment);
        self
    }

    fn tool_json(&self) -> Value {
        json!({
            "type": "shell",
            "environment": self.environment,
        })
    }
}

#[async_trait]
impl Tool for OpenAIShellTool {
    fn name(&self) -> &str {
        "openai_shell"
    }

    fn description(&self) -> &str {
        "Allows OpenAI to execute commands in the managed shell tool protocol."
    }

    fn is_builtin(&self) -> bool {
        true
    }

    fn declaration(&self) -> Value {
        json!({
            "name": self.name(),
            "description": self.description(),
            "x-adk-openai-tool": self.tool_json(),
        })
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        Err(adk_core::AdkError::tool(
            "OpenAI shell outputs must be handled through the Responses API item protocol",
        ))
    }
}

/// OpenAI apply_patch tool declaration.
#[derive(Debug, Clone, Default)]
pub struct OpenAIApplyPatchTool;

impl OpenAIApplyPatchTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for OpenAIApplyPatchTool {
    fn name(&self) -> &str {
        "openai_apply_patch"
    }

    fn description(&self) -> &str {
        "Allows OpenAI to propose file patches through the native apply_patch tool."
    }

    fn is_builtin(&self) -> bool {
        true
    }

    fn declaration(&self) -> Value {
        json!({
            "name": self.name(),
            "description": self.description(),
            "x-adk-openai-tool": {
                "type": "apply_patch"
            },
        })
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        Err(adk_core::AdkError::tool(
            "OpenAI apply_patch outputs must be handled through the Responses API item protocol",
        ))
    }
}
