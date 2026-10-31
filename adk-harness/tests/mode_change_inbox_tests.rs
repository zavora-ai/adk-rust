//! A loosening cancels held calls and leaves nothing in the inbox that could authorize a
//! later identical call (spec 3.1 §7.4, §9.2 I14; evaluation finding F-5, scenario s04b).

#![cfg(feature = "runner")]

use crate::common;

use adk_harness::{HarnessDecision, HarnessMode};
use adk_model::ScriptedResponse;
use common::{Outcome, fixture, outcome, response_of};
use serde_json::json;

fn write_notes() -> ScriptedResponse {
    ScriptedResponse::function_call("write_file", json!({ "path": "notes.txt", "content": "x" }))
}

// A held call ends the run, so each turn's script is the call alone.
fn done() -> ScriptedResponse {
    ScriptedResponse::text("done")
}

#[tokio::test]
async fn a_cancelled_call_leaves_no_entry_to_approve_later() {
    let f = fixture(vec![write_notes(), write_notes(), done()], |b| b).await;
    let events = f.send("write").await;
    assert_eq!(outcome(&response_of(&events, "write_file")), Outcome::Held);
    let pending = f.harness.pending_approvals(&f.user, &f.session).await.unwrap();
    assert_eq!(pending.len(), 1);
    let fingerprint = pending[0].fingerprint.clone();

    f.harness.set_mode(&f.user, &f.session, HarnessMode::Auto).await.unwrap();
    assert!(
        f.harness.pending_approvals(&f.user, &f.session).await.unwrap().is_empty(),
        "the cancelled call is no longer pending"
    );
    f.harness.set_mode(&f.user, &f.session, HarnessMode::Ask).await.unwrap();
    let decided =
        f.harness.decide(&f.user, &f.session, &fingerprint, HarnessDecision::Approve).await;
    assert!(decided.is_err(), "a cancelled call cannot be approved");

    let events = f.send("write again").await;
    assert_eq!(
        outcome(&response_of(&events, "write_file")),
        Outcome::Held,
        "the identical new call is asked about"
    );
    assert!(!f.path("notes.txt").exists());
}

#[tokio::test]
async fn a_decision_made_before_a_loosening_does_not_survive_it() {
    let f = fixture(vec![write_notes(), write_notes(), done()], |b| b).await;
    f.send("write").await;
    let pending = f.harness.pending_approvals(&f.user, &f.session).await.unwrap();
    f.harness
        .decide(&f.user, &f.session, &pending[0].fingerprint, HarnessDecision::Approve)
        .await
        .unwrap();

    f.harness.set_mode(&f.user, &f.session, HarnessMode::Auto).await.unwrap();
    f.harness.set_mode(&f.user, &f.session, HarnessMode::Ask).await.unwrap();
    let events = f.send("write again").await;
    assert_eq!(outcome(&response_of(&events, "write_file")), Outcome::Held);
    assert!(!f.path("notes.txt").exists(), "the cancelled call's approval authorized nothing");
}
