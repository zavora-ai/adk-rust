use adk_agent::LlmAgentBuilder;
use adk_core::model::{Llm, LlmRequest, LlmResponse, LlmResponseStream};
use adk_core::types::{Content, InvocationId, Part, Role, SessionId, UserId};
use adk_core::{Result, RunConfig};
use adk_session::{Session, State};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

struct StreamingMockLlm {
    chunks: Vec<String>,
}

#[async_trait]
impl Llm for StreamingMockLlm {
    fn name(&self) -> &str {
        "streaming-mock"
    }

    async fn generate_content(&self, _req: LlmRequest, stream: bool) -> Result<LlmResponseStream> {
        let chunks = self.chunks.clone();
        let s = async_stream::stream! {
            if stream {
                for chunk in chunks {
                    yield Ok(LlmResponse {
                        content: Some(Content::new(Role::Model).with_text(chunk)),
                        partial: true,
                        ..Default::default()
                    });
                }
                yield Ok(LlmResponse {
                    partial: false,
                    turn_complete: true,
                    ..Default::default()
                });
            } else {
                yield Ok(LlmResponse {
                    content: Some(Content::new(Role::Model).with_text(chunks.join(""))),
                    turn_complete: true,
                    ..Default::default()
                });
            }
        };
        Ok(Box::pin(s))
    }
}

struct MockSession {
    session_id: SessionId,
    user_id: UserId,
}

impl Session for MockSession {
    fn id(&self) -> &SessionId {
        &self.session_id
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
    fn conversation_history(&self) -> Vec<Content> {
        Vec::new()
    }
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
}

impl MockContext {
    fn new() -> Self {
        let mut identity = adk_core::types::AdkIdentity::default();
        identity.invocation_id = "inv-1".into();
        identity.user_id = "user-1".into();
        identity.session_id = "session-1".into();

        Self {
            identity,
            session: MockSession {
                session_id: SessionId::new("session-1").unwrap(),
                user_id: UserId::new("user-1").unwrap(),
            },
        }
    }
}

#[async_trait]
impl adk_agent::InvocationContext for MockContext {
    fn invocation_id(&self) -> &InvocationId { &self.identity.invocation_id }
    fn user_content(&self) -> &Content { 
        static EMPTY: std::sync::OnceLock<Content> = std::sync::OnceLock::new();
        EMPTY.get_or_init(|| Content::new(Role::User).with_text("hi"))
    }
    fn identity(&self) -> &adk_core::types::AdkIdentity { &self.identity }
    fn session(&self) -> &dyn Session { &self.session }
    fn metadata(&self) -> &HashMap<String, String> {
        static EMPTY: std::sync::OnceLock<HashMap<String, String>> = std::sync::OnceLock::new();
        EMPTY.get_or_init(HashMap::new)
    }
}

#[tokio::test]
async fn test_llm_agent_streaming_accumulation() {
    let chunks = vec!["Hello ".to_string(), "world".to_string(), "!".to_string()];
    let model = Arc::new(StreamingMockLlm { chunks });
    let agent = LlmAgentBuilder::new("test").model(model).build().unwrap();

    let ctx = Arc::new(MockContext::new());
    let mut stream = agent.run(ctx).await.unwrap();

    let mut partials = 0;
    let mut final_text = String::new();

    while let Some(result) = stream.next().await {
        let event = result.unwrap();
        if event.llm_response.partial {
            partials += 1;
            if let Some(content) = event.content() {
                final_text.push_str(&content.text());
            }
        }
    }

    assert_eq!(partials, 3);
    assert_eq!(final_text, "Hello world!");
}
