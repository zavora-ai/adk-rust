use adk_agent::LlmAgentBuilder;
use adk_core::model::{Llm, LlmRequest, LlmResponse, LlmResponseStream};
use adk_core::types::{Content, InvocationId, Part, Role, SessionId, UserId};
use adk_core::{Agent, InvocationContext, Result, RunConfig};
use adk_session::{Session, State};
use async_trait::async_trait;
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;

struct MockLlm {
    response_text: String,
}

#[async_trait]
impl Llm for MockLlm {
    fn name(&self) -> &str {
        "mock"
    }
    async fn generate_content(&self, _req: LlmRequest, _stream: bool) -> Result<LlmResponseStream> {
        let text = self.response_text.clone();
        let s = async_stream::stream! {
            yield Ok(LlmResponse {
                content: Some(Content::new(Role::Model).with_text(text)),
                ..Default::default()
            });
        };
        Ok(Box::pin(s))
    }
}

struct MockSession {
    id: SessionId,
    user_id: UserId,
}

impl MockSession {
    fn new() -> Self {
        Self {
            id: SessionId::new("session-real").unwrap(),
            user_id: UserId::new("user-real").unwrap(),
        }
    }
}

impl Session for MockSession {
    fn id(&self) -> &SessionId { &self.id }
    fn app_name(&self) -> &str { "test-app" }
    fn user_id(&self) -> &UserId { &self.user_id }
    fn state(&self) -> &dyn State { &MockState }
    fn conversation_history(&self) -> Vec<Content> { Vec::new() }
}

struct MockState;
impl State for MockState {
    fn get(&self, _key: &str) -> Option<serde_json::Value> { None }
    fn set(&mut self, _key: String, _value: serde_json::Value) {}
    fn all(&self) -> HashMap<String, serde_json::Value> { HashMap::new() }
}

struct MockContext {
    identity: adk_core::types::AdkIdentity,
    session: MockSession,
    user_content: Content,
}

impl MockContext {
    fn new(text: &str) -> Self {
        let mut identity = adk_core::types::AdkIdentity::default();
        identity.invocation_id = "inv-real".into();
        identity.agent_name = "gemini-agent".to_string();
        identity.user_id = "user-real".into();
        identity.app_name = "test-app".to_string();
        identity.session_id = "session-real".into();

        Self {
            identity,
            session: MockSession::new(),
            user_content: Content::new(Role::User).with_text(text.to_string()),
        }
    }
}

#[async_trait]
impl adk_agent::InvocationContext for MockContext {
    fn invocation_id(&self) -> &InvocationId { &self.identity.invocation_id }
    fn user_content(&self) -> &Content { &self.user_content }
    fn identity(&self) -> &adk_core::types::AdkIdentity { &self.identity }
    fn session(&self) -> &dyn Session { &self.session }
    fn metadata(&self) -> &HashMap<String, String> {
        static EMPTY: std::sync::OnceLock<HashMap<String, String>> = std::sync::OnceLock::new();
        EMPTY.get_or_init(HashMap::new)
    }
}

#[tokio::test]
async fn test_llm_agent_real_flow_simulation() {
    let model = Arc::new(MockLlm { response_text: "Final answer".to_string() });
    let agent = LlmAgentBuilder::new("gemini-agent").model(model).build().unwrap();

    let ctx = Arc::new(MockContext::new("What is the weather?"));
    let mut stream = agent.run(ctx).await.unwrap();

    let mut final_event = None;
    while let Some(res) = stream.next().await {
        final_event = Some(res.unwrap());
    }

    let event = final_event.unwrap();
    assert_eq!(event.author, "gemini-agent");
    assert_eq!(event.content().unwrap().text(), "Final answer");
}
