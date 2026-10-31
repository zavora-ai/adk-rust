//! Every harness protects the extended set of spec 3.4 §7.5 (§10 P1): non-waivable approval
//! in Ask and Auto, denied in ReadOnly and Plan (spec 3.1 §9.2 I29).

#![cfg(feature = "runner")]

use crate::common;

use adk_harness::HarnessMode;
use adk_model::ScriptedResponse;
use common::{Outcome, fixture, outcome, responses};
use serde_json::json;

fn batch() -> ScriptedResponse {
    ScriptedResponse::function_calls(
        ["Cargo.toml", ".github/workflows/ci.yml", "scripts/release.sh", "src/lib.rs"]
            .map(|path| ("write_file", json!({ "path": path, "content": "edited\n" }))),
    )
}

#[tokio::test]
async fn the_extended_set_needs_a_one_shot_approval_in_every_mode_that_edits() {
    use Outcome::{Denied, Executed, Held};
    for (mode, expected) in [
        (HarnessMode::ReadOnly, [Denied, Denied, Denied, Denied]),
        (HarnessMode::Plan, [Denied, Denied, Denied, Denied]),
        (HarnessMode::Ask, [Held, Held, Held, Held]),
        (HarnessMode::Auto, [Held, Held, Held, Executed]),
    ] {
        let f = fixture(vec![batch(), ScriptedResponse::text("done")], |b| b.mode(mode)).await;
        let events = f.send("edit").await;
        let got: Vec<Outcome> = responses(&events).iter().map(|(_, r)| outcome(r)).collect();
        assert_eq!(got, expected, "{mode}: {:#?}", responses(&events));
        assert!(!f.path("Cargo.toml").exists(), "{mode}");
        if mode == HarnessMode::Ask {
            let suggestions = f
                .harness
                .pending_approvals(&f.user, &f.session)
                .await
                .unwrap()
                .iter()
                .filter(|entry| entry.request.args["path"] != "src/lib.rs")
                .flat_map(|entry| adk_harness::StandingRule::suggestions(&entry.request))
                .count();
            assert_eq!(suggestions, 0, "no rule is offered for a protected path");
        }
    }
}
