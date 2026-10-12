//! A contention check every durable run store backend runs beyond the conformance suite.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use adk_core::run_store::{DurableRunStore, Enqueued, NewRun, RunFilter, RunOutcome, RunState};
use serde_json::json;

/// Runs enqueued on the contended queue.
pub const RUNS: usize = 48;
/// Concurrent claimers, split evenly across the two handles.
pub const CLAIMERS: usize = 24;

/// Runs [`claimers_drain_runs_exactly_once`] with [`RUNS`] runs and [`CLAIMERS`] claimers.
#[allow(dead_code, reason = "each test binary includes this module and calls one of the two")]
pub async fn claimers_drain_a_queue_exactly_once(
    first: Arc<dyn DurableRunStore>,
    second: Arc<dyn DurableRunStore>,
) {
    claimers_drain_runs_exactly_once(first, second, RUNS, CLAIMERS).await;
}

/// Enqueues `runs` runs on one queue, then lets `claimers` concurrent workers, half on
/// each handle, claim and complete runs until the queue is empty. Every run must be
/// claimed exactly once, at epoch 1, and end `Completed`.
pub async fn claimers_drain_runs_exactly_once(
    first: Arc<dyn DurableRunStore>,
    second: Arc<dyn DurableRunStore>,
    runs: usize,
    claimers: usize,
) {
    let queue = format!("contention:queue/ü/{}", uuid::Uuid::new_v4());
    let mut enqueued = Vec::new();
    for i in 0..runs {
        let run = NewRun::new(&queue, "contention-app", "user", "agent", json!({ "i": i }));
        match first.enqueue(run).await.expect("enqueue") {
            Enqueued::Created(record) => enqueued.push(record.run_id),
            other => panic!("expected a new run, got {other:?}"),
        }
    }

    let tasks: Vec<_> = (0..claimers)
        .map(|i| {
            let store = if i % 2 == 0 { first.clone() } else { second.clone() };
            let queue = queue.clone();
            tokio::spawn(async move {
                let worker = format!("worker-{i}");
                let mut claimed = Vec::new();
                while let Some(run) =
                    store.claim(&queue, &worker, Duration::from_secs(300)).await.expect("claim")
                {
                    assert_eq!(run.epoch, 1, "a run is never claimed twice");
                    store
                        .finish(&run.record.run_id, run.epoch, RunOutcome::Completed(json!(i)))
                        .await
                        .expect("the claimer still holds the run");
                    claimed.push(run.record.run_id);
                }
                claimed
            })
        })
        .collect();

    let mut claims: HashMap<String, usize> = HashMap::new();
    for task in tasks {
        for run_id in task.await.expect("claimer completes") {
            *claims.entry(run_id).or_default() += 1;
        }
    }
    let twice: Vec<_> = claims.iter().filter(|(_, count)| **count > 1).collect();
    assert!(twice.is_empty(), "runs claimed more than once: {twice:?}");
    let mut claimed: Vec<_> = claims.into_keys().collect();
    claimed.sort();
    enqueued.sort();
    assert_eq!(claimed, enqueued, "every run is claimed exactly once");

    let finished = second
        .list(RunFilter::default().queue(&queue).states([RunState::Completed]))
        .await
        .expect("list");
    assert_eq!(finished.len(), runs, "every run completed");
}
