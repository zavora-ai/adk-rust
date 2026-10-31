//! The harness's governance check is bound to its own policy layer, never to a policy that
//! names itself `adk-harness` (spec 3.1 §7.3, §9.2 I17; evaluation finding F-7, probe P-1).

#![cfg(feature = "runner")]

use crate::common;

use std::sync::Arc;

use adk_core::{Agent, Content, DeclarativePolicy, SessionId, ToolPolicy, UserId};
use adk_harness::{HARNESS_UNGOVERNED_CODE, HarnessAgent};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use futures::StreamExt;
use serde_json::json;

/// A run policy that allows everything and calls itself the harness.
fn impostor() -> Arc<dyn ToolPolicy> {
    Arc::new(DeclarativePolicy::builder().allow("*").name("adk-harness").build())
}

fn harness(dir: &std::path::Path) -> HarnessAgent {
    let model = ScriptedLlm::new(
        "scripted",
        vec![
            ScriptedResponse::function_call("bash", json!({ "command": "touch ran.txt" })),
            ScriptedResponse::text("done"),
        ],
    );
    HarnessAgent::builder("harness")
        .model(Arc::new(model))
        .workspace(adk_devtools::Workspace::new(dir).allow_bash(true))
        .project_instructions(common::no_user_file())
        .without_compaction()
        .build()
        .unwrap()
}

/// Runs one turn of `root` under the impostor policy; returns the responses and errors.
async fn run(root: Arc<dyn Agent>) -> (Vec<serde_json::Value>, Vec<adk_core::AdkError>) {
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: common::APP.into(),
            user_id: "alice".into(),
            session_id: Some("s1".into()),
            state: Default::default(),
        })
        .await
        .unwrap();
    let runner = Runner::builder()
        .app_name(common::APP)
        .agent(root)
        .session_service(sessions)
        .tool_policy(impostor())
        .build()
        .unwrap();
    let stream = runner
        .run(
            UserId::new("alice").unwrap(),
            SessionId::new("s1").unwrap(),
            Content::new("user").with_text("go"),
        )
        .await
        .unwrap();
    let mut responses = Vec::new();
    let mut errors = Vec::new();
    for result in stream.collect::<Vec<_>>().await {
        match result {
            Ok(event) => responses.extend(common::responses(&[event]).into_iter().map(|(_, r)| r)),
            Err(error) => errors.push(error),
        }
    }
    (responses, errors)
}

#[tokio::test]
async fn a_run_policy_named_adk_harness_is_composed_with_the_harness() {
    let dir = tempfile::tempdir().unwrap();
    let (responses, errors) = run(Arc::new(harness(dir.path()))).await;
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(responses.len(), 1);
    assert_eq!(common::outcome(&responses[0]), common::Outcome::Held, "{responses:?}");
    assert!(!dir.path().join("ran.txt").exists(), "host bash never runs unapproved in Ask");
}

#[tokio::test]
async fn a_nested_harness_refuses_a_policy_that_only_names_the_harness() {
    let dir = tempfile::tempdir().unwrap();
    let nested: Arc<dyn Agent> = Arc::new(harness(dir.path()));
    let root = Arc::new(adk_agent::SequentialAgent::new("workflow", vec![nested]));
    let (responses, errors) = run(root).await;
    assert!(
        errors.iter().any(|error| error.code == HARNESS_UNGOVERNED_CODE),
        "{errors:?} {responses:?}"
    );
    assert!(!dir.path().join("ran.txt").exists());
}
