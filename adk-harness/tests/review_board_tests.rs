//! Review rounds as runner roots under the harness layer (spec 3.1 §7.10 "group work"):
//! `HarnessTeam::review_round_with` and `HarnessAgent::governed_root`.

#![cfg(all(feature = "team", feature = "runner"))]

use crate::common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use adk_agent::{BlackboardHistoryPolicy, BlackboardPolicy};
use adk_core::audit_log::{AuditRecordKind, InMemoryAuditRecorder};
use adk_core::{Agent, Content, Event, LlmRequest, SessionId, UserId};
use adk_harness::{HarnessAgent, HarnessMode, MemberKind, MemberSpec, MemberTools};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use common::no_user_file;
use futures::StreamExt;
use serde_json::json;

const APP: &str = "review-board-tests";

fn seat(name: &str, model: Arc<ScriptedLlm>) -> MemberSpec {
    MemberSpec {
        name: name.to_string(),
        description: format!("Reviews the change for {name}."),
        kind: MemberKind::Local {
            instruction: format!("Review the change for {name}. End with a VERDICT line."),
            model: Some(model),
        },
        tools: MemberTools::ReadOnly,
        skills: false,
        max_iterations: 8,
        timeout: Duration::from_secs(600),
        output_schema: None,
    }
}

/// A read-only, shell-less harness whose only members are the two scripted seats.
fn board(dir: &Path, first: Arc<ScriptedLlm>, second: Arc<ScriptedLlm>) -> HarnessAgent {
    std::fs::write(dir.join("README.md"), "a project\n").unwrap();
    HarnessAgent::builder("ultracode_board")
        .model(Arc::new(ScriptedLlm::new("coordinator", Vec::<ScriptedResponse>::new())))
        .workspace(adk_devtools::Workspace::new(dir))
        .project_instructions(no_user_file())
        .mode(HarnessMode::ReadOnly)
        .without_shell()
        .without_default_members()
        .without_compaction()
        .member(seat("review_correctness", first))
        .member(seat("review_style", second))
        .build()
        .unwrap()
}

/// Runs `round` as the governed root of a runner over a fresh session.
async fn run_round(
    harness: &HarnessAgent,
    round: Arc<dyn Agent>,
    audit: Option<InMemoryAuditRecorder>,
) -> Vec<adk_core::Result<Event>> {
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.into(),
            user_id: "alice".into(),
            session_id: Some("board".into()),
            state: Default::default(),
        })
        .await
        .unwrap();
    let mut builder = harness.configure_runner(
        Runner::builder()
            .app_name(APP)
            .agent(harness.governed_root(round))
            .session_service(sessions),
    );
    if let Some(audit) = audit {
        builder = builder.audit_log(Arc::new(audit));
    }
    let runner = builder.build().unwrap();
    let stream = runner
        .run(
            UserId::new("alice").unwrap(),
            SessionId::new("board").unwrap(),
            Content::new("user").with_text("Review the workspace against the task."),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(30), stream.collect::<Vec<_>>())
        .await
        .expect("the round finishes")
}

fn texts(request: &LlmRequest, role: &str) -> Vec<String> {
    request
        .contents
        .iter()
        .filter(|content| content.role == role)
        .flat_map(|content| content.parts.iter())
        .filter_map(|part| part.text().map(str::to_string))
        .collect()
}

