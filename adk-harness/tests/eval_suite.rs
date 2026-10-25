//! The harness eval suite on the scripted model, against its committed baseline
//! (spec 3.1 §7.13). Regenerate the baseline with `ADK_HARNESS_SAVE_BASELINE=1`.

#![cfg(all(feature = "runner", feature = "observe"))]

#[path = "eval/cases.rs"]
mod cases;

use std::collections::HashMap;
use std::path::PathBuf;

use adk_eval::BaselineStore;

fn baseline_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/eval/baseline.scripted.json")
}

async fn scripted_metrics() -> HashMap<String, HashMap<String, f64>> {
    let scenarios = cases::scenarios();
    let mut outcomes = Vec::new();
    for scenario in &scenarios {
        outcomes.push(cases::run(scenario, cases::scripted_model(scenario)).await);
    }
    for outcome in &outcomes {
        assert!(outcome.passed, "{outcome:#?}");
    }
    cases::metrics(&scenarios, &outcomes)
}

#[tokio::test]
async fn the_scripted_suite_matches_its_baseline() {
    let metrics = scripted_metrics().await;
    let store = BaselineStore::new(baseline_path());
    if std::env::var_os("ADK_HARNESS_SAVE_BASELINE").is_some() {
        store.save(cases::EVAL_SET_ID, &metrics).unwrap();
    }
    let regressions = store.check_regressions(&metrics, 0.0).unwrap();
    assert!(regressions.is_empty(), "{regressions:#?}");
    let baseline = store.load().unwrap().expect("the committed baseline");
    assert_eq!(baseline.eval_set_id, cases::EVAL_SET_ID);
    for metric in ["passed", "trajectory", "efficiency"] {
        assert_eq!(baseline.metrics[metric].len(), 3, "{metric} covers every case");
    }
}

#[tokio::test]
async fn a_baseline_is_produced_and_a_regression_is_reported() {
    let metrics = scripted_metrics().await;
    let dir = tempfile::tempdir().unwrap();
    let store = BaselineStore::new(dir.path().join("baseline.json"));
    store.save(cases::EVAL_SET_ID, &metrics).unwrap();
    assert_eq!(store.load().unwrap().unwrap().metrics, metrics);
    assert!(store.check_regressions(&metrics, 0.0).unwrap().is_empty());

    let mut worse = metrics.clone();
    worse.get_mut("passed").unwrap().insert("goal_until".to_string(), 0.0);
    let regressions = store.check_regressions(&worse, 0.0).unwrap();
    assert_eq!(regressions.len(), 1);
    assert_eq!(regressions[0].case_id, "goal_until");
    assert_eq!(regressions[0].metric_name, "passed");
}

#[tokio::test]
async fn the_capability_questions_pass_on_the_scripted_model() {
    for question in cases::capability_questions() {
        let outcome = cases::ask(&question, cases::capability_model(&question)).await;
        assert!(outcome.passed, "{outcome:#?}");
    }
}

#[test]
fn a_capability_answer_without_its_commands_fails() {
    let questions = cases::capability_questions();
    let deny = questions.iter().find(|q| q.id == "capability_deny_rule").unwrap();
    assert!(!cases::capability_passes(deny, "Just ask me before each edit."));
    assert!(
        !cases::capability_passes(deny, "Run adk-rust rules add edit_file."),
        "--deny is missing"
    );
    assert!(cases::capability_passes(
        deny,
        "adk-rust RULES ADD edit_file --path-within migrations --DENY \"review\""
    ));
}

#[test]
fn the_block_names_every_command_the_questions_ask_about() {
    let dir = tempfile::tempdir().unwrap();
    let harness = adk_harness::HarnessAgent::builder("harness")
        .model(std::sync::Arc::new(adk_model::ScriptedLlm::new(
            "scripted",
            vec![adk_model::ScriptedResponse::text("ok")],
        )))
        .workspace(adk_devtools::Workspace::new(dir.path()))
        .project_instructions(adk_harness::InstructionsConfig {
            user_file: None,
            ..adk_harness::InstructionsConfig::default()
        })
        .surface(cases::capability_surface())
        .build()
        .unwrap();
    let block = harness.manifest().render();
    for expected in ["/approvals", "adk-rust --continue", "adk-rust rules add --deny"] {
        assert!(block.contains(expected), "{expected} is missing from {block}");
    }
}
