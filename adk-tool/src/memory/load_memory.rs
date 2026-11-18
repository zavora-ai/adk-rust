//! LoadMemoryTool — an agent-callable tool for searching long-term memory.
//!
//! Agents invoke this tool during reasoning to retrieve relevant context
//! from the configured [`MemoryService`].

use std::sync::Arc;

use adk_core::{AdkError, Result, Tool, ToolContext};
use adk_memory::{MemoryService, SearchRequest};
use async_trait::async_trait;
use serde_json::{Value, json};

use super::config::MemoryToolConfig;
use super::format::format_memory_results;

/// A tool that searches the agent's long-term memory on demand.
///
/// Agents call this tool during reasoning to retrieve relevant memories.
/// It delegates to the configured [`MemoryService`] and returns formatted results.
///
/// # Example
///
/// ```rust,ignore
/// use adk_tool::memory::LoadMemoryTool;
/// use adk_memory::InMemoryMemoryService;
/// use std::sync::Arc;
///
/// let service = Arc::new(InMemoryMemoryService::new());
/// let tool = LoadMemoryTool::builder()
///     .memory_service(service)
///     .max_results(5)
///     .build()
///     .unwrap();
/// ```
pub struct LoadMemoryTool {
    memory_service: Arc<dyn MemoryService>,
    config: MemoryToolConfig,
}

#[async_trait]
impl Tool for LoadMemoryTool {
    fn name(&self) -> &str {
        "load_memory"
    }

    fn description(&self) -> &str {
        "Search the agent's long-term memory for relevant information. \
         Use this when you need to recall past conversations, facts, or context."
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn is_concurrency_safe(&self) -> bool {
        true
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query to find relevant memories"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of results to return",
                    "minimum": 1,
                    "maximum": 100
                }
            },
            "required": ["query"]
        }))
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AdkError::tool("query parameter is required"))?
            .to_string();

        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|v| (v as usize).min(self.config.max_results))
            .unwrap_or(self.config.max_results);

        let request = SearchRequest {
            query,
            user_id: ctx.user_id().to_string(),
            app_name: ctx.app_name().to_string(),
            limit: Some(limit),
            min_score: self.config.min_relevance_score,
            project_id: self.config.project_id.clone(),
        };

        let response = self.memory_service.search(request).await?;
        Ok(format_memory_results(&response.memories))
    }
}

impl LoadMemoryTool {
    /// Create a new builder for `LoadMemoryTool`.
    pub fn builder() -> LoadMemoryToolBuilder {
        LoadMemoryToolBuilder::default()
    }
}

/// Builder for [`LoadMemoryTool`].
#[derive(Default)]
pub struct LoadMemoryToolBuilder {
    memory_service: Option<Arc<dyn MemoryService>>,
    config: MemoryToolConfig,
}

impl LoadMemoryToolBuilder {
    /// Set the memory service implementation.
    pub fn memory_service(mut self, service: Arc<dyn MemoryService>) -> Self {
        self.memory_service = Some(service);
        self
    }

    /// Set the maximum number of results to return.
    pub fn max_results(mut self, max: usize) -> Self {
        self.config.max_results = max;
        self
    }

    /// Set the minimum relevance score threshold.
    pub fn min_relevance_score(mut self, score: f32) -> Self {
        self.config.min_relevance_score = Some(score);
        self
    }

    /// Set the project identifier for scoped searches.
    pub fn project_id(mut self, id: impl Into<String>) -> Self {
        self.config.project_id = Some(id.into());
        self
    }

