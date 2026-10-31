//! The runner refuses a run for a subject being erased.

use std::sync::Arc;

use adk_core::subject::{
    ConsentGate, InMemoryConsentStore, InMemorySubjectRegistry, SUBJECT_ERASING_CODE,
    SubjectRegistry, SubjectState, VisitorId,
};
use adk_core::{
    Agent, Content, Event, EventStream, InvocationContext, RunConfig, SessionId, SubjectKind,
    UserId,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;

struct Echo;

#[async_trait]
impl Agent for Echo {
    fn name(&self) -> &str {
        "echo"
    }
    fn description(&self) -> &str {
        "answers once"
    }
    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }
    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> adk_core::Result<EventStream> {
        let mut event = Event::new(ctx.invocation_id());
        event.author = self.name().to_string();
        event.llm_response.content = Some(Content::new("model").with_text("hello"));
        Ok(Box::pin(futures::stream::iter([Ok(event)])))
    }
}

#[tokio::test]
async fn no_run_starts_for_a_subject_being_erased() {
    let visitor = VisitorId::generate();
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "shop".to_string(),
            user_id: visitor.to_string(),
            session_id: Some("s1".to_string()),
            state: Default::default(),
        })
        .await
        .unwrap();
    let registry = Arc::new(InMemorySubjectRegistry::new());
    let gate =
        ConsentGate::new(Arc::new(InMemoryConsentStore::new())).with_registry(registry.clone());
    let runner = Runner::builder()
        .app_name("shop")
        .agent(Arc::new(Echo) as Arc<dyn Agent>)
        .session_service(sessions as Arc<dyn SessionService>)
        .build()
        .unwrap();
    let config = || {
        RunConfig::builder().subject_kind(SubjectKind::Visitor).consent_gate(gate.clone()).build()
    };
    let run = |config: RunConfig| {
        runner.run_with_config(
            UserId::new(visitor.as_str()).unwrap(),
            SessionId::new("s1").unwrap(),
            Content::new("user").with_text("hi"),
            Some(config),
        )
    };

    let mut events = run(config()).await.expect("an active visitor runs");
    while let Some(event) = events.next().await {
        event.unwrap();
    }

    registry.set_state(&visitor.subject(), SubjectState::Erasing).await.unwrap();
    let refused = match run(config()).await {
        Err(error) => error,
        Ok(_) => panic!("a subject being erased gets no run"),
    };
    assert_eq!(refused.code, SUBJECT_ERASING_CODE);
}
