use adk_agent::LlmAgentBuilder;
use adk_core::{Agent, Content, InvocationContext, Part, RunConfig, Session, State};
use adk_model::GeminiModel;
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::Value;
use std::collections::HashMap;
use std::env;
use std::sync::Arc;

// --- Mocks ---

struct MockSession;
impl Session for MockSession {
    fn id(&self) -> &str {
        "session-real"
    }
    fn app_name(&self) -> &str {
        "test-app"
    }
    fn user_id(&self) -> &str {
        "user-real"
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
}

impl MockContext {
    fn new(text: &str) -> Self {
        Self {
            session: MockSession,
            user_content: Content {
                role: "user".to_string(),
                parts: vec![Part::Text { text: text.to_string() }],
            },
        }
    }
}

#[async_trait]
impl adk_core::ReadonlyContext for MockContext {
    fn invocation_id(&self) -> &str {
        "inv-real"
    }
    fn agent_name(&self) -> &str {
        "gemini-agent"
    }
    fn user_id(&self) -> &str {
        "user-real"
    }
    fn app_name(&self) -> &str {
        "test-app"
    }
    fn session_id(&self) -> &str {
        "session-real"
    }
    fn branch(&self) -> &str {
        "main"
    }
    fn user_content(&self) -> &Content {
        &self.user_content
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

// --- Tests ---

#[tokio::test]
#[ignore] // Requires GEMINI_API_KEY - run with: cargo test --ignored
async fn test_real_gemini_interaction() {
    // Load API key from env
    dotenvy::dotenv().ok();
    let api_key = env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY must be set");

    println!("Using Gemini API Key: [REDACTED]");

    let model = Arc::new(GeminiModel::new(api_key, "gemini-1.5-flash").unwrap());

    let agent = LlmAgentBuilder::new("gemini-agent")
        .model(model)
        .instruction("You are a helpful assistant. Answer concisely.")
        .build()
        .unwrap();

    let ctx = Arc::new(MockContext::new("What is the capital of France?"));

    let mut stream = agent.run(ctx).await.unwrap();

    let mut full_response = String::new();

    while let Some(result) = stream.next().await {
        match result {
            Ok(event) => {
                if let Some(content) = event.llm_response.content {
                    for part in content.parts {
                        if let Part::Text { text } = part {
                            print!("{}", text);
                            full_response.push_str(&text);
                        }
                    }
                }
            }
            Err(e) => {
                panic!("Error from agent: {}", e);
            }
        }
    }

    println!("\nFull response: {}", full_response);
    assert!(full_response.contains("Paris"));
}
