//! An operator policy's `RequireApproval` reaches the person, or the hold path without a
//! live surface, and is never turned into a denial (spec 3.1 §7.2 `operator_policy`, §7.3
//! compose rule; evaluation finding F-3, probe P-7).

#![cfg(feature = "runner")]

use crate::common;

use std::sync::Arc;

use adk_core::{ArgPredicate, DeclarativePolicy, PolicyRule, ToolPolicy};
use adk_harness::{HarnessDecision, PromptAnswer};
use adk_model::ScriptedResponse;
use common::{Outcome, fixture, outcome, response_of, responses};
use serde_json::json;

fn done() -> ScriptedResponse {
    ScriptedResponse::text("done")
}

fn read_needs_approval() -> Arc<dyn ToolPolicy> {
    Arc::new(
        DeclarativePolicy::builder()
            .require_approval("read_file", "reads need the operator's approval")
            .allow("*")
            .name("ops")
            .build(),
    )
}

#[tokio::test]
async fn an_operator_approval_requirement_prompts_the_person() {
    let prompter = common::ScriptedPrompter::new();
    prompter.approve_once();
    let f = fixture(
        vec![ScriptedResponse::function_call("read_file", json!({ "path": "notes.txt" })), done()],
        |b| b.operator_policy(read_needs_approval()).prompter(prompter.clone()),
    )
    .await;
    std::fs::write(f.path("notes.txt"), "operator approved\n").unwrap();

    let events = f.send("read").await;
    assert_eq!(prompter.count(), 1, "the operator's approval is asked for");
    let response = response_of(&events, "read_file");
    assert_eq!(outcome(&response), Outcome::Executed, "{:#?}", responses(&events));
    assert!(response.to_string().contains("operator approved"), "{response}");
    let prompts = prompter.prompts.lock().unwrap();
    assert!(prompts[0].suggestions.is_empty(), "a harness rule cannot answer the operator");
}

#[tokio::test]
async fn a_person_denying_an_operator_approval_is_reported_as_their_denial() {
    let prompter = common::ScriptedPrompter::new();
    prompter.then(|_| PromptAnswer::Deny { message: Some("not that file".into()) });
    let f = fixture(
        vec![ScriptedResponse::function_call("read_file", json!({ "path": "notes.txt" })), done()],
        |b| b.operator_policy(read_needs_approval()).prompter(prompter.clone()),
    )
    .await;
    std::fs::write(f.path("notes.txt"), "secret\n").unwrap();

    let events = f.send("read").await;
    assert_eq!(prompter.count(), 1);
    let response = response_of(&events, "read_file").to_string();
    assert!(response.contains("the approver declined it: not that file"), "{response}");
    assert!(!response.contains("did not evaluate"), "{response}");
}

#[tokio::test]
async fn without_a_prompter_an_operator_approval_is_held_for_the_inbox() {
    let f = fixture(
        vec![
            ScriptedResponse::function_call("read_file", json!({ "path": "notes.txt" })),
            done(),
            done(),
        ],
        |b| b.operator_policy(read_needs_approval()),
    )
    .await;
    std::fs::write(f.path("notes.txt"), "held then read\n").unwrap();

    let events = f.send("read").await;
    assert_eq!(outcome(&response_of(&events, "read_file")), Outcome::Held);
    let pending = f.harness.pending_approvals(&f.user, &f.session).await.unwrap();
    assert_eq!(pending.len(), 1);
    f.harness
        .decide(&f.user, &f.session, &pending[0].fingerprint, HarnessDecision::Approve)
        .await
        .unwrap();
    let events = f.resume().await;
    let response = response_of(&events, "read_file");
    assert!(response.to_string().contains("held then read"), "{response}");
}

#[tokio::test]
async fn a_standing_rule_never_answers_an_operator_approval() {
    // The operator asks about `echo hello again` only; the harness asks about every host
    // command until a rule covers it.
    let operator: Arc<dyn ToolPolicy> = Arc::new(
        DeclarativePolicy::builder()
            .rule(
                PolicyRule::require_approval("bash", "the operator reviews this command")
                    .when(ArgPredicate::starts_with("/command", "echo hello again")),
            )
            .allow("*")
            .name("ops")
            .build(),
    );
    let prompter = common::ScriptedPrompter::new();
    prompter.then(|prompt| {
        let rule = prompt
            .suggestions
            .iter()
            .find(|suggestion| suggestion.label.contains("echo hello"))
            .expect("a plain command offers a prefix rule")
            .clone();
        PromptAnswer::ApproveAndRemember(rule)
    });
    prompter.approve_once();
    let script = vec![
        ScriptedResponse::function_call("bash", json!({ "command": "echo hello there" })),
        done(),
        ScriptedResponse::function_call("bash", json!({ "command": "echo hello world" })),
        done(),
        ScriptedResponse::function_call("bash", json!({ "command": "echo hello again" })),
        done(),
    ];
    let f = fixture(script, |b| b.operator_policy(operator).prompter(prompter.clone())).await;

    f.send("first").await;
    assert_eq!(prompter.count(), 1);
    let events = f.send("second").await;
    assert_eq!(outcome(&response_of(&events, "bash")), Outcome::Executed);
    assert_eq!(prompter.count(), 1, "the rule answers the harness's own approval");

    let events = f.send("third").await;
    assert_eq!(prompter.count(), 2, "the rule does not answer the operator's approval");
    assert_eq!(outcome(&response_of(&events, "bash")), Outcome::Executed);
    let prompts = prompter.prompts.lock().unwrap();
    assert!(prompts[1].suggestions.is_empty());
}
