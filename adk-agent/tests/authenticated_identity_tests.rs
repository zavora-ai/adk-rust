//! Whether the run's user was authenticated reaches tools and callbacks inside an
//! `LlmAgent`, including under workflow agents.
//!
//! `ReadonlyContext::is_authenticated` defaults to `false`, so a context wrapper that does
//! not forward it makes an authenticated user look anonymous to everything it wraps. A
//! harness keeps standing rules and auto-approval inert for an unauthenticated user
//! (spec 3.1 §9.2 I6), so a dropped flag silently disables them.

use std::sync::{Arc, Mutex};

use adk_agent::{LlmAgentBuilder, LoopAgent, ParallelAgent, SequentialAgent};
use adk_core::{Agent, Content, RequestContext, Result, SessionId, Tool, ToolContext, UserId};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};

type Seen = Arc<Mutex<Vec<(&'static str, bool)>>>;

/// Records whether the tool's context reports an authenticated identity.
struct AuthProbe {
    seen: Seen,
}

#[async_trait]
impl Tool for AuthProbe {
    fn name(&self) -> &str {
        "auth_probe"
    }
    fn description(&self) -> &str {
        "reports whether the caller is authenticated"
    }
    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({ "type": "object", "properties": {} }))
    }
    async fn execute(&self, ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        self.seen.lock().unwrap().push(("tool", ctx.is_authenticated()));
        Ok(json!({ "authenticated": ctx.is_authenticated() }))
    }
}

fn worker(name: &str, seen: &Seen) -> Arc<dyn Agent> {
    let model = Arc::new(ScriptedLlm::new(
        "scripted",
        vec![
            ScriptedResponse::function_call("auth_probe", json!({})),
            ScriptedResponse::text("done"),
        ],
    ));
    let callback_seen = seen.clone();
    Arc::new(
        LlmAgentBuilder::new(name)
            .model(model)
            .tool(Arc::new(AuthProbe { seen: seen.clone() }))
            .after_tool_callback(Box::new(move |ctx| {
                let seen = callback_seen.clone();
                Box::pin(async move {
                    seen.lock().unwrap().push(("after_tool", ctx.is_authenticated()));
                    Ok(None)
                })
            }))
            .build()
            .unwrap(),
    )
}

async fn run(agent: Arc<dyn Agent>, request_context: Option<RequestContext>) {
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "app".to_string(),
            user_id: "alice".to_string(),
            session_id: Some("s1".to_string()),
            state: Default::default(),
        })
        .await
        .unwrap();
    let mut builder = Runner::builder()
        .app_name("app")
        .agent(agent)
        .session_service(sessions as Arc<dyn SessionService>);
    if let Some(request_context) = request_context {
        builder = builder.request_context(request_context);
    }
    let runner = builder.build().unwrap();
    let mut events = runner
        .run(
            UserId::new("alice").unwrap(),
            SessionId::new("s1").unwrap(),
            Content::new("user").with_text("go"),
        )
        .await
        .unwrap();
    while let Some(event) = events.next().await {
        event.unwrap();
    }
}

fn alice() -> RequestContext {
    RequestContext {
        user_id: "alice".to_string(),
        scopes: vec!["harness:shell".to_string()],
        metadata: Default::default(),
    }
}

#[tokio::test]
async fn tools_and_callbacks_of_an_llm_agent_see_the_authenticated_identity() {
    let seen: Seen = Arc::default();
    run(worker("worker", &seen), Some(alice())).await;
    assert_eq!(*seen.lock().unwrap(), vec![("tool", true), ("after_tool", true)]);
}

#[tokio::test]
async fn an_unauthenticated_run_stays_unauthenticated() {
    let seen: Seen = Arc::default();
    run(worker("worker", &seen), None).await;
    assert_eq!(*seen.lock().unwrap(), vec![("tool", false), ("after_tool", false)]);
}

#[tokio::test]
async fn workflow_agents_forward_the_authenticated_identity() {
    let seen: Seen = Arc::default();
    let sequential: Arc<dyn Agent> =
        Arc::new(SequentialAgent::new("sequential", vec![worker("in_sequence", &seen)]));
    run(sequential, Some(alice())).await;
    let parallel: Arc<dyn Agent> =
        Arc::new(ParallelAgent::new("parallel", vec![worker("in_parallel", &seen)]));
    run(parallel, Some(alice())).await;
    let looping: Arc<dyn Agent> =
        Arc::new(LoopAgent::new("loop", vec![worker("in_loop", &seen)]).with_max_iterations(1));
    run(looping, Some(alice())).await;
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 6, "every workflow ran its worker once: {seen:?}");
    assert!(seen.iter().all(|(_, authenticated)| *authenticated), "{seen:?}");
}
