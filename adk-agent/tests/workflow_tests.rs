use adk_agent::{ConditionalAgent, CustomAgentBuilder, LoopAgent, ParallelAgent, SequentialAgent};
use adk_core::{Agent, Content, Event, InvocationContext, Part, ReadonlyContext, RunConfig};
use async_trait::async_trait;
use futures::stream;
use std::sync::Arc;

struct TestContext {
    content: Content,
    config: RunConfig,
}

impl TestContext {
    fn new(message: &str) -> Self {
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
    fn invocation_id(&self) -> &str {
        "test-invocation"
    }
    fn agent_name(&self) -> &str {
        "test-agent"
    }
    fn user_id(&self) -> &str {
        "test-user"
    }
    fn app_name(&self) -> &str {
        "test-app"
    }
    fn session_id(&self) -> &str {
        "test-session"
    }
    fn branch(&self) -> &str {
        ""
    }
    fn user_content(&self) -> &Content {
        &self.content
    }
}

#[async_trait]
impl InvocationContext for TestContext {
    fn agent(&self) -> Arc<dyn Agent> {
        unimplemented!()
    }
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
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
}

#[tokio::test]
async fn test_sequential_agent_execution_order() {
    let agent1 = CustomAgentBuilder::new("agent1")
        .description("First agent")
        .handler(|_ctx| async move {
            let mut event = Event::new("test-invocation");
            event.author = "agent1".to_string();
            event.content = Some(Content {
                role: "assistant".to_string(),
                parts: vec![Part::Text {
                    text: "Response from agent1".to_string(),
                }],
            });
            Ok(Box::pin(stream::iter(vec![Ok(event)])) as adk_core::EventStream)
        })
        .build()
        .unwrap();

    let agent2 = CustomAgentBuilder::new("agent2")
        .description("Second agent")
        .handler(|_ctx| async move {
            let mut event = Event::new("test-invocation");
            event.author = "agent2".to_string();
            event.content = Some(Content {
                role: "assistant".to_string(),
                parts: vec![Part::Text {
                    text: "Response from agent2".to_string(),
                }],
            });
            Ok(Box::pin(stream::iter(vec![Ok(event)])) as adk_core::EventStream)
        })
        .build()
        .unwrap();

    let sequential = SequentialAgent::new(
        "sequential",
        vec![Arc::new(agent1), Arc::new(agent2)],
    );

    let ctx = Arc::new(TestContext::new("test"));
    let mut stream = sequential.run(ctx).await.unwrap();

    use futures::StreamExt;
    let mut events = Vec::new();
    while let Some(result) = stream.next().await {
        events.push(result.unwrap());
    }

    assert_eq!(events.len(), 2);
    assert_eq!(events[0].author, "agent1");
    assert_eq!(events[1].author, "agent2");
}

#[tokio::test]
async fn test_parallel_agent_execution() {
    let agent1 = CustomAgentBuilder::new("agent1")
        .handler(|_ctx| async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
            let mut event = Event::new("test-invocation");
            event.author = "agent1".to_string();
            Ok(Box::pin(stream::iter(vec![Ok(event)])) as adk_core::EventStream)
        })
        .build()
        .unwrap();

    let agent2 = CustomAgentBuilder::new("agent2")
        .handler(|_ctx| async move {
            let mut event = Event::new("test-invocation");
            event.author = "agent2".to_string();
            Ok(Box::pin(stream::iter(vec![Ok(event)])) as adk_core::EventStream)
        })
        .build()
        .unwrap();

    let parallel = ParallelAgent::new(
        "parallel",
        vec![Arc::new(agent1), Arc::new(agent2)],
    );

    let ctx = Arc::new(TestContext::new("test"));
    let mut stream = parallel.run(ctx).await.unwrap();

    use futures::StreamExt;
    let mut events = Vec::new();
    while let Some(result) = stream.next().await {
        events.push(result.unwrap());
    }

