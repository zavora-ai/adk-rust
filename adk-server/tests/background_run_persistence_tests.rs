//! Tests for background runs surviving a restart.
//!
//! Graph state already survived, through a checkpointer. The run registry did not,
//! so a restarted server could not report what had been in flight.

#![cfg(feature = "background")]

use std::collections::HashMap;
use std::sync::Arc;

use adk_server::background::{
    BackgroundRun, FileRunPersistence, RunPersistence, RunRetention, RunStatus, RunStore,
};
use serde_json::json;

fn temp_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("adk-runs-{}-{name}.json", std::process::id()))
}

fn a_run(id: &str, status: RunStatus) -> BackgroundRun {
    let mut input = HashMap::new();
    input.insert("topic".to_string(), json!("durability"));
    BackgroundRun {
        run_id: id.to_string(),
        workflow_id: "nightly".to_string(),
        status,
        input,
        result: None,
        error: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        timeout: None,
        max_retries: 0,
        retry_count: 0,
        cancel_token: tokio_util::sync::CancellationToken::new(),
    }
}

#[tokio::test]
async fn without_persistence_a_run_does_not_survive_a_new_store() {
    // The behaviour before this existed, pinned so the difference is visible.
    let store = RunStore::new();
    store.insert(a_run("r1", RunStatus::Running)).await;
    assert!(store.get("r1").await.is_some());

    let restarted = RunStore::new();
    assert!(restarted.get("r1").await.is_none(), "a fresh store knows nothing");
    assert_eq!(restarted.restore().await.unwrap(), Vec::<String>::new());
}

