use crate::{CallbackContext, EventActions, MemoryEntry, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;

    /// Returns an enhanced description that may include additional notes.
    /// For long-running tools, this includes a warning not to call the tool
    /// again if it has already returned a pending status.
    /// Default implementation returns the base description.
    fn enhanced_description(&self) -> String {
        self.description().to_string()
    }

    /// Indicates whether the tool is a long-running operation.
    /// Long-running tools typically return a task ID immediately and
    /// complete the operation asynchronously.
    fn is_long_running(&self) -> bool {
        false
    }
    fn parameters_schema(&self) -> Option<Value> {
        None
    }
    fn response_schema(&self) -> Option<Value> {
        None
    }
    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value>;
}

#[async_trait]
pub trait ToolContext: CallbackContext {
    fn function_call_id(&self) -> &str;
    fn actions(&self) -> &EventActions;
    async fn search_memory(&self, query: &str) -> Result<Vec<MemoryEntry>>;
}

#[async_trait]
pub trait Toolset: Send + Sync {
    fn name(&self) -> &str;
    async fn tools(&self, ctx: Arc<dyn crate::ReadonlyContext>) -> Result<Vec<Arc<dyn Tool>>>;
}

pub type ToolPredicate = Box<dyn Fn(&dyn Tool) -> bool + Send + Sync>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Content, EventActions, ReadonlyContext, RunConfig};

    struct TestTool {
        name: String,
    }

    #[allow(dead_code)]
    struct TestContext {
        content: Content,
        config: RunConfig,
        actions: EventActions,
    }

    impl TestContext {
        fn new() -> Self {
            Self {
                content: Content::new("user"),
                config: RunConfig::default(),
                actions: EventActions::default(),
            }
        }
    }

    #[async_trait]
    impl ReadonlyContext for TestContext {
        fn invocation_id(&self) -> &str {
            "test"
        }
        fn agent_name(&self) -> &str {
            "test"
        }
        fn user_id(&self) -> &str {
            "user"
        }
        fn app_name(&self) -> &str {
            "app"
        }
        fn session_id(&self) -> &str {
            "session"
        }
        fn branch(&self) -> &str {
            ""
        }
        fn user_content(&self) -> &Content {
            &self.content
        }
    }

    #[async_trait]
    impl CallbackContext for TestContext {
        fn artifacts(&self) -> Option<Arc<dyn crate::Artifacts>> {
            None
        }
    }

    #[async_trait]
    impl ToolContext for TestContext {
        fn function_call_id(&self) -> &str {
            "call-123"
        }
        fn actions(&self) -> &EventActions {
            &self.actions
        }
        async fn search_memory(&self, _query: &str) -> Result<Vec<crate::MemoryEntry>> {
            Ok(vec![])
        }
    }

    #[async_trait]
    impl Tool for TestTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            "test tool"
        }

        async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
            Ok(Value::String("result".to_string()))
        }
    }

    #[test]
    fn test_tool_trait() {
        let tool = TestTool { name: "test".to_string() };
        assert_eq!(tool.name(), "test");
        assert_eq!(tool.description(), "test tool");
        assert!(!tool.is_long_running());
    }

    #[tokio::test]
    async fn test_tool_execute() {
        let tool = TestTool { name: "test".to_string() };
        let ctx = Arc::new(TestContext::new()) as Arc<dyn ToolContext>;
        let result = tool.execute(ctx, Value::Null).await.unwrap();
        assert_eq!(result, Value::String("result".to_string()));
    }
}
