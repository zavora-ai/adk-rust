//! `Harness::send_requested`: a call a person asked for through something the agent drew
//! waits for their decision as the agent, whatever answered it before (spec 3.3 §7.4 rule 5,
//! wave 4 decisions 12 and 13).

#![cfg(feature = "runner")]

use crate::common;

use adk_core::{
    ApprovalScope, RequestContext, RequestedCall, RequestedVia, SubjectRef, ToolApproval,
};
use adk_harness::{HarnessDecision, HarnessMode, PromptAnswer};
use adk_model::ScriptedResponse;
use common::{Outcome, collect, confirmation_requests, fixture, outcome, response_of};
use serde_json::{Value, json};

fn done() -> ScriptedResponse {
    ScriptedResponse::text("done")
}

/// The person `alice`, signed in.
fn alice() -> RequestContext {
    RequestContext {
        user_id: "alice".to_string(),
        scopes: Vec::new(),
        metadata: Default::default(),
    }
}

/// `write_file` of `content` to `path`, asked for through version 1 of a drawn form.
fn requested_write(path: &str, content: &str) -> RequestedCall {
    RequestedCall::new(
        "write_file",
        json!({ "path": path, "content": content }),
        SubjectRef::human("alice"),
        RequestedVia::form("note-form", 1, "save"),
    )
}

/// The press is never the approval. In Go ahead, which allows edits, with a standing rule
/// the person made for `write_file`, an approval recorded for the identical call and an
/// approval prompt that would say yes, a requested call still waits, as the agent, with no
/// model call; Allow once then runs it once.
#[tokio::test]
async fn a_requested_call_waits_in_go_ahead_with_a_rule_an_approval_and_the_prompt() {
    let prompter = common::ScriptedPrompter::new();
    prompter.then(|prompt| PromptAnswer::ApproveAndRemember(prompt.suggestions[0].clone()));
    for _ in 0..4 {
        prompter.approve_once();
    }
    let script = vec![
        ScriptedResponse::function_call(
            "write_file",
            json!({ "path": "a.txt", "content": "from the model" }),
        ),
        done(),
        done(),
        done(),
        done(),
    ];
    let f = fixture(script, |b| b.prompter(prompter.clone())).await;
    let scope = ApprovalScope::new(common::APP, "alice", f.session.to_string());

    // A standing rule for `write_file`, made on the model's own call.
    let events = f.send("write a note").await;
    assert_eq!(outcome(&response_of(&events, "write_file")), Outcome::Executed);
    assert_eq!(prompter.count(), 1);
    f.harness.set_mode(&f.user, &f.session, HarnessMode::Auto).await.unwrap();

    let send = |call: RequestedCall| async {
        let stream =
            f.harness.send_requested(&f.user, &f.session, "", call, alice()).await.unwrap();
        collect(stream).await
    };
    let resume = || async {
        collect(f.harness.resume_as(&f.user, &f.session, alice()).await.unwrap()).await
    };

    let model_calls = f.model.requests().len();
    let first = requested_write("b.txt", "from the form");
    let events = send(first.clone()).await;
    let held = confirmation_requests(&events);
    assert_eq!(held.len(), 1, "the call waits for the person");
    assert_eq!(held[0].function_call_id.as_deref(), Some(first.function_call_id.as_str()));
    assert_eq!(
        held[0].acting.as_ref().map(|acting| acting.agent.id.as_str()),
        Some("urn:adk:agent:harness-tests/harness"),
        "held as the agent"
    );
    assert_eq!(f.model.requests().len(), model_calls, "no model call");
    let fingerprint = held[0].fingerprint();

    // The person says no to it; then an approval of the identical call is recorded.
    f.harness
        .decide(&f.user, &f.session, &fingerprint, HarnessDecision::Deny { message: None })
        .await
        .unwrap();
    let events = resume().await;
    assert_eq!(outcome(&response_of(&events, "write_file")), Outcome::Denied);
    let store = f.harness.agent().runtime().approval_store().clone();
    store.decide(&scope, &fingerprint, ToolApproval::approve()).await.unwrap();

    let model_calls = f.model.requests().len();
    let events = send(requested_write("b.txt", "from the form")).await;
    assert_eq!(confirmation_requests(&events).len(), 1, "the identical call waits again");
    let pending = f.harness.pending_approvals(&f.user, &f.session).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].fingerprint, fingerprint);
    assert!(!f.path("b.txt").exists(), "nothing ran before the person's decision");
    assert_eq!(prompter.count(), 1, "the approval prompt was never asked");
    assert_eq!(f.model.requests().len(), model_calls, "no model call");

    f.harness.decide(&f.user, &f.session, &fingerprint, HarnessDecision::Approve).await.unwrap();
    let events = resume().await;
    assert_eq!(outcome(&response_of(&events, "write_file")), Outcome::Executed);
    assert_eq!(std::fs::read_to_string(f.path("b.txt")).unwrap(), "from the form");
    assert!(f.harness.pending_approvals(&f.user, &f.session).await.unwrap().is_empty());
    assert!(confirmation_requests(&resume().await).is_empty(), "nothing waits any more");
}

