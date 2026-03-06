use adk_agent::CustomAgent;
use adk_core::{
    Agent, CallbackContext, Content, Event, InvocationContext, Memory, Part, ReadonlyContext,
    RunConfig, Session, types::AdkIdentity,
};
use async_trait::async_trait;
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;

use adk_core::types::{SessionId, UserId};

struct MockSession {
    id: SessionId,
    user_id: UserId,
}

impl MockSession {
    fn new() -> Self {
        Self {
            id: SessionId::new("test-session".to_string()).unwrap(),
            user_id: UserId::new("test-user".to_string()).unwrap(),
        }
    }
}

impl Session for MockSession {
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
        unimplemented!()
    }
    fn conversation_history(&self) -> Vec<Content> {
        Vec::new()
    }
}

struct MockContext {
    #[allow(dead_code)]
    content: Content,
    session: MockSession,
    user_content: Content,
    identity: AdkIdentity,
    metadata: HashMap<String, String>,
}

impl MockContext {
    fn new() -> Self {
        Self {
            content: Content {
                role: "user".to_string(),
                parts: vec![Part::text("test".to_string())],
            },
            session: MockSession::new(),
            user_content: Content {
                role: "user".to_string(),
                parts: vec![Part::text("test".to_string())],
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
impl CallbackContext for MockContext {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}

#[async_trait]
impl InvocationContext for MockContext {
    fn agent(&self) -> Arc<dyn Agent> {
        unimplemented!()
    }
    fn memory(&self) -> Option<Arc<dyn Memory>> {
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
            let mut event = Event::new(ctx.invocation_id().to_string());
            event.llm_response.content = Some(ctx.user_content().clone());

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
    assert!(event.llm_response.content.is_some());
}

#[test]
fn test_custom_agent_requires_handler() {
    let result = CustomAgent::builder("incomplete").description("Missing handler").build();

    assert!(result.is_err());
}

#[tokio::test]
async fn test_custom_agent_with_sub_agents() {
    let sub_agent = CustomAgent::builder("sub_agent")
        .description("A sub agent")
        .handler(|_ctx| async {
            let stream = async_stream::stream! {
                yield Ok(Event::new("inv-1"));
            };
            Ok(Box::pin(stream) as adk_core::EventStream)
        })
        .build()
        .unwrap();

    let agent = CustomAgent::builder("parent_agent")
        .description("Parent with sub-agents")
        .sub_agent(Arc::new(sub_agent))
        .handler(|_ctx| async {
            let stream = async_stream::stream! {
                yield Ok(Event::new("inv-1"));
            };
            Ok(Box::pin(stream) as adk_core::EventStream)
        })
        .build()
        .unwrap();

    assert_eq!(agent.sub_agents().len(), 1);
    assert_eq!(agent.sub_agents()[0].name(), "sub_agent");
}

#[test]
fn test_custom_agent_duplicate_sub_agents() {
    let sub1 = CustomAgent::builder("duplicate")
        .handler(|_ctx| async {
            let stream = async_stream::stream! {
                yield Ok(Event::new("inv-1"));
            };
            Ok(Box::pin(stream) as adk_core::EventStream)
        })
        .build()
        .unwrap();

    let sub2 = CustomAgent::builder("duplicate")
        .handler(|_ctx| async {
            let stream = async_stream::stream! {
                yield Ok(Event::new("inv-1"));
            };
            Ok(Box::pin(stream) as adk_core::EventStream)
        })
        .build()
        .unwrap();

    let result = CustomAgent::builder("parent")
        .sub_agents(vec![Arc::new(sub1), Arc::new(sub2)])
        .handler(|_ctx| async {
            let stream = async_stream::stream! {
                yield Ok(Event::new("inv-1"));
            };
            Ok(Box::pin(stream) as adk_core::EventStream)
        })
        .build();

    assert!(result.is_err());
}
