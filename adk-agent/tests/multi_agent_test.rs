use adk_agent::LlmAgentBuilder;
use adk_core::{
    Agent, Content, InvocationContext, Part, ReadonlyContext, RunConfig, Session, State,
    types::AdkIdentity,
};
use adk_model::GeminiModel;
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::Value;
use std::collections::HashMap;
use std::env;
use std::sync::Arc;

use adk_core::types::{SessionId, UserId};

struct MockSession {
    id: SessionId,
    user_id: UserId,
}

impl MockSession {
    fn new() -> Self {
        Self {
            id: SessionId::new("multi-agent-session".to_string()).unwrap(),
            user_id: UserId::new("multi-agent-user".to_string()).unwrap(),
        }
    }
}

impl Session for MockSession {
    fn id(&self) -> &SessionId {
        &self.id
    }
    fn app_name(&self) -> &str {
        "multi-agent-app"
    }
    fn user_id(&self) -> &UserId {
        &self.user_id
    }
    fn state(&self) -> &dyn State {
        &MockState
    }
    fn conversation_history(&self) -> Vec<adk_core::Content> {
        Vec::new()
    }
}

struct MockState;
impl State for MockState {
    fn get(&self, _key: &str) -> Option<Value> {
        None
    }
    fn set(&mut self, _key: String, _value: Value) {}
    fn all(&self) -> HashMap<String, Value> {
        HashMap::new()
    }
}

struct MockContext {
    session: MockSession,
    user_content: Content,
    identity: AdkIdentity,
    metadata: HashMap<String, String>,
}

impl MockContext {
    fn new(text: &str) -> Self {
        Self {
            session: MockSession::new(),
            user_content: Content {
                role: adk_core::Role::User,
                parts: vec![Part::text(text.to_string())],
            },
            identity: AdkIdentity::default(),
            metadata: HashMap::new(),
        }
    }
}

#[async_trait]
impl ReadonlyContext for MockContext {
    fn identity(&self) -> &AdkIdentity {
        &self.identity
    }
    fn user_content(&self) -> &Content {
        &self.user_content
    }
    fn metadata(&self) -> &HashMap<String, String> {
        &self.metadata
    }
}

#[async_trait]
impl adk_core::CallbackContext for MockContext {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}

#[async_trait]
impl InvocationContext for MockContext {
    fn agent(&self) -> Arc<dyn Agent> {
        unimplemented!()
    }
    fn memory(&self) -> Option<Arc<dyn adk_core::Memory>> {
        None
    }
    fn session(&self) -> &dyn Session {
        &self.session
    }
    fn run_config(&self) -> &RunConfig {
        static RUN_CONFIG: std::sync::OnceLock<RunConfig> = std::sync::OnceLock::new();
        RUN_CONFIG.get_or_init(RunConfig::default)
    }
    fn end_invocation(&self) {}
    fn ended(&self) -> bool {
        false
    }
}

#[tokio::test]
#[ignore] // Requires GEMINI_API_KEY - run with: cargo test --ignored
async fn test_multi_agent_workflow() {
    dotenvy::dotenv().ok();
    let api_key = env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY must be set");

    let model = Arc::new(GeminiModel::new(api_key.clone(), "gemini-1.5-flash").unwrap());

    // Create a research agent
    let research_agent = Arc::new(
        LlmAgentBuilder::new("researcher")
            .description("Research agent that gathers information")
            .model(model.clone())
            .instruction("You are a research assistant. Provide factual, concise information.")
            .build()
            .unwrap(),
    );

    // Create a writer agent with research agent as sub-agent
    let writer_agent = LlmAgentBuilder::new("writer")
        .description("Writing agent that creates content")
        .model(model)
        .instruction("You are a creative writer. Use research to create engaging content.")
        .sub_agent(research_agent)
        .build()
        .unwrap();

    let ctx =
        Arc::new(MockContext::new("Write a brief paragraph about Rust programming language."));
    let mut stream = writer_agent.run(ctx).await.unwrap();

    let mut response_text = String::new();
    while let Some(result) = stream.next().await {
        if let Ok(event) = result {
            if let Some(content) = event.llm_response.content {
                for part in content.parts {
                    if let Some(text) = part.as_text() {
                        response_text.push_str(text);
                    }
                }
            }
        }
    }

    // Verify we got a response
    assert!(!response_text.is_empty(), "Should have received a response");
    println!("Multi-agent response: {}", response_text);
}

#[tokio::test]
#[ignore] // Requires GEMINI_API_KEY - run with: cargo test --ignored
async fn test_agent_delegation() {
    dotenvy::dotenv().ok();
    let api_key = env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY must be set");

    let model = Arc::new(GeminiModel::new(api_key, "gemini-1.5-flash").unwrap());

    // Create specialist agent
    let specialist = Arc::new(
        LlmAgentBuilder::new("math_specialist")
            .description("Math specialist")
            .model(model.clone())
            .instruction("You are a math specialist. Solve math problems accurately.")
            .build()
            .unwrap(),
    );

    // Create coordinator agent
    let coordinator = LlmAgentBuilder::new("coordinator")
        .description("Task coordinator")
        .model(model)
        .instruction("You coordinate tasks. Delegate math questions to specialists.")
        .sub_agent(specialist)
        .build()
        .unwrap();

    let ctx = Arc::new(MockContext::new("What is 15 * 23?"));
    let mut stream = coordinator.run(ctx).await.unwrap();

    let mut has_answer = false;
    while let Some(result) = stream.next().await {
        if let Ok(event) = result {
            if let Some(content) = event.llm_response.content {
                for part in content.parts {
                    if let Some(text) = part.as_text() {
                        if text.contains("345") {
                            has_answer = true;
                        }
                    }
                }
            }
        }
    }

    assert!(has_answer, "Should contain the correct answer (345)");
}
