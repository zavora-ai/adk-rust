use adk_agent::CustomAgent;
use adk_core::{Agent, Content, Event, InvocationContext, ReadonlyContext, Result, RunConfig};
use async_trait::async_trait;
use futures::StreamExt;
use std::sync::Arc;

struct MockContext {
    content: Content,
    config: RunConfig,
}

impl MockContext {
    fn new() -> Self {
        Self {
            content: Content::new("user").with_text("test"),
            config: RunConfig::default(),
        }
    }
}

#[async_trait]
impl ReadonlyContext for MockContext {
    fn invocation_id(&self) -> &str { "inv-1" }
    fn agent_name(&self) -> &str { "test-agent" }
    fn user_id(&self) -> &str { "user-1" }
    fn app_name(&self) -> &str { "test-app" }
    fn session_id(&self) -> &str { "session-1" }
    fn branch(&self) -> &str { "" }
    fn user_content(&self) -> &Content { &self.content }
}

#[async_trait]
impl InvocationContext for MockContext {
    fn agent(&self) -> Arc<dyn Agent> {
        unimplemented!()
    }
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> { None }
    fn memory(&self) -> Option<Arc<dyn adk_core::Memory>> { None }
    fn run_config(&self) -> &RunConfig { &self.config }
    fn end_invocation(&self) {}
    fn ended(&self) -> bool { false }
}

#[test]
fn test_custom_agent_builder() {
    let agent = CustomAgent::builder("test_agent")
        .description("A test agent")
        .handler(|_ctx| async {
            let stream = async_stream::stream! {
                yield Ok(Event::new("inv-1"));
            };
            Ok(Box::pin(stream) as adk_core::EventStream)
        })
        .build()
        .unwrap();

    assert_eq!(agent.name(), "test_agent");
    assert_eq!(agent.description(), "A test agent");
}

#[tokio::test]
async fn test_custom_agent_run() {
    let agent = CustomAgent::builder("echo_agent")
        .description("Echoes input")
        .handler(|ctx| async move {
            let mut event = Event::new(ctx.invocation_id());
            event.content = Some(ctx.user_content().clone());
            
            let stream = async_stream::stream! {
                yield Ok(event);
            };
            Ok(Box::pin(stream) as adk_core::EventStream)
        })
        .build()
        .unwrap();

    let ctx = Arc::new(MockContext::new()) as Arc<dyn InvocationContext>;
    let mut stream = agent.run(ctx).await.unwrap();
    
    let event = stream.next().await.unwrap().unwrap();
    assert!(event.content.is_some());
}

#[test]
fn test_custom_agent_requires_handler() {
    let result = CustomAgent::builder("incomplete")
        .description("Missing handler")
        .build();
    
    assert!(result.is_err());
}
