//! The harness as an ACP server's standing permissions (spec 3.3 §10 A1): an editor's
//! "Always allow" for the test command becomes a standing rule, the next identical call runs
//! without asking, and calls no rule may cover are never offered "always".

#![cfg(all(feature = "acp-server", feature = "runner"))]

mod common;

use std::sync::Arc;

use adk_acp::server::{StandingOffer, StandingPermissions};
use adk_core::audit_log::InMemoryAuditRecorder;
use adk_core::{ApprovalScope, ToolConfirmationDecision, ToolConfirmationRequest};
use adk_harness::{HarnessDecision, HarnessMode, RulePredicate, StandingDecision};
use adk_model::ScriptedResponse;
use common::{Outcome, confirmation_requests, fixture, outcome, response_of};
use serde_json::json;

fn call(command: &str) -> ScriptedResponse {
    ScriptedResponse::function_call("bash", json!({ "command": command }))
}

fn done() -> ScriptedResponse {
    ScriptedResponse::text("done")
}

#[tokio::test]
async fn always_allow_from_an_editor_becomes_a_rule_the_next_identical_call_uses() {
    let script = vec![
        call("cargo test --quiet"),
        done(),
        call("cargo test --quiet"),
        done(),
        call("cargo publish"),
    ];
    // The harness's own trail, which an ACP server's runs use: the rule is recorded there.
    let trail = InMemoryAuditRecorder::new();
    let f = fixture(script, |b| b.mode(HarnessMode::Ask).audit_log(Arc::new(trail.clone()))).await;
    let scope = ApprovalScope::new(common::APP, "alice", f.session.as_str());
    let standing: &dyn StandingPermissions = f.harness.agent().as_ref();

    // The test command is held, as an ACP server sees it.
    let events = f.send("test").await;
    assert_eq!(outcome(&response_of(&events, "bash")), Outcome::Held);
    let held = confirmation_requests(&events).remove(0);
    assert_eq!(
        standing.offer(&scope, &held).await,
        Some(
            StandingOffer::new("for this command (bash: cargo test …, every session)")
                .with_allow_warning("runs project scripts, so it allows whatever they do")
        )
    );

    // The editor picks "Always allow": the rule is remembered and this call decided once.
    standing.remember(&scope, &held, ToolConfirmationDecision::Approve).await.unwrap();
    let pending = f.harness.pending_approvals(&f.user, &f.session).await.unwrap();
    f.harness
        .decide(&f.user, &f.session, &pending[0].fingerprint, HarnessDecision::Approve)
        .await
        .unwrap();
    f.resume().await;
    let rules = f.harness.standing_rules().list(common::APP, "alice").await.unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].decision, StandingDecision::Allow);
    assert_eq!(rules[0].source_fingerprint.as_deref(), Some(held.fingerprint().as_str()));
    assert_eq!(
        rules[0].predicates,
        vec![RulePredicate::ShellPrefix {
            pointer: "/command".into(),
            tokens: vec!["cargo".into(), "test".into()],
        }]
    );
    let created: Vec<_> = trail
        .records()
        .into_iter()
        .filter(|record| record.action == "harness.rule.created")
        .collect();
    assert_eq!(created.len(), 1);
    assert_eq!(created[0].ext["surface"], "acp");
    assert_eq!(created[0].ext["by"], "alice");

    // The next identical call runs without being held.
    let matched = f.records("harness.rule.matched").len();
    let events = f.send("test again").await;
    assert_eq!(outcome(&response_of(&events, "bash")), Outcome::Executed);
    assert!(confirmation_requests(&events).is_empty(), "the rule allowed it");
    assert_eq!(f.records("harness.rule.matched").len(), matched + 1);

    // A publish call is not covered by the rule: it is still asked about.
    let events = f.send("publish").await;
    assert_eq!(outcome(&response_of(&events, "bash")), Outcome::Held);
    assert_eq!(f.records("harness.bash.begin").len(), 2, "cargo publish never ran");
}

