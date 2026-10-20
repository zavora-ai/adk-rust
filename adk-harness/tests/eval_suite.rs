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
