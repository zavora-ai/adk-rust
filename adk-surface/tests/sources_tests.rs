//! The engine over its other sources (spec 3.3 §6): a kernel reached through its control plane
//! (`ControlSource` over `ControlClient` on HTTP), and a harness with no kernel, whose decided
//! call resumes with an empty turn.

#![cfg(feature = "web")]

mod common;

use std::sync::Arc;
use std::time::Duration;

use adk_core::{ApprovalState, RunState};
use adk_kernel::control::{ControlClient, ControlPlaneConfig};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_surface::{
    Answer, AnswerLog, Caller, Command, CommandOutcome, ControlSource, InMemoryAnswerLog, Origin,
    Risk, SurfaceEngine, SurfaceKind, ThreadItem,
};
use common::*;
use serde_json::json;

const WITHIN: Duration = Duration::from_secs(15);

fn scripted(path: &str) -> Arc<ScriptedLlm> {
    Arc::new(ScriptedLlm::new(
        "scripted",
        vec![
            ScriptedResponse::function_call(
                "write_file",
                json!({ "path": path, "content": "INV-1" }),
            ),
            ScriptedResponse::text("Written."),
        ],
    ))
}

fn decide(id: &str, version: u64) -> Command {
    Command::Decide {
        decision_id: id.to_string(),
        version,
        answer: Answer::AllowOnce,
        edited_args: None,
        reason: None,
        expanded: true,
    }
}

/// The user the kernel's loopback HTTP control plane authenticates callers as.
fn os_user() -> String {
    ["USER", "USERNAME"]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|user| !user.trim().is_empty())
        .unwrap_or_else(|| "user".to_string())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_terminal_decides_through_the_control_plane() {
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let control = ControlPlaneConfig::default()
        .without_socket()
        .with_http(format!("127.0.0.1:{port}").parse().unwrap());
    let s = scenario_with(scripted("terminal.txt"), control, true).await;
    let answers = Arc::new(InMemoryAnswerLog::new());
    let client = ControlClient::http(format!("http://127.0.0.1:{port}"));
    let remote = SurfaceEngine::builder(Arc::new(ControlSource::new(client, APP, APP)))
        .answers(answers.clone())
        .poll_interval(Duration::from_millis(20))
        .build();
    let me = os_user();
    let terminal =
        Caller::human(me.clone(), SurfaceKind::Terminal, Origin::Keyboard).with_paired_local();

    let sent = remote
        .execute(&terminal, Command::Send { session_id: "t1".into(), text: "write it".into() })
        .await
        .unwrap();
    assert_eq!(sent, CommandOutcome::Sent { session_id: "t1".into() });
    let card = eventually("the card over the control plane", WITHIN, || async {
        remote.decisions(&terminal).await.unwrap().into_iter().next()
    })
    .await;
    // The control plane reports no risk: the card counts as high risk and offers no edit.
    assert_eq!(card.risk, Risk::High);
    let offered: Vec<Answer> = card.allowed.options.iter().map(|option| option.answer).collect();
    assert_eq!(offered, [Answer::AllowOnce, Answer::Deny]);
    assert_eq!(card.heading, "Write file terminal.txt");
    let thread = remote.thread(&terminal, "t1").await.unwrap();
    assert!(
        thread
            .iter()
            .flat_map(|entry| &entry.items)
            .any(|item| matches!(item, ThreadItem::Activity { tool, .. } if tool == "write_file"))
    );
    // An unpaired terminal sends the person to the home for a high-risk call.
    let unpaired = Caller::human(me.clone(), SurfaceKind::Terminal, Origin::Keyboard);
    let error = remote.execute(&unpaired, decide(&card.id, card.version)).await.unwrap_err();
    assert_eq!(error.code(), "surface.answer_not_allowed");

    let outcome = remote.execute(&terminal, decide(&card.id, card.version)).await.unwrap();
    assert!(matches!(outcome, CommandOutcome::Answered { .. }), "{outcome:?}");
    // A second answer through the control plane: decided, not retried.
    let again = remote.execute(&terminal, decide(&card.id, card.version)).await;
    // The control plane lists open decisions only, so a decided one is no longer found.
    assert_eq!(again.unwrap_err().code(), "surface.not_found");
    eventually("the process to complete", WITHIN, || async {
        let processes = s.kernel.processes().ps(Default::default()).await.unwrap();
        processes.into_iter().find(|process| process.run.state == RunState::Completed)
    })
    .await;
    assert_eq!(std::fs::read_to_string(s.path.join("terminal.txt")).unwrap(), "INV-1");
    let stored = s.service.get(&card.id).await.unwrap().unwrap();
    assert_eq!((stored.state, stored.decisions.len()), (ApprovalState::Approved, 1));
    let answer = answers.find(&card.id).await.unwrap().unwrap();
    assert_eq!(answer.via.surface, SurfaceKind::Terminal);
    s.kernel.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_harness_without_a_kernel_resumes_the_decided_call() {
    let s =
        scenario_with(scripted("plain.txt"), ControlPlaneConfig::default().without_socket(), false)
            .await;
    let sent = s
        .engine
        .execute(&web(), Command::Send { session_id: "p1".into(), text: "write it".into() })
        .await
        .unwrap();
    assert_eq!(sent, CommandOutcome::Sent { session_id: "p1".into() });
    let card = eventually("the held call's card", WITHIN, || async {
        s.engine.decisions(&web()).await.unwrap().into_iter().next()
    })
    .await;
    assert!(!s.path.join("plain.txt").exists());
    let outcome = s.engine.execute(&telegram(), decide(&card.id, card.version)).await.unwrap();
    assert!(matches!(outcome, CommandOutcome::Answered { .. }), "{outcome:?}");
    eventually("the resumed call to write the file", WITHIN, || async {
        std::fs::read_to_string(s.path.join("plain.txt")).ok()
    })
    .await;
    let thread = eventually("the turn to finish", WITHIN, || async {
        let thread = s.engine.thread(&web(), "p1").await.unwrap();
        thread
            .iter()
            .flat_map(|entry| &entry.items)
            .any(|item| matches!(item, ThreadItem::Message { text, .. } if text == "Written."))
            .then_some(thread)
    })
    .await;
    let writes = thread
        .iter()
        .flat_map(|entry| &entry.items)
        .filter(|item| matches!(item, ThreadItem::Result { tool, ok: true, .. } if tool == "write_file"))
        .count();
    assert_eq!(writes, 1);
}
