use adk_agent::LlmAgentBuilder;
use adk_core::{
    Agent, Content, FinishReason, InvocationContext, Llm, LlmRequest, LlmResponse,
    LlmResponseStream, Part, Result, RunConfig, Session, State,
};
use async_stream::stream;
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

// --- Mocks ---

struct MockModel {
    chunks: Vec<String>,
}

impl MockModel {
    fn new(chunks: Vec<&str>) -> Self {
        Self { chunks: chunks.iter().map(|s| s.to_string()).collect() }
    }
}

#[async_trait]
impl Llm for MockModel {
    fn name(&self) -> &str {
        "mock-model"
    }

    async fn generate_content(&self, _req: LlmRequest, stream: bool) -> Result<LlmResponseStream> {
        assert!(stream, "Agent should request streaming");

        let chunks = self.chunks.clone();
        let s = stream! {
            for (i, text) in chunks.iter().enumerate() {
                let is_last = i == chunks.len() - 1;

                let content = Content {
                    role: "model".to_string(),
                    parts: vec![Part::Text { text: text.clone() }],
                };

                yield Ok(LlmResponse {
                    content: Some(content),
                    usage_metadata: None,
                    finish_reason: if is_last { Some(FinishReason::Stop) } else { None },
                    citation_metadata: None,
                    partial: !is_last,
                    turn_complete: is_last,
                    interrupted: false,
                    error_code: None,
                    error_message: None,
                });
            }
        };

        Ok(Box::pin(s))
    }
}

struct MockSession;
impl Session for MockSession {
    fn id(&self) -> &str {
        "session-1"
    }
    fn app_name(&self) -> &str {
        "test-app"
    }
    fn user_id(&self) -> &str {
        "user-1"
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
}

impl MockContext {
    fn new() -> Self {
        Self { session: MockSession }
    }
}

#[async_trait]
impl adk_core::ReadonlyContext for MockContext {
    fn invocation_id(&self) -> &str {
        "inv-1"
    }
    fn agent_name(&self) -> &str {
        "test-agent"
    }
    fn user_id(&self) -> &str {
        "user-1"
    }
    fn app_name(&self) -> &str {
        "test-app"
    }
    fn session_id(&self) -> &str {
        "session-1"
    }
    fn branch(&self) -> &str {
        "main"
    }
    fn user_content(&self) -> &Content {
        unimplemented!()
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
async fn test_streaming_chunks() {
    let model = Arc::new(MockModel::new(vec!["Hello", " ", "World", "!"]));
    let agent = LlmAgentBuilder::new("test-agent").model(model).build().unwrap();

    let _ctx = Arc::new(MockContext::new());

    // We need to provide user content in the context, but MockContext panics on user_content()
    // Let's fix MockContext to return dummy content
    // Actually, LlmAgent calls ctx.user_content()

    // Let's refine MockContext
    struct BetterMockContext {
        session: MockSession,
        user_content: Content,
    }

    impl BetterMockContext {
        fn new() -> Self {
            Self {
                session: MockSession,
                user_content: Content {
                    role: "user".to_string(),
                    parts: vec![Part::Text { text: "Hi".to_string() }],
                },
            }
        }
    }

    #[async_trait]
    impl adk_core::ReadonlyContext for BetterMockContext {
        fn invocation_id(&self) -> &str {
            "inv-1"
        }
        fn agent_name(&self) -> &str {
            "test-agent"
        }
        fn user_id(&self) -> &str {
            "user-1"
        }
        fn app_name(&self) -> &str {
            "test-app"
        }
        fn session_id(&self) -> &str {
            "session-1"
        }
        fn branch(&self) -> &str {
            "main"
        }
        fn user_content(&self) -> &Content {
            &self.user_content
        }
    }

    #[async_trait]
    impl adk_core::CallbackContext for BetterMockContext {
        fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
            None
        }
    }

    #[async_trait]
    impl InvocationContext for BetterMockContext {
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

    let ctx = Arc::new(BetterMockContext::new());
    let mut stream = agent.run(ctx).await.unwrap();

    let mut received_chunks = Vec::new();

    while let Some(result) = stream.next().await {
        let event = result.unwrap();
        if let Some(content) = event.llm_response.content {
            if let Some(Part::Text { text }) = content.parts.first() {
                received_chunks.push(text.clone());
            }
        }
    }

    assert_eq!(received_chunks, vec!["Hello", " ", "World", "!"]);
}
