use adk_agent::{
    ConditionalAgent, CustomAgentBuilder, LlmConditionalAgentBuilder, LoopAgent, ParallelAgent,
    SequentialAgent,
};
use adk_core::{
    Agent, Content, Event, InvocationContext, LlmRequest, Part, ReadonlyContext, RunConfig,
};
use async_trait::async_trait;
use futures::stream;
use std::collections::HashMap;
use std::sync::Arc;

struct MockState;

impl adk_core::State for MockState {
    fn get(&self, _key: &str) -> Option<serde_json::Value> {
        None
    }
    fn set(&mut self, _key: String, _value: serde_json::Value) {}
    fn all(&self) -> HashMap<String, serde_json::Value> {
        HashMap::new()
    }
}

struct MockSession {
    state: MockState,
}

impl MockSession {
    fn new() -> Self {
        Self { state: MockState }
    }
}

impl adk_core::Session for MockSession {
    fn id(&self) -> &str {
        "test-session"
    }
    fn app_name(&self) -> &str {
        "test-app"
    }
    fn user_id(&self) -> &str {
        "test-user"
    }
    fn state(&self) -> &dyn adk_core::State {
        &self.state
    }
    fn conversation_history(&self) -> Vec<adk_core::Content> {
        Vec::new()
    }
}

struct TestContext {
    content: Content,
    config: RunConfig,
    session: MockSession,
}

