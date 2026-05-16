//! Test helpers for the server module.

use std::sync::Arc;

use adk_core::{Agent, Content, EventStream, InvocationContext, Result as AdkResult};
use adk_session::{InMemorySessionService, SessionService};
use async_trait::async_trait;

/// A minimal mock agent for testing.
pub(crate) struct MockAgent {
    name: String,
}

impl MockAgent {
    pub fn new(name: &str) -> Self {
        Self { name: name.to_string() }
    }
}

#[async_trait]
impl Agent for MockAgent {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        "A mock agent for testing"
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, _ctx: Arc<dyn InvocationContext>) -> AdkResult<EventStream> {
        use adk_core::Event;
        use async_stream::stream;

        let s = stream! {
            let mut event = Event::new("mock-invocation");
            event.set_content(Content::new("model").with_text("mock response"));
            yield Ok(event);
        };
        Ok(Box::pin(s))
    }
}

/// Create a mock agent and in-memory session service for testing.
pub(crate) fn mock_agent_and_session() -> (Arc<dyn Agent>, Arc<dyn SessionService>) {
    let agent: Arc<dyn Agent> = Arc::new(MockAgent::new("test-agent"));
    let session_svc: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    (agent, session_svc)
}
