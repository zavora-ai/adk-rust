use adk_agent::LlmAgentBuilder;
use adk_core::model::{Llm, LlmRequest, LlmResponse, LlmResponseStream};
use adk_core::types::{Content, InvocationId, Part, Role, SessionId, UserId};
use adk_core::{Agent, InvocationContext, ReadonlyContext, RunConfig, Session, State};
use async_trait::async_trait;
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

struct MockLlm {
    response_text: String,
}

#[async_trait]
impl Llm for MockLlm {
    fn name(&self) -> &str {
        "mock"
    }
    async fn generate_content(&self, _req: LlmRequest, _stream: bool) -> adk_core::Result<LlmResponseStream> {
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

struct MockSession;
impl Session for MockSession {
    fn id(&self) -> &SessionId { unimplemented!() }
    fn app_name(&self) -> &str { "test" }
    fn user_id(&self) -> &UserId { unimplemented!() }
    fn state(&self) -> &dyn State { unimplemented!() }
    fn conversation_history(&self) -> Vec<Content> { Vec::new() }
}

struct MockContext {
    identity: adk_core::types::AdkIdentity,
    content: Content,
}

impl MockContext {
    fn new() -> Self {
        Self {
            identity: adk_core::types::AdkIdentity::default(),
            content: Content::new(Role::User).with_text("hi"),
        }
    }
}

#[async_trait]
impl ReadonlyContext for MockContext {
    fn identity(&self) -> &adk_core::types::AdkIdentity { &self.identity }
    fn user_content(&self) -> &Content { &self.content }
    fn metadata(&self) -> &HashMap<String, String> { 
        static EMPTY: std::sync::OnceLock<HashMap<String, String>> = std::sync::OnceLock::new();
        EMPTY.get_or_init(HashMap::new)
    }
}

#[async_trait]
impl adk_core::CallbackContext for MockContext {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> { None }
}

#[async_trait]
impl InvocationContext for MockContext {
    fn agent(&self) -> Arc<dyn Agent> { unimplemented!() }
    fn memory(&self) -> Option<Arc<dyn adk_core::Memory>> { None }
    fn session(&self) -> &dyn Session { &MockSession }
    fn run_config(&self) -> &RunConfig { 
        static DEFAULT: std::sync::OnceLock<RunConfig> = std::sync::OnceLock::new();
        DEFAULT.get_or_init(RunConfig::default)
    }
    fn end_invocation(&self) {}
    fn ended(&self) -> bool { false }
}

#[tokio::test]
async fn test_agent_before_callback_mutation() {
    let llm = Arc::new(MockLlm { response_text: "ok".to_string() });
    
    let agent = LlmAgentBuilder::new("test")
        .model(llm)
        .before_callback(Box::new(|_ctx| {
            Box::pin(async move {
                Ok(Some(Content::new(Role::System).with_text("prefix: ")))
            })
        }))
        .build()
        .unwrap();

    let ctx = Arc::new(MockContext::new());
    let mut stream = agent.run(ctx).await.unwrap();
    let event = stream.next().await.unwrap().unwrap();
    
    assert_eq!(event.author, "test");
}

#[tokio::test]
async fn test_agent_after_callback_observation() {
    let called = Arc::new(Mutex::new(false));
    let called_clone = called.clone();
    
    let llm = Arc::new(MockLlm { response_text: "ok".to_string() });
    let agent = LlmAgentBuilder::new("test")
        .model(llm)
        .after_callback(Box::new(move |_ctx| {
            let called = called_clone.clone();
            Box::pin(async move {
                *called.lock().unwrap() = true;
                Ok(None)
            })
        }))
        .build()
        .unwrap();

    let ctx = Arc::new(MockContext::new());
    let mut stream = agent.run(ctx).await.unwrap();
    while let Some(res) = stream.next().await {
        let _ = res.unwrap();
    }
    
    assert!(*called.lock().unwrap());
}
