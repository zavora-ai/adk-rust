use crate::{event::Event, InvocationContext, Result};
use async_trait::async_trait;
use futures::stream::Stream;
use std::pin::Pin;
use std::sync::Arc;

pub type EventStream = Pin<Box<dyn Stream<Item = Result<Event>> + Send>>;

#[async_trait]
pub trait Agent: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn sub_agents(&self) -> &[Arc<dyn Agent>];

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Content, ReadonlyContext, RunConfig};
    use async_stream::stream;

    struct TestAgent {
        name: String,
    }

    use crate::{CallbackContext, Session, State};
    use std::collections::HashMap;

    struct MockState;
    impl State for MockState {
        fn get(&self, _key: &str) -> Option<serde_json::Value> {
            None
        }
        fn set(&mut self, _key: String, _value: serde_json::Value) {}
        fn all(&self) -> HashMap<String, serde_json::Value> {
            HashMap::new()
        }
    }

    struct MockSession;
    impl Session for MockSession {
        fn id(&self) -> &str {
            "session"
        }
        fn app_name(&self) -> &str {
            "app"
        }
        fn user_id(&self) -> &str {
            "user"
        }
        fn state(&self) -> &dyn State {
            &MockState
        }
        fn conversation_history(&self) -> Vec<Content> {
            Vec::new()
        }
    }

    #[allow(dead_code)]
    struct TestContext {
        content: Content,
        config: RunConfig,
        session: MockSession,
    }

    #[allow(dead_code)]
    impl TestContext {
        fn new() -> Self {
            Self {
                content: Content::new("user"),
                config: RunConfig::default(),
                session: MockSession,
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
    impl InvocationContext for TestContext {
        fn agent(&self) -> Arc<dyn Agent> {
            unimplemented!()
        }
        fn memory(&self) -> Option<Arc<dyn crate::Memory>> {
            None
        }
        fn session(&self) -> &dyn Session {
            &self.session
        }
        fn run_config(&self) -> &RunConfig {
            &self.config
        }
        fn end_invocation(&self) {}
        fn ended(&self) -> bool {
            false
        }
    }

    #[async_trait]
    impl Agent for TestAgent {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            "test agent"
        }

        fn sub_agents(&self) -> &[Arc<dyn Agent>] {
            &[]
        }

        async fn run(&self, _ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
            let s = stream! {
                yield Ok(Event::new("test"));
            };
            Ok(Box::pin(s))
        }
    }

    #[test]
    fn test_agent_trait() {
        let agent = TestAgent { name: "test".to_string() };
        assert_eq!(agent.name(), "test");
        assert_eq!(agent.description(), "test agent");
    }
}