/// Just look refuses edits, so a requested edit is refused with no hold and no model call.
#[tokio::test]
async fn just_look_refuses_a_requested_call() {
    let f = fixture(vec![done()], |b| b).await;
    f.harness.set_mode(&f.user, &f.session, HarnessMode::ReadOnly).await.unwrap();

    let stream = f
        .harness
        .send_requested(&f.user, &f.session, "", requested_write("c.txt", "x"), alice())
        .await
        .unwrap();
    let events = collect(stream).await;

    let response: Value = response_of(&events, "write_file");
    assert_eq!(outcome(&response), Outcome::Denied, "{response}");
    assert!(confirmation_requests(&events).is_empty());
    assert!(f.harness.pending_approvals(&f.user, &f.session).await.unwrap().is_empty());
    assert!(f.model.requests().is_empty(), "no model call");
    assert!(!f.path("c.txt").exists());
}

/// Spec 3.3 §7.4 rule 5: a looser mode never answers a call a person asked for, so it keeps
/// waiting for them when the mode loosens, and their Allow once still runs it.
#[tokio::test]
async fn loosening_the_mode_keeps_a_requested_call_waiting() {
    let f = fixture(vec![done(), done()], |b| b).await;
    let call = requested_write("d.txt", "from the form");
    let events =
        collect(f.harness.send_requested(&f.user, &f.session, "", call, alice()).await.unwrap())
            .await;
    let held = confirmation_requests(&events);
    assert_eq!(held.len(), 1, "the call waits for the person");

    f.harness.set_mode(&f.user, &f.session, HarnessMode::Auto).await.unwrap();

    let pending = f.harness.pending_approvals(&f.user, &f.session).await.unwrap();
    assert_eq!(pending.len(), 1, "a looser mode does not cancel it");
    assert_eq!(pending[0].fingerprint, held[0].fingerprint());
    f.harness
        .decide(&f.user, &f.session, &held[0].fingerprint(), HarnessDecision::Approve)
        .await
        .unwrap();
    let events = collect(f.harness.resume_as(&f.user, &f.session, alice()).await.unwrap()).await;
    assert_eq!(outcome(&response_of(&events, "write_file")), Outcome::Executed);
    assert_eq!(std::fs::read_to_string(f.path("d.txt")).unwrap(), "from the form");
}

/// Spec 3.3 §7.4 rule 5: no confirmation handler answers a call a person asked for, so one
/// whose decision a run took before the call began is held for the person again, on a
/// harness with an approval prompt too.
#[tokio::test]
async fn a_stranded_requested_call_waits_again_with_a_prompt_installed() {
    let prompter = common::ScriptedPrompter::new();
    let f = fixture(vec![done(), done()], |b| b.prompter(prompter.clone())).await;
    let call = requested_write("e.txt", "from the form");
    let events =
        collect(f.harness.send_requested(&f.user, &f.session, "", call, alice()).await.unwrap())
            .await;
    let held = confirmation_requests(&events);
    assert_eq!(held.len(), 1, "the call waits for the person");
    let fingerprint = held[0].fingerprint();

    // A run took the person's yes, and the process stopped before the call began.
    let store = f.harness.agent().runtime().approval_store().clone();
    let scope = ApprovalScope::new(common::APP, "alice", f.session.to_string());
    store.decide(&scope, &fingerprint, ToolApproval::approve()).await.unwrap();
    assert!(store.take_approval(&scope, &fingerprint).await.unwrap().is_some());
    assert!(f.harness.pending_approvals(&f.user, &f.session).await.unwrap().is_empty());

    collect(f.harness.resume_as(&f.user, &f.session, alice()).await.unwrap()).await;

    let pending = f.harness.pending_approvals(&f.user, &f.session).await.unwrap();
    assert_eq!(pending.len(), 1, "the person can decide again");
    assert_eq!(pending[0].fingerprint, fingerprint);
    assert_eq!(prompter.count(), 0, "the prompt never answers it");
    assert!(!f.path("e.txt").exists(), "nothing ran");
}
