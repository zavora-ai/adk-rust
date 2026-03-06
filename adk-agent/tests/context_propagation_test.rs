use adk_agent::ToolContext;
use adk_core::model::{Llm, LlmRequest, LlmResponse, LlmResponseStream};
use adk_core::types::{Content, InvocationId, Part, Role, SessionId, UserId};
use adk_core::{Result, Tool};
use adk_session::{Session, State};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

// Mock LLM for testing
struct MockLlm {
    response: LlmResponse,
}

#[async_trait]
impl Llm for MockLlm {
    fn name(&self) -> &str {
        "mock"
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
    captured_user_id: Arc<Mutex<Option<UserId>>>,
    captured_session_id: Arc<Mutex<Option<SessionId>>>,
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
        // Capture context using typed IDs
        *self.captured_user_id.lock().unwrap() = Some(ctx.user_id().clone());
        *self.captured_session_id.lock().unwrap() = Some(ctx.session_id().clone());

        Ok(json!({ "status": "ok" }))
    }
}

struct MockSession {
    id: SessionId,
    user_id: UserId,
}

impl MockSession {
    fn new() -> Self {
        Self {
            id: SessionId::new("session-456").unwrap(),
            user_id: UserId::new("user-123").unwrap(),
        }
    }
}

impl Session for MockSession {
    fn id(&self) -> &SessionId {
        &self.id
    }
    fn app_name(&self) -> &str {
        "test-app"
    }
    fn user_id(&self) -> &UserId {
        &self.user_id
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
    identity: adk_core::types::AdkIdentity,
    session: MockSession,
    user_content: Content,
}

impl MockContext {
    fn new() -> Self {
        let mut identity = adk_core::types::AdkIdentity::default();
        identity.invocation_id = "inv-1".into();
        identity.agent_name = "test-agent".to_string();
        identity.user_id = "user-123".into();
        identity.app_name = "test-app".to_string();
        identity.session_id = "session-456".into();
        identity.branch = "main".to_string();

        Self {
            identity,
            session: MockSession::new(),
            user_content: Content::new(Role::User).with_text("run tool"),
        }
    }
}

#[async_trait]
impl adk_agent::InvocationContext for MockContext {
    fn invocation_id(&self) -> &InvocationId {
        &self.identity.invocation_id
    }
    fn user_content(&self) -> &Content {
        &self.user_content
    }
    fn identity(&self) -> &adk_core::types::AdkIdentity {
        &self.identity
    }
    fn session(&self) -> &dyn Session {
        &self.session
    }
    fn metadata(&self) -> &HashMap<String, String> {
        static EMPTY: std::sync::OnceLock<HashMap<String, String>> = std::sync::OnceLock::new();
        EMPTY.get_or_init(HashMap::new)
    }
}

#[tokio::test]
async fn test_tool_context_propagation() {
    let tool = Arc::new(MockTool::new());
    let tool_registry = vec![tool.clone() as Arc<dyn Tool>];

    let llm_response = LlmResponse {
        content: Some(Content::new(Role::Model).with_part(Part::FunctionCall {
            name: "test_tool".to_string(),
            args: json!({}),
            id: Some("call-1".to_string()),
            thought_signature: None,
        })),
        ..Default::default()
    };

    let llm = Arc::new(MockLlm { response: llm_response });
    let agent = adk_agent::LlmAgent::new("test-agent", llm, tool_registry);

    let ctx = Arc::new(MockContext::new());
    let mut stream = agent.run(ctx).await.unwrap();

    // Run until tool call happens
    while let Some(result) = stream.next().await {
        let _ = result.unwrap();
    }

    // Verify captured IDs are typed
    assert_eq!(
        *tool.captured_user_id.lock().unwrap(),
        Some(UserId::new("user-123").unwrap())
    );
    assert_eq!(
        *tool.captured_session_id.lock().unwrap(),
        Some(SessionId::new("session-456").unwrap())
    );
}
