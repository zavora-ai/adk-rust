//! Tests for the coding-agent harness (feature `coding`).
#![cfg(feature = "coding")]

use std::sync::Arc;

use adk_agent::coding::{CodingAgent, TODOS_STATE_KEY, TodoTool};
use adk_core::{Content, SessionId, Tool, ToolContext, ToolEffect, UserId};
use adk_devtools::Workspace;
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};

// ── A trivial mock model so we can build an agent without network access ──

struct MockLlm;

#[async_trait]
impl adk_core::Llm for MockLlm {
    fn name(&self) -> &str {
        "mock-llm"
    }
    async fn generate_content(
        &self,
        _request: adk_core::LlmRequest,
        _stream: bool,
    ) -> adk_core::Result<adk_core::LlmResponseStream> {
        let s = async_stream::stream! {
            yield Ok(adk_core::LlmResponse {
                content: Some(adk_core::Content {
                    role: "model".to_string(),
                    parts: vec![adk_core::Part::Text { text: "done".to_string() }],
                }),
                usage_metadata: None,
                finish_reason: None,
                citation_metadata: None,
                partial: false,
                turn_complete: true,
                interrupted: false,
                error_code: None,
                error_message: None,
                provider_metadata: None,
                interaction_id: None,
                model: None,
                provider: None,
            });
        };
        Ok(Box::pin(s))
    }
}

// Minimal ToolContext for exercising TodoTool directly.
struct TestCtx;

#[async_trait]
impl adk_core::ReadonlyContext for TestCtx {
    fn invocation_id(&self) -> &str {
        "inv"
    }
    fn agent_name(&self) -> &str {
        "a"
    }
    fn user_id(&self) -> &str {
        "u"
    }
    fn app_name(&self) -> &str {
        "app"
    }
    fn session_id(&self) -> &str {
        "s"
    }
    fn branch(&self) -> &str {
        ""
    }
    fn user_content(&self) -> &adk_core::Content {
        static C: std::sync::OnceLock<adk_core::Content> = std::sync::OnceLock::new();
        C.get_or_init(|| adk_core::Content::new("user").with_text("hi"))
    }
}

#[async_trait]
impl adk_core::CallbackContext for TestCtx {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}

#[async_trait]
impl ToolContext for TestCtx {
    fn function_call_id(&self) -> &str {
        "call"
    }
    fn actions(&self) -> adk_core::EventActions {
        adk_core::EventActions::default()
    }
    fn set_actions(&self, _actions: adk_core::EventActions) {}
    async fn search_memory(&self, _query: &str) -> adk_core::Result<Vec<adk_core::MemoryEntry>> {
        Ok(vec![])
    }
}

#[test]
fn builder_requires_model_and_workspace() {
    let dir = tempfile::tempdir().unwrap();
    assert!(CodingAgent::builder().build().is_err());
    assert!(CodingAgent::builder().model(Arc::new(MockLlm)).build().is_err());
    assert!(
        CodingAgent::builder()
            .model(Arc::new(MockLlm))
            .workspace(Workspace::new(dir.path()))
            .build()
            .is_ok()
    );
}

#[test]
fn builds_a_named_agent() {
    let dir = tempfile::tempdir().unwrap();
    let coding = CodingAgent::builder()
        .name("repo-fixer")
        .model(Arc::new(MockLlm))
        .workspace(Workspace::new(dir.path()))
        .build()
        .unwrap();
    assert_eq!(coding.agent().name(), "repo-fixer");
    assert!(coding.todos().is_empty());
}

#[tokio::test]
async fn todo_tool_tracks_plan() {
    let tool = TodoTool::new();
    let ctx: Arc<dyn ToolContext> = Arc::new(TestCtx);
    let out = Tool::execute(
        &tool,
        ctx,
        json!({"todos": [
            {"content": "read the failing test", "status": "in_progress"},
            {"content": "fix it", "status": "pending"}
        ]}),
    )
    .await
    .unwrap();

    assert_eq!(out["total"], 2);
    assert_eq!(out["remaining"], 2);
    let items = tool.items();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].status, "in_progress");
}

#[test]
fn todo_tool_is_idempotent() {
    // Rewriting the same list leaves the same state, so the call is retry-safe and
    // not recorded in the action ledger.
    assert_eq!(TodoTool::new().effect(), ToolEffect::Idempotent);
}

const APP: &str = "coding-todos";
const USER: &str = "user";

/// Runs one turn in `session` to completion and returns its events.
async fn run_turn(runner: &Runner, session: &str, prompt: &str) -> Vec<adk_core::Event> {
    let mut stream = runner
        .run(
            UserId::new(USER).unwrap(),
            SessionId::new(session).unwrap(),
            Content::new("user").with_text(prompt),
        )
        .await
        .unwrap();
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event.unwrap());
    }
    events
}

/// A runner over `agent` with in-memory sessions `ids`.
async fn runner_with_sessions(
    agent: Arc<dyn adk_core::Agent>,
    ids: &[&str],
) -> (Runner, Arc<InMemorySessionService>) {
    let sessions = Arc::new(InMemorySessionService::new());
    for session in ids {
        sessions
            .create(CreateRequest {
                app_name: APP.to_string(),
                user_id: USER.to_string(),
                session_id: Some((*session).to_string()),
                state: Default::default(),
            })
            .await
            .unwrap();
    }
    let runner = Runner::builder()
        .app_name(APP)
        .agent(agent)
        .session_service(sessions.clone() as Arc<dyn SessionService>)
        .build()
        .unwrap();
    (runner, sessions)
}