#[tokio::test]
async fn a_completed_run_survives_a_new_store() {
    let path = temp_path("completed");
    let _ = std::fs::remove_file(&path);
    let backend = Arc::new(FileRunPersistence::new(&path));

    let store = RunStore::new().with_persistence(backend.clone());
    store.insert(a_run("r1", RunStatus::Running)).await;
    store.set_completed("r1", json!({ "answer": 42 })).await;

    // A second store, sharing only the file.
    let restarted = RunStore::new().with_persistence(backend);
    let interrupted = restarted.restore().await.unwrap();
    assert!(interrupted.is_empty(), "a finished run was not interrupted");

    let run = restarted.get("r1").await.expect("the run is known after the restart");
    assert_eq!(run.status, RunStatus::Completed);
    assert_eq!(run.result, Some(json!({ "answer": 42 })));
    assert_eq!(run.input.get("topic"), Some(&json!("durability")));

    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn a_run_in_flight_is_reported_as_interrupted() {
    let path = temp_path("inflight");
    let _ = std::fs::remove_file(&path);
    let backend = Arc::new(FileRunPersistence::new(&path));

    let store = RunStore::new().with_persistence(backend.clone());
    store.insert(a_run("r1", RunStatus::Running)).await;
    store.insert(a_run("r2", RunStatus::Queued)).await;
    store.insert(a_run("r3", RunStatus::Running)).await;
    store.set_completed("r3", json!("done")).await;

    let restarted = RunStore::new().with_persistence(backend);
    let mut interrupted = restarted.restore().await.unwrap();
    interrupted.sort();
    assert_eq!(
        interrupted,
        vec!["r1".to_string(), "r2".to_string()],
        "the two that were in flight are named; the finished one is not"
    );

    let r1 = restarted.get("r1").await.unwrap();
    assert_eq!(r1.status, RunStatus::Failed, "it cannot still be running");
    assert!(
        r1.error.as_deref().is_some_and(|e| e.contains("process stopped")),
        "and it says why: {:?}",
        r1.error
    );
    assert_eq!(restarted.get("r3").await.unwrap().status, RunStatus::Completed);

    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn a_restored_run_has_a_fresh_cancellation_token() {
    // The token belongs to a running task. A restored run has none until it is
    // driven again, so it must not arrive already cancelled.
    let path = temp_path("token");
    let _ = std::fs::remove_file(&path);
    let backend = Arc::new(FileRunPersistence::new(&path));

    let store = RunStore::new().with_persistence(backend.clone());
    let run = a_run("r1", RunStatus::Queued);
    run.cancel_token.cancel();
    store.insert(run).await;

    let restarted = RunStore::new().with_persistence(backend);
    restarted.restore().await.unwrap();
    assert!(
        !restarted.get("r1").await.unwrap().cancel_token.is_cancelled(),
        "a restored run starts with a live token"
    );

    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn an_update_replaces_the_record_rather_than_appending() {
    let path = temp_path("replace");
    let _ = std::fs::remove_file(&path);
    let backend = Arc::new(FileRunPersistence::new(&path));

    let store = RunStore::new().with_persistence(backend.clone());
    store.insert(a_run("r1", RunStatus::Queued)).await;
    store.update_status("r1", RunStatus::Running).await;
    store.set_failed("r1", "upstream refused".to_string()).await;

    let records = backend.load_all().await.unwrap();
    assert_eq!(records.len(), 1, "one run, one record");
    assert_eq!(records[0].status, RunStatus::Failed);
    assert_eq!(records[0].error.as_deref(), Some("upstream refused"));

    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn a_missing_file_reads_as_no_runs() {
    let backend = FileRunPersistence::new(temp_path("absent"));
    assert!(backend.load_all().await.unwrap().is_empty());
}

#[tokio::test]
async fn the_default_bounds_finished_runs() {
    // The default must be bounded: a store that keeps every finished run forever
    // is a leak that only shows up after weeks in production.
    assert_eq!(RunRetention::default().max_finished, Some(1000));
}

#[tokio::test]
async fn finished_runs_beyond_the_bound_are_discarded_oldest_first() {
    let store = RunStore::new().with_retention(RunRetention::keep_finished(2));
    for index in 0..5 {
        let id = format!("r{index}");
        store.insert(a_run(&id, RunStatus::Running)).await;
        store.set_completed(&id, json!(index)).await;
    }

    assert!(store.get("r0").await.is_none(), "the oldest finished run went");
    assert!(store.get("r1").await.is_none());
    assert!(store.get("r2").await.is_none());
    assert!(store.get("r3").await.is_some(), "the newest two are kept");
    assert!(store.get("r4").await.is_some());
}

#[tokio::test]
async fn a_run_in_flight_is_never_discarded_by_the_bound() {
    let store = RunStore::new().with_retention(RunRetention::keep_finished(1));
    store.insert(a_run("live", RunStatus::Running)).await;
    for index in 0..4 {
        let id = format!("done{index}");
        store.insert(a_run(&id, RunStatus::Running)).await;
        store.set_completed(&id, json!(index)).await;
    }

    assert!(
        store.get("live").await.is_some(),
        "a busy server must not lose an in-flight run to the bound"
    );
    assert!(store.get("done3").await.is_some(), "and the newest finished one stays");
}

#[tokio::test]
async fn unlimited_retention_keeps_every_finished_run() {
    let store = RunStore::new().with_retention(RunRetention::unlimited());
    for index in 0..5 {
        let id = format!("r{index}");
        store.insert(a_run(&id, RunStatus::Running)).await;
        store.set_completed(&id, json!(index)).await;
    }
    assert!(store.get("r0").await.is_some(), "asking for unbounded gives unbounded");
}

#[tokio::test]
async fn the_persisted_file_is_bounded_too() {
    // The bound on the map is not enough: if the record outlives the run, the file
    // grows for the lifetime of the deployment.
    let path = temp_path("bounded-file");
    let _ = std::fs::remove_file(&path);
    let backend = Arc::new(FileRunPersistence::new(&path));

    let store = RunStore::new()
        .with_persistence(backend.clone())
        .with_retention(RunRetention::keep_finished(2));
    for index in 0..6 {
        let id = format!("r{index}");
        store.insert(a_run(&id, RunStatus::Running)).await;
        store.set_completed(&id, json!(index)).await;
    }

    let records = backend.load_all().await.unwrap();
    assert_eq!(records.len(), 2, "the file holds only what the bound allows");
    let mut ids: Vec<&str> = records.iter().map(|r| r.run_id.as_str()).collect();
    ids.sort();
    assert_eq!(ids, vec!["r4", "r5"], "and they are the newest two");

    let _ = std::fs::remove_file(&path);
}
