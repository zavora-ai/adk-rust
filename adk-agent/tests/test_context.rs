use adk_core::{Content, InvocationContext, Part, ReadonlyContext, RunConfig, Agent};
use async_trait::async_trait;
use std::sync::Arc;

pub struct TestContext {
    content: Content,
    config: RunConfig,
}

impl TestContext {
    pub fn new(message: &str) -> Self {
        Self {
            content: Content {
                role: "user".to_string(),
                parts: vec![Part::Text {
                    text: message.to_string(),
                }],
            },
            config: RunConfig::default(),
        }
    }
}

#[async_trait]
impl ReadonlyContext for TestContext {
    fn invocation_id(&self) -> &str { "test-inv" }
    fn agent_name(&self) -> &str { "test-agent" }
    fn user_id(&self) -> &str { "test-user" }
    fn app_name(&self) -> &str { "test-app" }
    fn session_id(&self) -> &str { "test-session" }
    fn branch(&self) -> &str { "" }
    fn user_content(&self) -> &Content { &self.content }
}

#[async_trait]
impl adk_core::CallbackContext for TestContext {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> { None }
}

#[async_trait]
impl InvocationContext for TestContext {
    fn agent(&self) -> Arc<dyn Agent> { unimplemented!() }
    fn memory(&self) -> Option<Arc<dyn adk_core::Memory>> { None }
    fn run_config(&self) -> &RunConfig { &self.config }
    fn end_invocation(&self) {}
    fn ended(&self) -> bool { false }
    fn session(&self) -> &dyn adk_core::Session { &DummySession }
}

// Dummy session for testing
struct DummySession;

impl adk_core::Session for DummySession {
    fn id(&self) -> &str { "test-session" }
    fn app_name(&self) -> &str { "test-app" }
    fn user_id(&self) -> &str { "test-user" }
    fn state(&self) -> &dyn adk_core::State { &DummyState }
}

struct DummyState;

impl adk_core::State for DummyState {
    fn get(&self, _key: &str) -> Option<serde_json::Value> { None }
    fn set(&mut self, _key: String, _value: serde_json::Value) {}
    fn all(&self) -> std::collections::HashMap<String, serde_json::Value> {
        std::collections::HashMap::new()
    }
}
