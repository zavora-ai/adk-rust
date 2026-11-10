//! PreloadMemoryTool — automatically loads relevant memories at turn start.
//!
//! This tool can be used in two ways:
//! 1. As a regular tool that agents can call during reasoning
//! 2. As a `BeforeModelCallback` that injects memories into the LLM request

use std::sync::Arc;

use adk_core::{
    AdkError, BeforeModelCallback, BeforeModelResult, CallbackContext, Content, LlmRequest, Result,
    Tool, ToolContext,
};
use adk_memory::{MemoryService, SearchRequest};
use async_trait::async_trait;
use serde_json::{Value, json};

use super::config::MemoryToolConfig;
use super::format::{format_memory_results, format_memory_results_as_text};

/// A tool that loads relevant memories based on the current conversation context.
///
/// Can be used as a regular tool or converted into a [`BeforeModelCallback`]
/// via [`into_before_model_callback`](Self::into_before_model_callback) for
/// automatic turn-start execution.
///
/// # Example
///
/// ```rust,ignore
/// use adk_tool::memory::PreloadMemoryTool;
/// use adk_memory::InMemoryMemoryService;
/// use std::sync::Arc;
///
/// let service = Arc::new(InMemoryMemoryService::new());
/// let tool = PreloadMemoryTool::builder()
///     .memory_service(service)
///     .max_results(3)
///     .build()
///     .unwrap();
///
/// // Use as a before-model callback
/// let callback = tool.into_before_model_callback();
/// ```
pub struct PreloadMemoryTool {
    memory_service: Arc<dyn MemoryService>,
    config: MemoryToolConfig,
}

#[async_trait]
impl Tool for PreloadMemoryTool {
    fn name(&self) -> &str {
        "preload_memory"
    }

    fn description(&self) -> &str {
        "Load relevant memories based on the current conversation context. \
         Automatically retrieves memories related to the user's input."
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
                    "description": "Optional search query. If not provided, uses the user's latest input."
                }
            }
        }))
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        // Use explicit query from args, or fall back to user content
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| extract_text_from_content(ctx.user_content()));

        if query.is_empty() {
            return Ok(json!({"memories": [], "count": 0}));
        }

        let request = SearchRequest {
            query,
            user_id: ctx.user_id().to_string(),
            app_name: ctx.app_name().to_string(),
            limit: Some(self.config.max_results),
            min_score: self.config.min_relevance_score,
            project_id: self.config.project_id.clone(),
        };

        let response = self.memory_service.search(request).await?;
        Ok(format_memory_results(&response.memories))
    }
}

impl PreloadMemoryTool {
    /// Create a new builder for `PreloadMemoryTool`.
    pub fn builder() -> PreloadMemoryToolBuilder {
        PreloadMemoryToolBuilder::default()
    }

    /// Convert this tool into a [`BeforeModelCallback`] that injects memories
    /// into the `LlmRequest`'s system instruction before each model call.
    ///
    /// The callback extracts the user's latest message text from the request,
    /// searches memory, and appends relevant memories to the system content.
    pub fn into_before_model_callback(self) -> BeforeModelCallback {
        let tool = Arc::new(self);
        Box::new(move |ctx: Arc<dyn CallbackContext>, request: LlmRequest| {
            let tool = tool.clone();
            Box::pin(async move {
                // Extract user content text from the last user message
                let query = extract_user_query_from_request(&request);

                if query.is_empty() {
                    return Ok(BeforeModelResult::Continue(request));
                }

                let search_request = SearchRequest {
                    query,
                    user_id: ctx.user_id().to_string(),
                    app_name: ctx.app_name().to_string(),
                    limit: Some(tool.config.max_results),
                    min_score: tool.config.min_relevance_score,
                    project_id: tool.config.project_id.clone(),
                };

                let response = tool.memory_service.search(search_request).await?;

                if response.memories.is_empty() {
                    return Ok(BeforeModelResult::Continue(request));
                }

                // Inject memories into the request as a system-level content entry
                let memory_text = format_memory_results_as_text(&response.memories);
                let mut modified_request = request;

                // Find or create a system content entry and append memory context
                if let Some(system_content) =
                    modified_request.contents.iter_mut().find(|c| c.role == "system")
                {
                    system_content.parts.push(adk_core::Part::Text { text: memory_text });
                } else {
                    // Insert system content at the beginning
                    let system_content = Content::new("system").with_text(memory_text);
                    modified_request.contents.insert(0, system_content);
                }

                Ok(BeforeModelResult::Continue(modified_request))
            })
        })
    }
}

/// Extract text from the last user message in an LlmRequest.
fn extract_user_query_from_request(request: &LlmRequest) -> String {
    request
        .contents
        .iter()
        .rev()
        .find(|c| c.role == "user")
        .map(extract_text_from_content)
        .unwrap_or_default()
}

/// Extract text content from a `Content` value by concatenating all text parts.
fn extract_text_from_content(content: &Content) -> String {
    content.parts.iter().filter_map(|part| part.text()).collect::<Vec<_>>().join(" ")
}

/// Builder for [`PreloadMemoryTool`].
#[derive(Default)]
pub struct PreloadMemoryToolBuilder {
    memory_service: Option<Arc<dyn MemoryService>>,
    config: MemoryToolConfig,
}

impl PreloadMemoryToolBuilder {
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

