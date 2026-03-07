use adk_agent::{
    CustomAgentBuilder, LlmConditionalAgentBuilder, LoopAgent, ParallelAgent, SequentialAgent,
};
use adk_core::{
    Agent, Content, Event, InvocationContext, LlmRequest, ReadonlyContext, RunConfig,
    types::{AdkIdentity, Role, SessionId, UserId},
};
use async_trait::async_trait;
use futures::stream;
use std::collections::HashMap;
use std::sync::Arc;

struct MockSession {
    id: SessionId,
    user_id: UserId,
}

impl MockSession {
    fn new() -> Self {
        Self {
            id: SessionId::new("test-session").unwrap(),
            user_id: UserId::new("test-user").unwrap(),
        }
    }
}

impl adk_core::Session for MockSession {
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
        &MockState
    }
    fn conversation_history(&self) -> Vec<adk_core::Content> {
        Vec::new()
    }
}

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

struct TestContext {
    content: Content,
    config: RunConfig,
    session: MockSession,
    identity: AdkIdentity,
    metadata: HashMap<String, String>,
}

impl TestContext {
    fn new(message: &str) -> Self {
        Self {
            content: Content::new(Role::User).with_text(message.to_string()),
            config: RunConfig::default(),
            session: MockSession::new(),
            identity: AdkIdentity::default(),
            metadata: HashMap::new(),
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
    fn metadata(&self) -> &HashMap<String, String> {
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
                content: Some(Content::new(Role::Model).with_text(text)),
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
        .handler(|ctx| {
            let inv_id = ctx.invocation_id().clone();
            async move {
                let mut event = Event::new(inv_id);
                event.author = "agent1".to_string();
                event.llm_response.content =
                    Some(Content::new(Role::Model).with_text("Response from agent1"));
                Ok(Box::pin(stream::iter(vec![Ok(event)])) as adk_core::EventStream)
            }
        })
        .build()
        .unwrap();

    let agent2 = CustomAgentBuilder::new("agent2")
        .handler(|ctx| {
            let inv_id = ctx.invocation_id().clone();
            async move {
                let mut event = Event::new(inv_id);
                event.author = "agent2".to_string();
                event.llm_response.content =
                    Some(Content::new(Role::Model).with_text("Response from agent2"));
                Ok(Box::pin(stream::iter(vec![Ok(event)])) as adk_core::EventStream)
            }
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
        .handler(|ctx| {
            let inv_id = ctx.invocation_id().clone();
            async move {
                tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                let mut event = Event::new(inv_id);
                event.author = "agent1".to_string();
                Ok(Box::pin(stream::iter(vec![Ok(event)])) as adk_core::EventStream)
            }
        })
        .build()
        .unwrap();

    let agent2 = CustomAgentBuilder::new("agent2")
        .handler(|ctx| {
            let inv_id = ctx.invocation_id().clone();
            async move {
                let mut event = Event::new(inv_id);
                event.author = "agent2".to_string();
                Ok(Box::pin(stream::iter(vec![Ok(event)])) as adk_core::EventStream)
            }
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
    let authors: Vec<_> = events.iter().map(|e| e.author.as_str()).collect();
    assert!(authors.contains(&"agent1"));
    assert!(authors.contains(&"agent2"));
}

#[tokio::test]
async fn test_loop_agent_with_max_iterations() {
    use std::sync::atomic::{AtomicU32, Ordering};
    let counter = Arc::new(AtomicU32::new(0));
    let counter_clone = counter.clone();

    let agent = CustomAgentBuilder::new("counter")
        .handler(move |ctx| {
            let counter = counter_clone.clone();
            let inv_id = ctx.invocation_id().clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                let mut event = Event::new(inv_id);
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
        .handler(|ctx| {
            let inv_id = ctx.invocation_id().clone();
            async move {
                let text = ctx.user_content().text();
                let mut event = Event::new(inv_id);
                event.author = "echo".to_string();
                event.llm_response.content = Some(Content::new(Role::Model).with_text(text));
                Ok(Box::pin(stream::iter(vec![Ok(event)])) as adk_core::EventStream)
            }
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
    let text = event.content().unwrap().text();

    assert!(text.contains("[skill:search]"));
    assert!(text.contains("Use rg first."));
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
        .handler(|ctx| {
            let inv_id = ctx.invocation_id().clone();
            async move {
                let text = ctx.user_content().text();
                let mut event = Event::new(inv_id);
                event.author = "technical_agent".to_string();
                event.llm_response.content = Some(Content::new(Role::Model).with_text(text));
                Ok(Box::pin(stream::iter(vec![Ok(event)])) as adk_core::EventStream)
            }
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
    let text = target_event.content().unwrap().text();

    assert!(text.contains("[skill:search]"));
    assert!(text.contains("Use rg first."));
}