    assert_eq!(events.len(), 2);
    // Order not guaranteed in parallel execution
    let authors: Vec<_> = events.iter().map(|e| e.author.as_str()).collect();
    assert!(authors.contains(&"agent1"));
    assert!(authors.contains(&"agent2"));
}

#[tokio::test]
async fn test_sequential_agent_empty() {
    let sequential = SequentialAgent::new("empty", vec![]);

    let ctx = Arc::new(TestContext::new("test"));
    let mut stream = sequential.run(ctx).await.unwrap();

    use futures::StreamExt;
    let result = stream.next().await;
    assert!(result.is_none());
}

#[tokio::test]
async fn test_parallel_agent_empty() {
    let parallel = ParallelAgent::new("empty", vec![]);

    let ctx = Arc::new(TestContext::new("test"));
    let mut stream = parallel.run(ctx).await.unwrap();

    use futures::StreamExt;
    let result = stream.next().await;
    assert!(result.is_none());
}

#[tokio::test]
async fn test_sequential_agent_with_description() {
    let agent = SequentialAgent::new("test", vec![])
        .with_description("Test description");

    assert_eq!(agent.name(), "test");
    assert_eq!(agent.description(), "Test description");
}

#[tokio::test]
async fn test_loop_agent_with_max_iterations() {
    use std::sync::atomic::{AtomicU32, Ordering};
    let counter = Arc::new(AtomicU32::new(0));
    let counter_clone = counter.clone();

    let agent = CustomAgentBuilder::new("counter")
        .handler(move |_ctx| {
            let counter = counter_clone.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                let mut event = Event::new("test-invocation");
                event.author = "counter".to_string();
                Ok(Box::pin(stream::iter(vec![Ok(event)])) as adk_core::EventStream)
            }
        })
        .build()
        .unwrap();

    let loop_agent = LoopAgent::new("loop", vec![Arc::new(agent)])
        .with_max_iterations(3);

    let ctx = Arc::new(TestContext::new("test"));
    let mut stream = loop_agent.run(ctx).await.unwrap();

    use futures::StreamExt;
    let mut events = Vec::new();
    while let Some(result) = stream.next().await {
        events.push(result.unwrap());
    }

    assert_eq!(counter.load(Ordering::SeqCst), 3);
    assert_eq!(events.len(), 3);
}

#[tokio::test]
async fn test_loop_agent_with_escalation() {
    use std::sync::atomic::{AtomicU32, Ordering};
    let counter = Arc::new(AtomicU32::new(0));
    let counter_clone = counter.clone();

    let agent = CustomAgentBuilder::new("escalator")
        .handler(move |_ctx| {
            let counter = counter_clone.clone();
            async move {
                let count = counter.fetch_add(1, Ordering::SeqCst);
                let mut event = Event::new("test-invocation");
                event.author = "escalator".to_string();
                if count == 1 {
                    event.actions.escalate = true;
                }
                Ok(Box::pin(stream::iter(vec![Ok(event)])) as adk_core::EventStream)
            }
        })
        .build()
        .unwrap();

    let loop_agent = LoopAgent::new("loop", vec![Arc::new(agent)])
        .with_max_iterations(10);

    let ctx = Arc::new(TestContext::new("test"));
    let mut stream = loop_agent.run(ctx).await.unwrap();

    use futures::StreamExt;
    let mut events = Vec::new();
    while let Some(result) = stream.next().await {
        events.push(result.unwrap());
    }

    assert_eq!(counter.load(Ordering::SeqCst), 2);
    assert_eq!(events.len(), 2);
}

