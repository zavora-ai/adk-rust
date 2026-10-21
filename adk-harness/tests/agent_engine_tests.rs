//! The harness as an Agent Engine container (spec 3.1 H9, §8.5): the app
//! `serve_agent_engine` serves, driven through its class-method dispatch on a loopback port
//! (no GCP call). A held call goes to the harness's approval inbox, is decided there, and
//! the next `stream_query` of the session runs it once.

use std::sync::Arc;

use adk_core::principal::SubjectRef;
use adk_core::{
    ApprovalDecision, ApprovalPolicy, ApprovalService, ApproverSelector, Event,
    InMemoryApprovalStore, InboxQuery, StoreApprovalService,
};
use adk_harness::{HarnessAgent, HarnessMode, InstructionsConfig};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_server::agent_engine::{AgentEngineOptions, build_agent_engine_app};
use serde_json::{Value, json};

const OPERATORS: &str = "operators";

fn approval_service() -> Arc<dyn ApprovalService> {
    let routing = ApprovalPolicy::new([ApproverSelector::group(OPERATORS)]).allow_self_approval();
    Arc::new(StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new())).with_router(routing))
}

/// The engine app on a loopback port, and its base URL.
async fn serve(harness: &HarnessAgent) -> String {
    let app = build_agent_engine_app(harness.agent(), AgentEngineOptions::new()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await });
    format!("http://{address}")
}

/// One `stream_query` dispatch: the newline-delimited events of the turn.
async fn stream_query(base: &str, session: &str, message: &str) -> Vec<Event> {
    let body = json!({
        "class_method": "stream_query",
        "input": { "user_id": "ana", "session_id": session, "message": message },
    });
    let response = reqwest::Client::new()
        .post(format!("{base}/api/stream_reasoning_engine"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success(), "{}", response.status());
    let text = response.text().await.unwrap();
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str::<Event>(line).unwrap_or_else(|error| panic!("{line}: {error}"))
        })
        .collect()
}

fn bash_answers(events: &[Event]) -> Vec<Value> {
    events
        .iter()
        .filter_map(|event| event.llm_response.content.as_ref())
        .flat_map(|content| content.parts.iter())
        .filter_map(|part| match part {
            adk_core::Part::FunctionResponse { function_response, .. }
                if function_response.name == "bash" =>
            {
                Some(function_response.response.clone())
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_held_call_decided_in_the_inbox_runs_once_on_the_next_stream_query() {
    let workspace = tempfile::tempdir().unwrap();
    let service = approval_service();
    let harness = HarnessAgent::builder("harness")
        .model(Arc::new(ScriptedLlm::new(
            "scripted",
            vec![
                ScriptedResponse::function_call(
                    "bash",
                    json!({ "command": "printf 'ran\\n' >> log.txt" }),
                ),
                ScriptedResponse::text("Waiting for approval."),
                ScriptedResponse::text("Logged."),
                ScriptedResponse::text("Nothing else ran."),
            ],
        )))
        .workspace(adk_devtools::Workspace::new(workspace.path()).allow_bash(true))
        .project_instructions(InstructionsConfig {
            user_file: None,
            ..InstructionsConfig::default()
        })
        .mode(HarnessMode::Ask)
        .approval_service(service.clone())
        .build()
        .unwrap();
    let base = serve(&harness).await;
    let log = || std::fs::read_to_string(workspace.path().join("log.txt")).unwrap_or_default();

    // The turn holds the host command and ends; the hold is in the stream and the inbox.
    let held = stream_query(&base, "s1", "log a line").await;
    let request = held
        .iter()
        .find_map(|event| event.actions.tool_confirmation.clone())
        .expect("the held call is in the event stream");
    assert_eq!(request.tool_name, "bash");
    assert_eq!(log(), "", "held, not run");
    let mut query = InboxQuery::default();
    query.groups = vec![OPERATORS.into()];
    let inbox = service.inbox(query).await.unwrap();
    assert_eq!(inbox.items.len(), 1);
    let item = &inbox.items[0];
    assert_eq!(item.fingerprint, request.fingerprint());

    // An operator approves it; the next query of the session resumes it.
    service
        .decide(
            &item.id,
            item.version,
            ApprovalDecision::approve(SubjectRef::human("operator")).with_groups([OPERATORS]),
        )
        .await
        .unwrap();
    let resumed = stream_query(&base, "s1", "").await;
    assert_eq!(log(), "ran\n");
    assert!(
        bash_answers(&resumed).iter().any(|answer| !adk_core::is_confirmation_pending(answer)),
        "the resumed call's result is in the stream"
    );

    // Another query runs nothing again.
    stream_query(&base, "s1", "anything else?").await;
    assert_eq!(log(), "ran\n", "exactly once");
}
