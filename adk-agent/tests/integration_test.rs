use adk_agent::Agent;
use adk_agent::LlmAgentBuilder;
use adk_core::types::{AdkIdentity, InvocationId, Role, SessionId, UserId};
use adk_core::{Content, InvocationContext, Part, ReadonlyContext, Session, State};
use adk_model::GeminiModel;
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::Value;
use std::collections::HashMap;
use std::env;
use std::sync::Arc;

// --- Mocks ---

struct MockSession {
    session_id: SessionId,
    user_id: UserId,
}

impl Session for MockSession {
    fn id(&self) -> &SessionId {
        &self.session_id
    }
    fn app_name(&self) -> &str {
        "test-app"
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
    identity: AdkIdentity,
    session: MockSession,
    user_content: Content,
    metadata: HashMap<String, String>,
}

impl MockContext {
    fn new(text: &str) -> Self {
        let identity = AdkIdentity {
            invocation_id: InvocationId::new("inv-real").unwrap(),
            user_id: UserId::new("user-real").unwrap(),
            session_id: SessionId::new("session-real").unwrap(),
            ..Default::default()
        };
        Self {
            identity,
            session: MockSession {
                session_id: SessionId::new("session-real").unwrap(),
                user_id: UserId::new("user-real").unwrap(),
            },
            user_content: Content { role: Role::User, parts: vec![Part::Text(text.to_string())] },
            metadata: HashMap::new(),
        }
    }
}

impl ReadonlyContext for MockContext {
    fn identity(&self) -> &AdkIdentity {
        &self.identity
    }
    fn metadata(&self) -> &HashMap<String, String> {
        &self.metadata
    }
    fn user_content(&self) -> &Content {
        &self.user_content
    }
}

impl adk_core::CallbackContext for MockContext {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}

#[async_trait]
impl InvocationContext for MockContext {
    fn session(&self) -> &dyn Session {
        &self.session
    }
    fn agent(&self) -> Arc<dyn Agent> {
        unimplemented!()
    }
    fn memory(&self) -> Option<Arc<dyn adk_core::Memory>> {
        None
    }
    fn run_config(&self) -> &adk_core::RunConfig {
        static RUN_CONFIG: std::sync::OnceLock<adk_core::RunConfig> = std::sync::OnceLock::new();
        RUN_CONFIG.get_or_init(adk_core::RunConfig::default)
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
                        if let Part::Text(text) = part {
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