#[tokio::test]
async fn test_loop_agent_no_max_iterations() {
    use std::sync::atomic::{AtomicU32, Ordering};
    let counter = Arc::new(AtomicU32::new(0));
    let counter_clone = counter.clone();

    let agent = CustomAgentBuilder::new("stopper")
        .handler(move |_ctx| {
            let counter = counter_clone.clone();
            async move {
                let count = counter.fetch_add(1, Ordering::SeqCst);
                let mut event = Event::new("test-invocation");
                event.author = "stopper".to_string();
                if count >= 4 {
                    event.actions.escalate = true;
                }
                Ok(Box::pin(stream::iter(vec![Ok(event)])) as adk_core::EventStream)
            }
        })
        .build()
        .unwrap();

    let loop_agent = LoopAgent::new("loop", vec![Arc::new(agent)]);

    let ctx = Arc::new(TestContext::new("test"));
    let mut stream = loop_agent.run(ctx).await.unwrap();

    use futures::StreamExt;
    let mut events = Vec::new();
    while let Some(result) = stream.next().await {
        events.push(result.unwrap());
    }

    assert_eq!(counter.load(Ordering::SeqCst), 5);
    assert_eq!(events.len(), 5);
}

#[tokio::test]
async fn test_conditional_agent_if_branch() {
    let if_agent = CustomAgentBuilder::new("if_agent")
        .handler(|_ctx| async move {
            let mut event = Event::new("test-invocation");
            event.author = "if_agent".to_string();
            Ok(Box::pin(stream::iter(vec![Ok(event)])) as adk_core::EventStream)
        })
        .build()
        .unwrap();

    let else_agent = CustomAgentBuilder::new("else_agent")
        .handler(|_ctx| async move {
            let mut event = Event::new("test-invocation");
            event.author = "else_agent".to_string();
            Ok(Box::pin(stream::iter(vec![Ok(event)])) as adk_core::EventStream)
        })
        .build()
        .unwrap();

    let conditional = ConditionalAgent::new(
        "conditional",
        |_ctx| true,
        Arc::new(if_agent),
    )
    .with_else(Arc::new(else_agent));

    let ctx = Arc::new(TestContext::new("test"));
    let mut stream = conditional.run(ctx).await.unwrap();

    use futures::StreamExt;
    let mut events = Vec::new();
    while let Some(result) = stream.next().await {
        events.push(result.unwrap());
    }

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].author, "if_agent");
}

#[tokio::test]
async fn test_conditional_agent_else_branch() {
    let if_agent = CustomAgentBuilder::new("if_agent")
        .handler(|_ctx| async move {
            let mut event = Event::new("test-invocation");
            event.author = "if_agent".to_string();
            Ok(Box::pin(stream::iter(vec![Ok(event)])) as adk_core::EventStream)
        })
        .build()
        .unwrap();

    let else_agent = CustomAgentBuilder::new("else_agent")
        .handler(|_ctx| async move {
            let mut event = Event::new("test-invocation");
            event.author = "else_agent".to_string();
            Ok(Box::pin(stream::iter(vec![Ok(event)])) as adk_core::EventStream)
        })
        .build()
        .unwrap();

    let conditional = ConditionalAgent::new(
        "conditional",
        |_ctx| false,
        Arc::new(if_agent),
    )
    .with_else(Arc::new(else_agent));

    let ctx = Arc::new(TestContext::new("test"));
    let mut stream = conditional.run(ctx).await.unwrap();

    use futures::StreamExt;
    let mut events = Vec::new();
    while let Some(result) = stream.next().await {
        events.push(result.unwrap());
    }

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].author, "else_agent");
}

#[tokio::test]
async fn test_conditional_agent_no_else() {
    let if_agent = CustomAgentBuilder::new("if_agent")
        .handler(|_ctx| async move {
            let mut event = Event::new("test-invocation");
            event.author = "if_agent".to_string();
            Ok(Box::pin(stream::iter(vec![Ok(event)])) as adk_core::EventStream)
        })
        .build()
        .unwrap();

    let conditional = ConditionalAgent::new(
        "conditional",
        |_ctx| false,
        Arc::new(if_agent),
    );

    let ctx = Arc::new(TestContext::new("test"));
    let mut stream = conditional.run(ctx).await.unwrap();

    use futures::StreamExt;
    let result = stream.next().await;
    assert!(result.is_none());
}
