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
        Some(StandingOffer::new("for this command (bash: cargo test …)"))
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
