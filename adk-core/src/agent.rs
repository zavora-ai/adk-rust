use crate::{InvocationContext, Result, event::Event};
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

/// A validated context containing engineered instructions and resolved tool instances.
///
/// This structure serves as the "Atomic Unit of Capability" for an agent. It guarantees
/// that the agent's cognitive frame (the instructions telling it what it can do) is
/// perfectly aligned with its physical capabilities (the binary tool instances bound
/// to the session).
///
/// By using `ResolvedContext`, the framework eliminates "Phantom Tool" hallucinations,
/// where an agent tries to call a tool that was mentioned in its prompt but never
/// actually registered in the runtime.
#[derive(Clone)]
pub struct ResolvedContext {
    /// The engineered system instruction.
    pub system_instruction: String,
    /// The resolved, executable tools.
    pub active_tools: Vec<Arc<dyn crate::Tool>>,
}

impl std::fmt::Debug for ResolvedContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedContext")
            .field("system_instruction_len", &self.system_instruction.len())
            .field("active_tools_count", &self.active_tools.len())
            .finish()
    }
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
        fn id(&self) -> &crate::types::SessionId {
            static ID: std::sync::OnceLock<crate::types::SessionId> = std::sync::OnceLock::new();
            ID.get_or_init(|| crate::types::SessionId::new("session").unwrap())
        }
        fn app_name(&self) -> &str {
            "app"
        }
        fn user_id(&self) -> &crate::types::UserId {
            static ID: std::sync::OnceLock<crate::types::UserId> = std::sync::OnceLock::new();
            ID.get_or_init(|| crate::types::UserId::new("user").unwrap())
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
                content: Content::new(crate::types::Role::User),
                config: RunConfig::default(),
                session: MockSession,
            }
        }
    }

    #[async_trait]
    impl ReadonlyContext for TestContext {
        fn identity(&self) -> &crate::types::AdkIdentity {
            static IDENTITY: std::sync::OnceLock<crate::types::AdkIdentity> =
                std::sync::OnceLock::new();
            IDENTITY.get_or_init(|| crate::types::AdkIdentity {
                agent_name: "test".to_string(),
                app_name: "app".to_string(),
                ..Default::default()
            })
        }
        fn user_content(&self) -> &Content {
            &self.content
        }
        fn metadata(&self) -> &std::collections::HashMap<String, String> {
            static METADATA: std::sync::OnceLock<std::collections::HashMap<String, String>> =
                std::sync::OnceLock::new();
            METADATA.get_or_init(std::collections::HashMap::new)
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
                let id = crate::types::InvocationId::new("test").unwrap();
                yield Ok(Event::new(id));
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
