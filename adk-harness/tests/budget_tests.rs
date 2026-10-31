//! A run budget set on the builder bounds each run on its own (spec 3.1 §7.13): a runner
//! built with `HarnessAgent::configure_runner` starts every run with fresh counters, so a
//! limit one run used is not charged to the next.

#![cfg(feature = "runner")]

use crate::common;

use adk_core::RunBudget;
use adk_model::ScriptedResponse;
use common::{Outcome, fixture, outcome, response_of};
use serde_json::json;

fn read_readme() -> ScriptedResponse {
    ScriptedResponse::function_call("read_file", json!({ "path": "README.md" }))
}

#[tokio::test]
async fn each_run_of_a_configured_runner_gets_its_own_budget() {
    let script = vec![
        read_readme(),
        ScriptedResponse::text("read"),
        read_readme(),
        ScriptedResponse::text("read again"),
    ];
    let f = fixture(script, |b| b.budget(RunBudget::new().max_tool_calls(1))).await;
    std::fs::write(f.path("README.md"), "a project\n").unwrap();

    let first = f.send("read the readme").await;
    assert_eq!(outcome(&response_of(&first, "read_file")), Outcome::Executed);

    // The second run starts with its own tool-call budget: the one call of the first run
    // is not charged to it.
    let second = f.send("read it again").await;
    assert_eq!(outcome(&response_of(&second, "read_file")), Outcome::Executed);
}

#[tokio::test]
async fn a_run_still_stops_at_its_own_limit() {
    let script = vec![
        ScriptedResponse::function_calls([
            ("read_file", json!({ "path": "README.md" })),
            ("read_file", json!({ "path": "README.md" })),
        ]),
        ScriptedResponse::text("read twice"),
    ];
    let f = fixture(script, |b| b.budget(RunBudget::new().max_tool_calls(1))).await;
    std::fs::write(f.path("README.md"), "a project\n").unwrap();

    let stream = f.harness.send(&f.user, &f.session, "read the readme twice").await.unwrap();
    let results: Vec<_> = futures::StreamExt::collect(stream).await;

    let error = results.iter().find_map(|result| result.as_ref().err()).expect("the run stops");
    assert_eq!(error.category, adk_core::ErrorCategory::ResourceExhausted, "{error}");
}