impl TestContext {
    fn new(message: &str) -> Self {
        Self {
            content: Content {
                role: "user".to_string(),
                parts: vec![Part::Text { text: message.to_string() }],
            },
            config: RunConfig::default(),
            session: MockSession::new(),
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
    fn session(&self) -> &dyn adk_core::Session {
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

struct MockRouterLlm {
    response_text: String,
}

impl MockRouterLlm {
    fn new(response_text: &str) -> Self {
        Self { response_text: response_text.to_string() }
    }
}

#[async_trait]
impl adk_core::Llm for MockRouterLlm {
    fn name(&self) -> &str {
        "mock-router-llm"
    }

    async fn generate_content(
        &self,
        _request: LlmRequest,
        _stream: bool,
    ) -> adk_core::Result<adk_core::LlmResponseStream> {
        let text = self.response_text.clone();
        let s = async_stream::stream! {
            yield Ok(adk_core::LlmResponse {
                content: Some(adk_core::Content {
                    role: "model".to_string(),
                    parts: vec![adk_core::Part::Text { text }],
                }),
                usage_metadata: None,
                finish_reason: None,
                citation_metadata: None,
                partial: false,
                turn_complete: true,
                interrupted: false,
                error_code: None,
                error_message: None,
            });
        };
        Ok(Box::pin(s))
    }
}

#[tokio::test]
async fn test_sequential_agent_execution_order() {
    let agent1 = CustomAgentBuilder::new("agent1")
        .description("First agent")
        .handler(|_ctx| async move {
            let mut event = Event::new("test-invocation");
            event.author = "agent1".to_string();
            event.llm_response.content = Some(Content {
                role: "assistant".to_string(),
                parts: vec![Part::Text { text: "Response from agent1".to_string() }],
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
            event.llm_response.content = Some(Content {
                role: "assistant".to_string(),
                parts: vec![Part::Text { text: "Response from agent2".to_string() }],
            });
            Ok(Box::pin(stream::iter(vec![Ok(event)])) as adk_core::EventStream)
        })
        .build()
        .unwrap();

    let sequential = SequentialAgent::new("sequential", vec![Arc::new(agent1), Arc::new(agent2)]);

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

    let parallel = ParallelAgent::new("parallel", vec![Arc::new(agent1), Arc::new(agent2)]);

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
    let agent = SequentialAgent::new("test", vec![]).with_description("Test description");

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

    let loop_agent = LoopAgent::new("loop", vec![Arc::new(agent)]).with_max_iterations(3);

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

    let loop_agent = LoopAgent::new("loop", vec![Arc::new(agent)]).with_max_iterations(10);

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

    let conditional = ConditionalAgent::new("conditional", |_ctx| true, Arc::new(if_agent))
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

    let conditional = ConditionalAgent::new("conditional", |_ctx| false, Arc::new(if_agent))
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

    let conditional = ConditionalAgent::new("conditional", |_ctx| false, Arc::new(if_agent));

    let ctx = Arc::new(TestContext::new("test"));
    let mut stream = conditional.run(ctx).await.unwrap();

    use futures::StreamExt;
    let result = stream.next().await;
    assert!(result.is_none());
}

#[tokio::test]
async fn test_sequential_agent_with_skills_injects_user_content() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    std::fs::create_dir_all(root.join(".skills")).unwrap();
    std::fs::write(
        root.join(".skills/search.md"),
        "---\nname: search\ndescription: Search repository code\ntags: [search, code]\n---\nUse rg first.\n",
    )
    .unwrap();

    let echo_agent = CustomAgentBuilder::new("echo")
        .handler(|ctx| async move {
            let text = ctx
                .user_content()
                .parts
                .iter()
                .find_map(|p| p.text())
                .unwrap_or_default()
                .to_string();

            let mut event = Event::new("test-invocation");
            event.author = "echo".to_string();
            event.llm_response.content = Some(Content::new("assistant").with_text(text));
            Ok(Box::pin(stream::iter(vec![Ok(event)])) as adk_core::EventStream)
        })
        .build()
        .unwrap();

    let sequential = SequentialAgent::new("sequential", vec![Arc::new(echo_agent)])
        .with_skills_from_root(root)
        .unwrap();

    let ctx = Arc::new(TestContext::new("please search this repo"));
    let mut stream = sequential.run(ctx).await.unwrap();
    use futures::StreamExt;
    let event = stream.next().await.unwrap().unwrap();
    let text = event
        .llm_response
        .content
        .unwrap()
        .parts
        .iter()
        .find_map(|p| p.text())
        .unwrap()
        .to_string();

    assert!(text.contains("[skill:search]"));
}

#[tokio::test]
async fn test_parallel_agent_with_skills_injects_user_content() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    std::fs::create_dir_all(root.join(".skills")).unwrap();
    std::fs::write(
        root.join(".skills/search.md"),
        "---\nname: search\ndescription: Search repository code\ntags: [search, code]\n---\nUse rg first.\n",
    )
    .unwrap();

    let echo_agent = CustomAgentBuilder::new("echo")
        .handler(|ctx| async move {
            let text = ctx
                .user_content()
                .parts
                .iter()
                .find_map(|p| p.text())
                .unwrap_or_default()
                .to_string();
            let mut event = Event::new("test-invocation");
            event.author = "echo".to_string();
            event.llm_response.content = Some(Content::new("assistant").with_text(text));
            Ok(Box::pin(stream::iter(vec![Ok(event)])) as adk_core::EventStream)
        })
        .build()
        .unwrap();

    let parallel = ParallelAgent::new("parallel", vec![Arc::new(echo_agent)])
        .with_skills_from_root(root)
        .unwrap();

    let ctx = Arc::new(TestContext::new("please search this repo"));
    let mut stream = parallel.run(ctx).await.unwrap();
    use futures::StreamExt;
    let event = stream.next().await.unwrap().unwrap();
    let text = event
        .llm_response
        .content
        .unwrap()
        .parts
        .iter()
        .find_map(|p| p.text())
        .unwrap()
        .to_string();

    assert!(text.contains("[skill:search]"));
}

#[tokio::test]
async fn test_conditional_agent_with_skills_injects_user_content() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    std::fs::create_dir_all(root.join(".skills")).unwrap();
    std::fs::write(
        root.join(".skills/search.md"),
        "---\nname: search\ndescription: Search repository code\ntags: [search, code]\n---\nUse rg first.\n",
    )
    .unwrap();

    let if_agent = CustomAgentBuilder::new("if_agent")
        .handler(|ctx| async move {
            let text = ctx
                .user_content()
                .parts
                .iter()
                .find_map(|p| p.text())
                .unwrap_or_default()
                .to_string();
            let mut event = Event::new("test-invocation");
            event.author = "if_agent".to_string();
            event.llm_response.content = Some(Content::new("assistant").with_text(text));
            Ok(Box::pin(stream::iter(vec![Ok(event)])) as adk_core::EventStream)
        })
        .build()
        .unwrap();

    let conditional = ConditionalAgent::new("conditional", |_ctx| true, Arc::new(if_agent))
        .with_skills_from_root(root)
        .unwrap();

    let ctx = Arc::new(TestContext::new("please search this repo"));
    let mut stream = conditional.run(ctx).await.unwrap();
    use futures::StreamExt;
    let event = stream.next().await.unwrap().unwrap();
    let text = event
        .llm_response
        .content
        .unwrap()
        .parts
        .iter()
        .find_map(|p| p.text())
        .unwrap()
        .to_string();

    assert!(text.contains("[skill:search]"));
}

#[tokio::test]
async fn test_llm_conditional_agent_with_skills_injects_user_content() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    std::fs::create_dir_all(root.join(".skills")).unwrap();
    std::fs::write(
        root.join(".skills/search.md"),
        "---\nname: search\ndescription: Search repository code\ntags: [search, code]\n---\nUse rg first.\n",
    )
    .unwrap();

    let target_agent = CustomAgentBuilder::new("technical_agent")
        .handler(|ctx| async move {
            let text = ctx
                .user_content()
                .parts
                .iter()
                .find_map(|p| p.text())
                .unwrap_or_default()
                .to_string();
            let mut event = Event::new("test-invocation");
            event.author = "technical_agent".to_string();
            event.llm_response.content = Some(Content::new("assistant").with_text(text));
            Ok(Box::pin(stream::iter(vec![Ok(event)])) as adk_core::EventStream)
        })
        .build()
        .unwrap();

    let router =
        LlmConditionalAgentBuilder::new("router", Arc::new(MockRouterLlm::new("technical")))
            .with_skills_from_root(root)
            .unwrap()
            .instruction("Classify request")
            .route("technical", Arc::new(target_agent))
            .build()
            .unwrap();

    let ctx = Arc::new(TestContext::new("please search this repo"));
    let mut stream = router.run(ctx).await.unwrap();
    use futures::StreamExt;
    let _routing = stream.next().await.unwrap().unwrap();
    let target_event = stream.next().await.unwrap().unwrap();
    let text = target_event
        .llm_response
        .content
        .unwrap()
        .parts
        .iter()
        .find_map(|p| p.text())
        .unwrap()
        .to_string();

    assert!(text.contains("[skill:search]"));
}
