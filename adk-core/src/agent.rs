use crate::{event::Event, Result};
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

#[async_trait]
pub trait InvocationContext: Send + Sync {
    fn invocation_id(&self) -> &str;
    fn user_id(&self) -> &str;
    fn session_id(&self) -> &str;
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_stream::stream;

    struct TestAgent {
        name: String,
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
        let agent = TestAgent {
            name: "test".to_string(),
        };
        assert_eq!(agent.name(), "test");
        assert_eq!(agent.description(), "test agent");
    }
}
