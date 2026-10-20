//! An agent can tell whether its run's user was authenticated (spec 3.1 §10 F5).
//!
//! A harness keeps standing rules and auto-approval inert unless the server authenticated
//! the user, so the context the runner hands an agent has to say which it is.

use std::sync::Arc;

use adk_core::{
    Agent, Content, Event, EventStream, InvocationContext, RequestContext, SessionId, UserId,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;

/// Answers with whether the identity it runs under was authenticated.
struct Reporter;

#[async_trait]
impl Agent for Reporter {
    fn name(&self) -> &str {
        "reporter"
    }
    fn description(&self) -> &str {
        "reports its identity"
    }
    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }
    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> adk_core::Result<EventStream> {
        let mut event = Event::new(ctx.invocation_id());
        event.author = self.name().to_string();
        event.llm_response.content = Some(Content::new("model").with_text(format!(
            "{} authenticated={}",
            ctx.user_id(),
            ctx.is_authenticated()
        )));
        Ok(Box::pin(futures::stream::iter([Ok(event)])))
    }
}

async fn report(request_context: Option<RequestContext>) -> String {
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "harness".to_string(),
            user_id: "alice".to_string(),
            session_id: Some("s1".to_string()),
            state: Default::default(),
        })
        .await
        .unwrap();
    let mut builder = Runner::builder()
        .app_name("harness")
        .agent(Arc::new(Reporter) as Arc<dyn Agent>)
        .session_service(sessions as Arc<dyn SessionService>);
    if let Some(request_context) = request_context {
        builder = builder.request_context(request_context);
    }
    let runner = builder.build().unwrap();
    let mut events = runner
        .run(UserId::new("alice").unwrap(), SessionId::new("s1").unwrap(), Content::new("user"))
        .await
        .unwrap();
    let mut texts = Vec::new();
    while let Some(event) = events.next().await {
        let event = event.unwrap();
        if let Some(content) = event.llm_response.content {
            texts.extend(content.parts.iter().filter_map(|part| part.text().map(str::to_string)));
        }
    }
    texts.join("")
}

#[tokio::test]
async fn a_run_with_a_request_context_reports_an_authenticated_identity() {
    let alice = RequestContext {
        user_id: "alice".to_string(),
        scopes: vec!["harness:shell".to_string()],
        metadata: Default::default(),
    };
    assert_eq!(report(Some(alice)).await, "alice authenticated=true");
}

#[tokio::test]
async fn a_run_without_one_reports_an_unauthenticated_identity() {
    assert_eq!(report(None).await, "alice authenticated=false");
}