async fn stored_todos(sessions: &InMemorySessionService, session: &str) -> Option<Value> {
    sessions
        .get(GetRequest {
            app_name: APP.to_string(),
            user_id: USER.to_string(),
            session_id: session.to_string(),
            num_recent_events: None,
            after: None,
        })
        .await
        .unwrap()
        .state()
        .get(TODOS_STATE_KEY)
}

#[tokio::test]
async fn each_session_keeps_its_own_todos() {
    let dir = tempfile::tempdir().unwrap();
    let model = Arc::new(ScriptedLlm::new(
        "scripted",
        [
            ScriptedResponse::function_call(
                "write_todos",
                json!({"todos": [{"content": "plan for one", "status": "in_progress"}]}),
            ),
            ScriptedResponse::text("planned one"),
            ScriptedResponse::function_call(
                "write_todos",
                json!({"todos": [
                    {"content": "plan for two", "status": "pending"},
                    {"content": "second step", "status": "pending"}
                ]}),
            ),
            ScriptedResponse::text("planned two"),
            ScriptedResponse::function_call(
                "write_todos",
                json!({"todos": [{"content": "plan for one", "status": "completed"}]}),
            ),
            ScriptedResponse::text("finished one"),
        ],
    ));
    let coding =
        CodingAgent::builder().model(model).workspace(Workspace::new(dir.path())).build().unwrap();
    let (runner, sessions) = runner_with_sessions(coding.agent(), &["one", "two"]).await;

    run_turn(&runner, "one", "plan one").await;
    run_turn(&runner, "two", "plan two").await;

    assert_eq!(
        stored_todos(&sessions, "one").await,
        Some(json!({
            "items": [{"id": "1", "content": "plan for one", "status": "in_progress"}],
            "revision": 1
        }))
    );
    assert_eq!(
        stored_todos(&sessions, "two").await,
        Some(json!({
            "items": [
                {"id": "1", "content": "plan for two", "status": "pending"},
                {"id": "2", "content": "second step", "status": "pending"}
            ],
            "revision": 1
        }))
    );

    run_turn(&runner, "one", "finish one").await;

    assert_eq!(
        stored_todos(&sessions, "one").await,
        Some(json!({
            "items": [{"id": "1", "content": "plan for one", "status": "completed"}],
            "revision": 2
        }))
    );
    assert_eq!(stored_todos(&sessions, "two").await.unwrap()["revision"], 1);
    // The process-wide accessor reports whichever session wrote last.
    assert_eq!(coding.todos()[0].status, "completed");
}

/// The function responses in `events`, by tool name.
fn responses(events: &[adk_core::Event]) -> std::collections::HashMap<String, Value> {
    events
        .iter()
        .filter_map(|event| event.llm_response.content.as_ref())
        .flat_map(|content| &content.parts)
        .filter_map(|part| match part {
            adk_core::Part::FunctionResponse { function_response, .. } => {
                Some((function_response.name.clone(), function_response.response.clone()))
            }
            _ => None,
        })
        .collect()
}

/// Whether a process whose command line contains `pattern` is running.
#[cfg(unix)]
fn running(pattern: &str) -> bool {
    std::process::Command::new("pgrep")
        .args(["-f", pattern])
        .output()
        .expect("pgrep must be available")
        .status
        .success()
}

#[cfg(unix)]
#[tokio::test]
async fn a_coding_agent_shared_by_two_sessions_keeps_them_apart() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "hello world\n").unwrap();
    std::fs::write(dir.path().join(".env"), "API_KEY=canary-secret\n").unwrap();
    let nanos =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().subsec_nanos();
    let sleep = format!("300.{}{:06}", std::process::id(), nanos % 1_000_000);
    let model = Arc::new(ScriptedLlm::new(
        "scripted",
        [
            // Session one starts a background process and reads a file.
            ScriptedResponse::function_calls([
                ("bash", json!({"command": format!("sleep {sleep} & echo started")})),
                ("read_file", json!({"path": "a.txt"})),
            ]),
            ScriptedResponse::text("one done"),
            // Session two edits that file without reading it, then goes after the secret.
            ScriptedResponse::function_calls([
                ("edit_file", json!({"path": "a.txt", "old_string": "hello", "new_string": "bye"})),
                ("grep", json!({"pattern": "canary"})),
                ("read_file", json!({"path": ".env"})),
            ]),
            ScriptedResponse::text("two done"),
        ],
    ));
    let workspace = Workspace::new(dir.path())
        .allow_bash(true)
        .bash_timeout(std::time::Duration::from_secs(20));
    let coding = CodingAgent::builder().model(model).workspace(workspace).build().unwrap();
    let (runner, _sessions) = runner_with_sessions(coding.agent(), &["one", "two"]).await;

    let started = std::time::Instant::now();
    let one = responses(&run_turn(&runner, "one", "start").await);
    assert!(started.elapsed() < std::time::Duration::from_secs(10), "bash waited for the sleep");
    assert_eq!(one["bash"]["stdout"], json!("started\n"));
    assert!(one["read_file"]["content"].as_str().unwrap().contains("hello world"));
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(!running(&format!("sleep {sleep}")), "the background sleep outlived the bash call");

    let two = responses(&run_turn(&runner, "two", "continue").await);
    assert!(two["edit_file"].get("error").is_some(), "session two edited an unread file");
    assert_eq!(std::fs::read_to_string(dir.path().join("a.txt")).unwrap(), "hello world\n");
    assert_eq!(two["grep"]["count"], 0, "grep searched .env");
    let refused = two["read_file"]["error"].as_str().unwrap();
    assert!(refused.contains("devtools.path_excluded"), "{refused}");
    assert!(!refused.contains("canary"));
}