/// Every file under `root` with its bytes, sorted.
fn tree(root: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, root, out);
            } else {
                let name = path.strip_prefix(root).unwrap().display().to_string();
                out.push((name, std::fs::read(&path).unwrap_or_default()));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

#[tokio::test]
async fn a_governed_round_attributes_speakers() {
    let dir = tempfile::tempdir().unwrap();
    let first = Arc::new(ScriptedLlm::new(
        "first",
        [
            ScriptedResponse::function_call("read_file", json!({ "path": "README.md" })),
            ScriptedResponse::text("The error path is unhandled.\nVERDICT: changes"),
        ],
    ));
    let second = Arc::new(ScriptedLlm::new("second", [ScriptedResponse::text("VERDICT: approve")]));
    let harness = board(dir.path(), first.clone(), second.clone());
    let round = harness
        .runtime()
        .team()
        .unwrap()
        .review_round_with(
            "ultracode_board",
            &["review_correctness", "review_style"],
            BlackboardPolicy {
                max_rounds: 1,
                history: BlackboardHistoryPolicy::Full,
                ..BlackboardPolicy::default()
            },
        )
        .unwrap();
    let events = run_round(&harness, round, None).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");

    let request = second.requests().into_iter().next().expect("the second seat spoke");
    let context = texts(&request, "user");
    assert!(
        context.iter().any(|text| text.starts_with("[review_correctness] said:")
            && text.contains("The error path is unhandled.")),
        "the first seat's note reaches the second as other-agent context: {context:#?}"
    );
    assert!(
        context.iter().any(|text| text.starts_with("[review_correctness] called tool `read_file`")),
        "the first seat's tool call reaches the second as context: {context:#?}"
    );
    assert!(
        !texts(&request, "model").iter().any(|text| text.contains("The error path is unhandled")),
        "the first seat's note must never be the second seat's own model turn"
    );
    let own_calls = request
        .contents
        .iter()
        .filter(|content| content.role == "model")
        .any(adk_core::Content::has_function_calls);
    assert!(!own_calls, "another seat's tool call must not be replayed as the second seat's call");
}

#[tokio::test]
async fn a_governed_round_is_read_only_and_audited() {
    let dir = tempfile::tempdir().unwrap();
    let first = Arc::new(ScriptedLlm::new(
        "first",
        [
            ScriptedResponse::function_calls([
                ("write_file", json!({ "path": "README.md", "content": "rewritten" })),
                ("bash", json!({ "command": "touch made-by-bash" })),
                ("read_file", json!({ "path": "README.md" })),
            ]),
            ScriptedResponse::text("VERDICT: changes"),
        ],
    ));
    let second = Arc::new(ScriptedLlm::new("second", [ScriptedResponse::text("VERDICT: approve")]));
    let harness = board(dir.path(), first.clone(), second.clone());
    let before = tree(dir.path());
    let round = harness
        .runtime()
        .team()
        .unwrap()
        .review_round("ultracode_board", &["review_correctness", "review_style"], 1)
        .unwrap();
    let audit = InMemoryAuditRecorder::new();
    let events = run_round(&harness, round, Some(audit.clone())).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");

    let allowed = ["read_file", "glob", "grep", "write_todos", "update_todo"];
    for request in first.requests().iter().chain(second.requests().iter()) {
        let declared: Vec<&String> = request.tools.keys().collect();
        assert!(
            declared.iter().all(|name| allowed.contains(&name.as_str())),
            "a seat declares only read tools: {declared:?}"
        );
    }
    assert_eq!(tree(dir.path()), before, "a seat's write_file and bash change nothing");

    let records = audit.records();
    let governed: Vec<_> = records
        .iter()
        .filter(|record| record.kind == AuditRecordKind::ToolCall && record.action == "read_file")
        .collect();
    assert!(!governed.is_empty(), "the seat's read is a governed call: {records:#?}");
    assert!(
        governed.iter().all(|record| record
            .policy_ref
            .as_deref()
            .is_some_and(|policy| policy.contains("adk-harness"))),
        "{governed:#?}"
    );
    for action in ["harness.member.spawned", "harness.member.finished"] {
        assert_eq!(
            records.iter().filter(|record| record.action == action).count(),
            2,
            "{action} once per seat"
        );
    }
    assert!(
        records.iter().any(|record| record.action == "harness.spend"),
        "the governed root meters its run"
    );
    assert!(harness.runtime().spend(None).model_calls >= 3, "the round's model calls are counted");
}

#[tokio::test]
async fn review_round_with_applies_its_policy() {
    let dir = tempfile::tempdir().unwrap();
    let first = Arc::new(ScriptedLlm::new(
        "first",
        [ScriptedResponse::text("VERDICT: changes"), ScriptedResponse::text("VERDICT: approve")],
    ));
    let second = Arc::new(ScriptedLlm::new(
        "second",
        [ScriptedResponse::text("VERDICT: approve"), ScriptedResponse::text("VERDICT: approve")],
    ));
    let harness = board(dir.path(), first.clone(), second.clone());
    let team = harness.runtime().team().unwrap();
    let two_rounds = BlackboardPolicy {
        max_rounds: 2,
        history: BlackboardHistoryPolicy::Full,
        ..BlackboardPolicy::default()
    };
    let round = team
        .review_round_with("ultracode_board", &["review_correctness", "review_style"], two_rounds)
        .unwrap();
    let events = run_round(&harness, round, None).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    assert_eq!((first.requests().len(), second.requests().len()), (2, 2));
    let last = second.requests().pop().unwrap();
    assert!(
        texts(&last, "model").iter().any(|text| text.contains("VERDICT: approve")),
        "in round 2 a seat sees its own round-1 turn as its own"
    );

    let zero = BlackboardPolicy { max_rounds: 0, ..BlackboardPolicy::default() };
    let Err(error) = team.review_round_with("ultracode_board", &["review_correctness"], zero)
    else {
        panic!("a round of zero rounds is refused");
    };
    assert_eq!(error.category, adk_core::ErrorCategory::InvalidInput, "{error}");

    let one = Arc::new(ScriptedLlm::new("one", [ScriptedResponse::text("VERDICT: approve")]));
    let other = Arc::new(ScriptedLlm::new("other", [ScriptedResponse::text("VERDICT: approve")]));
    let dir = tempfile::tempdir().unwrap();
    let harness = board(dir.path(), one.clone(), other.clone());
    let round = harness
        .runtime()
        .team()
        .unwrap()
        .review_round("ultracode_board", &["review_correctness", "review_style"], 1)
        .unwrap();
    let events = run_round(&harness, round, None).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    assert_eq!((one.requests().len(), other.requests().len()), (1, 1));
}

#[tokio::test]
async fn a_governed_root_applies_the_harness_layer() {
    let dir = tempfile::tempdir().unwrap();
    let first = Arc::new(ScriptedLlm::new("first", Vec::<ScriptedResponse>::new()));
    let second = Arc::new(ScriptedLlm::new("second", Vec::<ScriptedResponse>::new()));
    let harness = board(dir.path(), first.clone(), second);
    let round = harness
        .runtime()
        .team()
        .unwrap()
        .review_round("ultracode_board", &["review_correctness", "review_style"], 1)
        .unwrap();
    let root = harness.governed_root(round);
    assert_eq!(root.name(), "ultracode_board");
    assert!(!root.supports_agent_transfer());

    let mut config = adk_core::RunConfig::default();
    root.configure_run(root.name(), &mut config);
    assert!(adk_harness::has_harness_layer(config.tool_policy.as_deref().unwrap()));
    assert!(first.requests().is_empty());
}
