use adk_agent::Agent;
use adk_agent::LlmAgentBuilder;
use adk_core::types::{AdkIdentity, InvocationId, Role, SessionId, UserId};
use adk_core::{
    Content, FinishReason, InvocationContext, Llm, LlmRequest, LlmResponse, LlmResponseStream,
    Part, ReadonlyContext, Result, Session, State,
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
                    role: Role::Model,
                    parts: vec![Part::Text(text.clone())],
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

// --- Tests ---

#[tokio::test]
async fn test_streaming_chunks() {
    let model = Arc::new(MockModel::new(vec!["Hello", " ", "World", "!"]));
    let agent = LlmAgentBuilder::new("test-agent").model(model).build().unwrap();

    struct BetterMockContext {
        identity: AdkIdentity,
        session: MockSession,
        user_content: Content,
        metadata: HashMap<String, String>,
    }

    impl BetterMockContext {
        fn new() -> Self {
            let identity = AdkIdentity {
                invocation_id: InvocationId::new("inv-1").unwrap(),
                user_id: UserId::new("user-1").unwrap(),
                session_id: SessionId::new("session-1").unwrap(),
                ..Default::default()
            };
            Self {
                identity,
                session: MockSession {
                    session_id: SessionId::new("session-1").unwrap(),
                    user_id: UserId::new("user-1").unwrap(),
                },
                user_content: Content {
                    role: Role::User,
                    parts: vec![Part::Text("Hi".to_string())],
                },
                metadata: HashMap::new(),
            }
        }
    }

    impl ReadonlyContext for BetterMockContext {
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

    impl adk_core::CallbackContext for BetterMockContext {
        fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
            None
        }
    }

    #[async_trait]
    impl InvocationContext for BetterMockContext {
        fn session(&self) -> &dyn Session {
            &self.session
        }
        fn agent(&self) -> Arc<dyn adk_core::Agent> {
            unimplemented!()
        }
        fn memory(&self) -> Option<Arc<dyn adk_core::Memory>> {
            None
        }
        fn run_config(&self) -> &adk_core::RunConfig {
            static RUN_CONFIG: std::sync::OnceLock<adk_core::RunConfig> =
                std::sync::OnceLock::new();
            RUN_CONFIG.get_or_init(adk_core::RunConfig::default)
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
            if let Some(Part::Text(text)) = content.parts.first() {
                received_chunks.push(text.clone());
            }
        }
    }

    assert_eq!(received_chunks, vec!["Hello", " ", "World", "!"]);
}