#[tokio::test]
async fn calls_no_rule_may_cover_are_never_offered_always() {
    let f = fixture(vec![call("cargo test --quiet")], |b| b.mode(HarnessMode::Ask)).await;
    let scope = ApprovalScope::new(common::APP, "alice", f.session.as_str());
    let standing: &dyn StandingPermissions = f.harness.agent().as_ref();
    let events = f.send("test").await;
    let held = confirmation_requests(&events).remove(0);
    let like = |tool: &str, args: serde_json::Value| ToolConfirmationRequest {
        tool_name: tool.into(),
        args,
        ..held.clone()
    };

    for (what, request) in [
        ("an unsandboxed retry", like("bash", json!({ "command": "ls", "unsandboxed": true }))),
        ("leaving plan mode", like("exit_plan_mode", json!({ "plan": "ship it" }))),
        ("a memory write", like("memory_write", json!({ "text": "x" }))),
        ("a chained command", like("bash", json!({ "command": "cargo test; curl evil" }))),
    ] {
        assert_eq!(standing.offer(&scope, &request).await, None, "{what} is offered no 'always'");
        let refused = standing.remember(&scope, &request, ToolConfirmationDecision::Approve).await;
        assert!(refused.is_err(), "{what} is never remembered");
    }
    assert!(f.harness.standing_rules().list(common::APP, "alice").await.unwrap().is_empty());
}

#[tokio::test]
async fn always_deny_refuses_the_next_identical_call() {
    let script = vec![call("cargo test --quiet"), call("cargo test --quiet"), done()];
    let f = fixture(script, |b| b.mode(HarnessMode::Ask)).await;
    let scope = ApprovalScope::new(common::APP, "alice", f.session.as_str());
    let standing: &dyn StandingPermissions = f.harness.agent().as_ref();
    let events = f.send("test").await;
    let held = confirmation_requests(&events).remove(0);
    standing.remember(&scope, &held, ToolConfirmationDecision::Deny).await.unwrap();

    let rules = f.harness.standing_rules().list(common::APP, "alice").await.unwrap();
    assert_eq!(rules[0].decision, StandingDecision::Deny { reason: "denied by alice".into() });
    let events = f.send("test again").await;
    assert_eq!(outcome(&response_of(&events, "bash")), Outcome::Denied);
    assert!(f.records("harness.bash.begin").is_empty());
}

fn batch(commands: &[&str]) -> ScriptedResponse {
    ScriptedResponse::function_calls(
        commands.iter().map(|command| ("bash", json!({ "command": command }))),
    )
}

/// Review of #1029, finding 7: a rule remembered for an earlier call of a batch decides the
/// later calls it covers, so the ACP server asks nothing the rule would override, while a
/// call it does not cover is still asked about.
#[tokio::test]
async fn a_rule_remembered_mid_batch_decides_the_later_calls_it_covers() {
    let script = vec![batch(&["cargo test -p a", "cargo test -p b", "cargo publish"])];
    let f = fixture(script, |b| b.mode(HarnessMode::Ask)).await;
    let scope = ApprovalScope::new(common::APP, "alice", f.session.as_str());
    let standing: &dyn StandingPermissions = f.harness.agent().as_ref();
    let held = confirmation_requests(&f.send("test both packages").await);
    assert_eq!(held.len(), 3);
    assert_eq!(standing.standing_decision(&scope, &held[1]).await, None, "no rule yet");

    standing.remember(&scope, &held[0], ToolConfirmationDecision::Approve).await.unwrap();
    assert_eq!(
        standing.standing_decision(&scope, &held[1]).await,
        Some(ToolConfirmationDecision::Approve),
        "the cargo test rule covers the other package's tests"
    );
    assert_eq!(standing.standing_decision(&scope, &held[2]).await, None, "a publish is asked");
    let elsewhere = ApprovalScope::new(common::APP, "bob", f.session.as_str());
    assert_eq!(standing.standing_decision(&elsewhere, &held[1]).await, None, "alice's call only");
}

