use adk_agent::LlmAgentBuilder;
use adk_core::{
    Agent, Content, FinishReason, InvocationContext, Llm, LlmRequest, LlmResponse,
    LlmResponseStream, Part, Result, RunConfig, Session, State, Tool, ToolContext,
};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

// --- Mocks ---

struct MockModel {
    response: LlmResponse,
}

impl MockModel {
    fn new_function_call(name: &str, args: Value) -> Self {
        let content = Content {
            role: "model".to_string(),
            parts: vec![Part::FunctionCall {
                name: name.to_string(),
                args,
                id: Some(format!("call_{}", name)),
                thought_signature: None,
            }],
        };

        Self {
            response: LlmResponse {
                content: Some(content),
                usage_metadata: None,
                finish_reason: Some(FinishReason::Stop),
                citation_metadata: None,
                partial: false,
                turn_complete: true,
                interrupted: false,
                error_code: None,
                error_message: None,
            },
        }
    }
}

#[async_trait]
impl Llm for MockModel {
    fn name(&self) -> &str {
        "mock-model"
    }

    async fn generate_content(&self, _req: LlmRequest, _stream: bool) -> Result<LlmResponseStream> {
        let response = self.response.clone();
        let s = async_stream::stream! {
            yield Ok(response);
        };
        Ok(Box::pin(s))
    }
}

struct MockTool {
    captured_user_id: Arc<Mutex<Option<String>>>,
    captured_session_id: Arc<Mutex<Option<String>>>,
}

impl MockTool {
    fn new() -> Self {
        Self {
            captured_user_id: Arc::new(Mutex::new(None)),
            captured_session_id: Arc::new(Mutex::new(None)),
        }
    }
}

#[async_trait]
impl Tool for MockTool {
    fn name(&self) -> &str {
        "test_tool"
    }
    fn description(&self) -> &str {
        "Test tool"
    }
    fn parameters_schema(&self) -> Option<Value> {
        None
    }
    fn response_schema(&self) -> Option<Value> {
        None
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        // Capture context
        *self.captured_user_id.lock().unwrap() = Some(ctx.user_id().to_string());
        *self.captured_session_id.lock().unwrap() = Some(ctx.session_id().to_string());

        Ok(json!({ "status": "ok" }))
    }
}

struct MockSession;
impl Session for MockSession {
    fn id(&self) -> &str {
        "session-456"
    }
    fn app_name(&self) -> &str {
        "test-app"
    }
    fn user_id(&self) -> &str {
        "user-123"
    }
    fn state(&self) -> &dyn State {
        &MockState
    }
    fn conversation_history(&self) -> Vec<adk_core::Content> {
        Vec::new()
    }
}

struct MockState;
impl State for MockState {
    fn get(&self, _key: &str) -> Option<Value> {
        None
    }
    fn set(&mut self, _key: String, _value: Value) {}
    fn all(&self) -> HashMap<String, Value> {
        HashMap::new()
    }
}

struct MockContext {
    session: MockSession,
    user_content: Content,
}

impl MockContext {
    fn new() -> Self {
        Self {
            session: MockSession,
            user_content: Content {
                role: "user".to_string(),
                parts: vec![Part::Text { text: "call tool".to_string() }],
            },
        }
    }
}

#[async_trait]
impl adk_core::ReadonlyContext for MockContext {
    fn invocation_id(&self) -> &str {
        "inv-1"
    }
    fn agent_name(&self) -> &str {
        "test-agent"
    }
    fn user_id(&self) -> &str {
        "user-123"
    }
    fn app_name(&self) -> &str {
        "test-app"
    }
    fn session_id(&self) -> &str {
        "session-456"
    }
    fn branch(&self) -> &str {
        "main"
    }
    fn user_content(&self) -> &Content {
        &self.user_content
    }
}

#[async_trait]
impl adk_core::CallbackContext for MockContext {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}

#[async_trait]
impl InvocationContext for MockContext {
    fn agent(&self) -> Arc<dyn Agent> {
        unimplemented!()
    }
    fn memory(&self) -> Option<Arc<dyn adk_core::Memory>> {
        None
    }
    fn session(&self) -> &dyn Session {
        &self.session
    }
    fn run_config(&self) -> &RunConfig {
        static RUN_CONFIG: std::sync::OnceLock<RunConfig> = std::sync::OnceLock::new();
        RUN_CONFIG.get_or_init(RunConfig::default)
    }
    fn end_invocation(&self) {}
    fn ended(&self) -> bool {
        false
    }
}

// --- Tests ---

#[tokio::test]
async fn test_tool_context_propagation() {
    let tool = Arc::new(MockTool::new());
    let model = Arc::new(MockModel::new_function_call("test_tool", json!({})));

    let agent = LlmAgentBuilder::new("test-agent").model(model).tool(tool.clone()).build().unwrap();

    let ctx = Arc::new(MockContext::new());

    let mut stream = agent.run(ctx).await.unwrap();

    // Consume stream to trigger tool execution
    while (stream.next().await).is_some() {}

    // Verify captured context
    let captured_user = tool.captured_user_id.lock().unwrap().clone();
    let captured_session = tool.captured_session_id.lock().unwrap().clone();

    assert_eq!(captured_user, Some("user-123".to_string()));
    assert_eq!(captured_session, Some("session-456".to_string()));
}
