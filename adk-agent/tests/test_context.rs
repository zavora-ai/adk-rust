use adk_core::{
    Agent, Content, InvocationContext, Part, ReadonlyContext, RunConfig, types::AdkIdentity,
};
use async_trait::async_trait;
use std::sync::Arc;

pub struct TestContext {
    identity: AdkIdentity,
    content: Content,
    config: RunConfig,
    metadata: std::collections::HashMap<String, String>,
    session: DummySession,
}

impl TestContext {
    pub fn new(message: &str) -> Self {
        Self {
            identity: AdkIdentity::default(),
            content: Content {
                role: adk_core::types::Role::User,
                parts: vec![Part::text(message.to_string())],
            },
            config: RunConfig::default(),
            metadata: std::collections::HashMap::new(),
            session: DummySession::new(),
        }
    }
}

#[async_trait]
impl ReadonlyContext for TestContext {
    fn identity(&self) -> &AdkIdentity {
        &self.identity
    }

    fn user_content(&self) -> &Content {
        &self.content
    }

    fn metadata(&self) -> &std::collections::HashMap<String, String> {
        &self.metadata
    }
}

#[async_trait]
impl adk_core::CallbackContext for TestContext {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}

#[async_trait]
impl InvocationContext for TestContext {
    fn agent(&self) -> Arc<dyn Agent> {
        unimplemented!()
    }
    fn memory(&self) -> Option<Arc<dyn adk_core::Memory>> {
        None
    }
    fn run_config(&self) -> &RunConfig {
        &self.config
    }
    fn end_invocation(&self) {}
    fn ended(&self) -> bool {
        false
    }
    fn session(&self) -> &dyn adk_core::Session {
        &self.session
    }
}

// Dummy session for testing
use adk_core::types::{SessionId, UserId};

struct DummySession {
    id: SessionId,
    user_id: UserId,
}

impl DummySession {
    fn new() -> Self {
        Self {
            id: SessionId::new("test-session".to_string()).unwrap(),
            user_id: UserId::new("test-user".to_string()).unwrap(),
        }
    }
}

impl adk_core::Session for DummySession {
    fn id(&self) -> &SessionId {
        &self.id
    }
    fn app_name(&self) -> &str {
        "test-app"
    }
    fn user_id(&self) -> &UserId {
        &self.user_id
    }
    fn state(&self) -> &dyn adk_core::State {
        &DummyState
    }
    fn conversation_history(&self) -> Vec<adk_core::Content> {
        Vec::new()
    }
}

struct DummyState;

impl adk_core::State for DummyState {
    fn get(&self, _key: &str) -> Option<serde_json::Value> {
        None
    }
    fn set(&mut self, _key: String, _value: serde_json::Value) {}
    fn all(&self) -> std::collections::HashMap<String, serde_json::Value> {
        std::collections::HashMap::new()
    }
}