    /// Build the `PreloadMemoryTool`, validating configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - `memory_service` is not configured
    /// - `max_results` is outside [1, 100]
    /// - `min_relevance_score` is outside [0.0, 1.0]
    pub fn build(self) -> Result<PreloadMemoryTool> {
        let memory_service = self
            .memory_service
            .ok_or_else(|| AdkError::tool("memory_service is required for PreloadMemoryTool"))?;

        self.config.validate()?;

        Ok(PreloadMemoryTool { memory_service, config: self.config })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::{Content, EventActions, ReadonlyContext};
    use adk_memory::{InMemoryMemoryService, MemoryEntry};
    use chrono::Utc;
    use std::collections::HashMap;
    use std::sync::Mutex;

    struct MockToolContext {
        content: Content,
        actions: Mutex<EventActions>,
    }

    impl MockToolContext {
        fn new(text: &str) -> Self {
            Self {
                content: Content::new("user").with_text(text),
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
        let tool = PreloadMemoryTool::builder().memory_service(service).build().unwrap();

        assert_eq!(tool.name(), "preload_memory");
        assert!(tool.is_read_only());
        assert!(tool.is_concurrency_safe());
        assert!(tool.parameters_schema().is_some());
    }

    #[test]
    fn test_builder_missing_service() {
        let result = PreloadMemoryTool::builder().build();
        assert!(result.is_err());
    }

    #[test]
    fn test_parameters_schema_structure() {
        let service = Arc::new(InMemoryMemoryService::new());
        let tool = PreloadMemoryTool::builder().memory_service(service).build().unwrap();

        let schema = tool.parameters_schema().unwrap();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["query"].is_object());
        assert_eq!(schema["properties"]["query"]["type"], "string");
        // query is optional for preload
        assert!(schema.get("required").is_none());
    }

    #[tokio::test]
    async fn test_execute_with_explicit_query() {
        let service = Arc::new(InMemoryMemoryService::new());

        let entry = MemoryEntry {
            content: Content::new("user").with_text("Paris is the capital of France"),
            author: "user".to_string(),
            timestamp: Utc::now(),
        };
        service.add_session("test-app", "user-1", "session-1", vec![entry]).await.unwrap();

        let tool = PreloadMemoryTool::builder().memory_service(service).build().unwrap();

        let ctx = Arc::new(MockToolContext::new("test")) as Arc<dyn ToolContext>;
        let args = json!({"query": "capital of France"});
        let result = tool.execute(ctx, args).await.unwrap();

        assert_eq!(result["count"], 1);
    }

    #[tokio::test]
    async fn test_execute_uses_user_content_as_fallback() {
        let service = Arc::new(InMemoryMemoryService::new());

        let entry = MemoryEntry {
            content: Content::new("user").with_text("I like Rust programming"),
            author: "user".to_string(),
            timestamp: Utc::now(),
        };
        service.add_session("test-app", "user-1", "session-1", vec![entry]).await.unwrap();

        let tool = PreloadMemoryTool::builder().memory_service(service).build().unwrap();

        let ctx = Arc::new(MockToolContext::new("Rust programming")) as Arc<dyn ToolContext>;
        let args = json!({});
        let result = tool.execute(ctx, args).await.unwrap();

        assert_eq!(result["count"], 1);
    }

    #[tokio::test]
    async fn test_before_model_callback() {
        let service = Arc::new(InMemoryMemoryService::new());

        let entry = MemoryEntry {
            content: Content::new("user").with_text("The user prefers dark mode"),
            author: "assistant".to_string(),
            timestamp: Utc::now(),
        };
        service.add_session("test-app", "user-1", "session-1", vec![entry]).await.unwrap();

        let tool = PreloadMemoryTool::builder().memory_service(service).build().unwrap();

        let callback = tool.into_before_model_callback();

        let ctx = Arc::new(MockToolContext::new("dark mode")) as Arc<dyn CallbackContext>;

        let request = LlmRequest {
            model: "test-model".to_string(),
            contents: vec![Content::new("user").with_text("What about dark mode?")],
            config: None,
            tools: adk_core::ToolDeclarations::new(),
            previous_response_id: None,
        };

        let result = callback(ctx, request).await.unwrap();
        match result {
            BeforeModelResult::Continue(modified_request) => {
                // Should have injected memory content
                let has_memory_content = modified_request.contents.iter().any(|c| {
                    c.parts
                        .iter()
                        .any(|p| p.text().map(|t| t.contains("Relevant Memories")).unwrap_or(false))
                });
                assert!(has_memory_content);

                // Original user message should still be present
                let has_user_msg = modified_request.contents.iter().any(|c| c.role == "user");
                assert!(has_user_msg);
            }
            BeforeModelResult::Skip(_) => panic!("Expected Continue, got Skip"),
        }
    }

    #[tokio::test]
    async fn test_before_model_callback_no_results() {
        let service = Arc::new(InMemoryMemoryService::new());
        let tool = PreloadMemoryTool::builder().memory_service(service).build().unwrap();

        let callback = tool.into_before_model_callback();

        let ctx = Arc::new(MockToolContext::new("something")) as Arc<dyn CallbackContext>;

        let request = LlmRequest {
            model: "test-model".to_string(),
            contents: vec![Content::new("user").with_text("hello")],
            config: None,
            tools: adk_core::ToolDeclarations::new(),
            previous_response_id: None,
        };

        let result = callback(ctx, request.clone()).await.unwrap();
        match result {
            BeforeModelResult::Continue(modified_request) => {
                // No memories found, request should be unchanged
                assert_eq!(modified_request.contents.len(), request.contents.len());
            }
            BeforeModelResult::Skip(_) => panic!("Expected Continue, got Skip"),
        }
    }
}