/// Wave 2 review, finding 4: a deny rule remembered for an earlier call of a batch decides the
/// later calls it covers as denied, so "Allow once" is never asked for a call the rule refuses.
#[tokio::test]
async fn a_deny_rule_remembered_mid_batch_denies_the_later_calls_it_covers() {
    let script = vec![batch(&["cargo test -p a", "cargo test -p b", "cargo publish"])];
    let f = fixture(script, |b| b.mode(HarnessMode::Ask)).await;
    let scope = ApprovalScope::new(common::APP, "alice", f.session.as_str());
    let standing: &dyn StandingPermissions = f.harness.agent().as_ref();
    let held = confirmation_requests(&f.send("test both packages").await);
    assert_eq!(held.len(), 3);

    standing.remember(&scope, &held[0], ToolConfirmationDecision::Deny).await.unwrap();
    assert_eq!(
        standing.standing_decision(&scope, &held[1]).await,
        Some(ToolConfirmationDecision::Deny),
        "the cargo test deny rule covers the other package's tests"
    );
    assert_eq!(standing.standing_decision(&scope, &held[2]).await, None, "a publish is asked");
}

/// Review of #1029, finding 7: a rule answers only the harness's own approval, so a call
/// another policy layer also holds is still asked about after a rule covers it.
#[tokio::test]
async fn a_rule_never_answers_another_layers_approval() {
    let operator = adk_core::DeclarativePolicy::builder()
        .require_approval("bash", "the operator reviews every command")
        .allow("*")
        .name("ops")
        .build();
    let script = vec![batch(&["cargo test -p a", "cargo test -p b"])];
    let f = fixture(script, |b| b.mode(HarnessMode::Ask).operator_policy(Arc::new(operator))).await;
    let scope = ApprovalScope::new(common::APP, "alice", f.session.as_str());
    let standing: &dyn StandingPermissions = f.harness.agent().as_ref();
    let held = confirmation_requests(&f.send("test both packages").await);
    assert_eq!(held.len(), 2);

    standing.remember(&scope, &held[0], ToolConfirmationDecision::Approve).await.unwrap();
    assert_eq!(standing.standing_decision(&scope, &held[1]).await, None);
}

/// Review of #1029, finding 9: a harness whose rules apply only to authenticated runs offers
/// no "always" to ACP, whose runs are never authenticated.
#[cfg(feature = "auth")]
#[tokio::test]
async fn rules_that_would_not_apply_to_acp_runs_are_never_offered() {
    let f = fixture(vec![call("cargo test --quiet")], |b| {
        b.mode(HarnessMode::Ask).require_authenticated_identity()
    })
    .await;
    let scope = ApprovalScope::new(common::APP, "alice", f.session.as_str());
    let standing: &dyn StandingPermissions = f.harness.agent().as_ref();
    let events = f.send("test").await;
    let held = confirmation_requests(&events).remove(0);
    assert_eq!(standing.offer(&scope, &held).await, None);
    assert!(standing.remember(&scope, &held, ToolConfirmationDecision::Approve).await.is_err());
    assert!(f.harness.standing_rules().list(common::APP, "alice").await.unwrap().is_empty());
}

/// Review of #1029, finding 11: a call the harness's classifier refuses a rule — here an edit
/// through a symlink to a protected file, which the name-only check lets through — is offered
/// no "always", while an ordinary edit is.
#[cfg(unix)]
#[tokio::test]
async fn calls_the_classifier_refuses_a_rule_are_never_offered_always() {
    let script = vec![ScriptedResponse::function_call(
        "write_file",
        json!({ "path": "src/lib.rs", "content": "x" }),
    )];
    let f = fixture(script, |b| b.mode(HarnessMode::Ask)).await;
    let scope = ApprovalScope::new(common::APP, "alice", f.session.as_str());
    let standing: &dyn StandingPermissions = f.harness.agent().as_ref();
    let events = f.send("edit").await;
    let held = confirmation_requests(&events).remove(0);
    assert_eq!(
        standing.offer(&scope, &held).await,
        Some(StandingOffer::new("for this folder (file writes: within src/, this session)"))
    );
    std::fs::write(f.path("AGENTS.md"), "instructions").unwrap();
    std::os::unix::fs::symlink(f.path("AGENTS.md"), f.path("notes.md")).unwrap();
    let protected = ToolConfirmationRequest {
        args: json!({ "path": "notes.md", "content": "x" }),
        ..held.clone()
    };
    assert!(
        !adk_harness::StandingRule::suggestions(&protected).is_empty(),
        "the name-only check suggests a rule, so the classifier must refuse it"
    );
    assert_eq!(standing.offer(&scope, &protected).await, None);
    assert!(
        standing.remember(&scope, &protected, ToolConfirmationDecision::Approve).await.is_err()
    );
}