    /// Build the `LoadMemoryTool`, validating configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - `memory_service` is not configured
    /// - `max_results` is outside [1, 100]
    /// - `min_relevance_score` is outside [0.0, 1.0]
    pub fn build(self) -> Result<LoadMemoryTool> {
        let memory_service = self
            .memory_service
            .ok_or_else(|| AdkError::tool("memory_service is required for LoadMemoryTool"))?;

        self.config.validate()?;

        Ok(LoadMemoryTool { memory_service, config: self.config })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::{Content, EventActions, ReadonlyContext};
    use adk_memory::{InMemoryMemoryService, MemoryEntry};
    use std::sync::Mutex;

    struct MockToolContext {
        content: Content,
        actions: Mutex<EventActions>,
    }

    impl MockToolContext {
        fn new() -> Self {
            Self {
                content: Content::new("user").with_text("test input"),
                actions: Mutex::new(EventActions::default()),
            }
        }
    }

    #[async_trait]
    impl ReadonlyContext for MockToolContext {
        fn invocation_id(&self) -> &str {
            "inv-1"
        }
        fn agent_name(&self) -> &str {
            "test-agent"
        }
        fn user_id(&self) -> &str {
            "user-1"
        }
        fn app_name(&self) -> &str {
            "test-app"
        }
        fn session_id(&self) -> &str {
            "session-1"
        }
        fn branch(&self) -> &str {
            ""
        }
        fn user_content(&self) -> &Content {
            &self.content
        }
    }

    #[async_trait]
    impl adk_core::CallbackContext for MockToolContext {
        fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
            None
        }
    }

    #[async_trait]
    impl ToolContext for MockToolContext {
        fn function_call_id(&self) -> &str {
            "call-1"
        }
        fn actions(&self) -> EventActions {
            self.actions.lock().unwrap().clone()
        }
        fn set_actions(&self, actions: EventActions) {
            *self.actions.lock().unwrap() = actions;
        }
        async fn search_memory(&self, _query: &str) -> Result<Vec<adk_core::MemoryEntry>> {
            Ok(vec![])
        }
    }

    #[test]
    fn test_tool_metadata() {
        let service = Arc::new(InMemoryMemoryService::new());
        let tool = LoadMemoryTool::builder().memory_service(service).build().unwrap();

        assert_eq!(tool.name(), "load_memory");
        assert!(tool.is_read_only());
        assert!(tool.is_concurrency_safe());
        assert!(tool.parameters_schema().is_some());
    }

    #[test]
    fn test_builder_missing_service() {
        let result = LoadMemoryTool::builder().build();
        assert!(result.is_err());
    }

    #[test]
    fn test_builder_invalid_max_results() {
        let service = Arc::new(InMemoryMemoryService::new());
        let result = LoadMemoryTool::builder().memory_service(service).max_results(0).build();
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_execute_with_query() {
        let service = Arc::new(InMemoryMemoryService::new());

        // Add a memory entry
        let entry = MemoryEntry::new(
            Content::new("user").with_text("The capital of France is Paris"),
            "user",
        );
        service.add_session("test-app", "user-1", "session-1", vec![entry]).await.unwrap();

        let tool = LoadMemoryTool::builder().memory_service(service).build().unwrap();

        let ctx = Arc::new(MockToolContext::new()) as Arc<dyn ToolContext>;
        let args = json!({"query": "capital of France"});
        let result = tool.execute(ctx, args).await.unwrap();

        assert_eq!(result["count"], 1);
        let memories = result["memories"].as_array().unwrap();
        assert_eq!(memories[0]["content"], "The capital of France is Paris");
    }

    #[tokio::test]
    async fn test_execute_missing_query() {
        let service = Arc::new(InMemoryMemoryService::new());
        let tool = LoadMemoryTool::builder().memory_service(service).build().unwrap();

        let ctx = Arc::new(MockToolContext::new()) as Arc<dyn ToolContext>;
        let args = json!({});
        let result = tool.execute(ctx, args).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_execute_empty_results() {
        let service = Arc::new(InMemoryMemoryService::new());
        let tool = LoadMemoryTool::builder().memory_service(service).build().unwrap();

        let ctx = Arc::new(MockToolContext::new()) as Arc<dyn ToolContext>;
        let args = json!({"query": "nonexistent"});
        let result = tool.execute(ctx, args).await.unwrap();

        assert_eq!(result["count"], 0);
        assert_eq!(result["memories"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn test_parameters_schema_structure() {
        let service = Arc::new(InMemoryMemoryService::new());
        let tool = LoadMemoryTool::builder().memory_service(service).build().unwrap();

        let schema = tool.parameters_schema().unwrap();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["query"].is_object());
        assert_eq!(schema["properties"]["query"]["type"], "string");
        assert!(schema["properties"]["limit"].is_object());
        assert_eq!(schema["properties"]["limit"]["type"], "integer");

        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("query")));
    }
}
