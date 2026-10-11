//! Three OS processes claim from one PostgreSQL run queue at once, and every run is
//! claimed and finished exactly once.
//!
//! The parent test enqueues 200 runs and starts this test binary three times, running
//! only [`claim_worker`]. Each worker process claims with four concurrent tasks until the
//! queue is empty, finishes every run it claims, and prints one `adk-claimed` line per
//! run. Requires a server at `DATABASE_URL`:
//!
//! ```bash
//! DATABASE_URL=postgres://postgres@localhost:5432/postgres \
//!   cargo nextest run -p adk-session --features postgres --run-ignored only \
//!   --test run_store_multiprocess_postgres
//! ```

#![cfg(feature = "postgres")]

use std::collections::{BTreeMap, BTreeSet};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use adk_core::{DurableRunStore, NewRun, RunFilter, RunOutcome, RunState};
use adk_session::PostgresRunStore;
use serde_json::json;
use sqlx::postgres::PgPoolOptions;

const RUNS: usize = 200;
const PROCESSES: usize = 3;
const TASKS_PER_PROCESS: usize = 4;
const QUEUE_ENV: &str = "ADK_RUN_STORE_WORKER_QUEUE";
const WORKER_ENV: &str = "ADK_RUN_STORE_WORKER_ID";
const START_ENV: &str = "ADK_RUN_STORE_WORKER_START_MS";
const MARKER: &str = "adk-claimed";

async fn store(max_connections: u32) -> PostgresRunStore {
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres@localhost:5432/postgres".to_string());
    let pool = PgPoolOptions::new()
        .max_connections(max_connections)
        .connect(&url)
        .await
        .expect("connect to postgres");
    let store = PostgresRunStore::from_pool(pool);
    store.migrate().await.expect("run schema migrates");
    store
}

fn unix_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after the epoch")
        .as_millis()
}

/// The worker process body. A no-op unless the parent test set the worker variables.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "worker process for three_processes_claim_each_run_exactly_once"]
async fn claim_worker() {
    let (Ok(queue), Ok(worker)) = (std::env::var(QUEUE_ENV), std::env::var(WORKER_ENV)) else {
        return;
    };
    let start: u128 = std::env::var(START_ENV).expect("start time").parse().expect("millis");
    let store = Arc::new(store(TASKS_PER_PROCESS as u32 + 1).await);
    // Every process starts claiming at the same instant, so their claims overlap.
    let wait = start.saturating_sub(unix_millis());
    tokio::time::sleep(Duration::from_millis(u64::try_from(wait).unwrap_or(0))).await;

    let tasks: Vec<_> = (0..TASKS_PER_PROCESS)
        .map(|task| {
            let store = store.clone();
            let queue = queue.clone();
            let worker = worker.clone();
            tokio::spawn(async move {
                let owner = format!("{worker}/{task}");
                let mut claimed = Vec::new();
                while let Some(run) =
                    store.claim(&queue, &owner, Duration::from_secs(60)).await.expect("claim")
                {
                    // Holds the lease briefly, so the other processes claim meanwhile.
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    store
                        .finish(
                            &run.record.run_id,
                            run.epoch,
                            RunOutcome::Completed(json!({ "worker": worker })),
                        )
                        .await
                        .expect("finish");
                    claimed.push(run.record.run_id);
                }
                claimed
            })
        })
        .collect();
    for task in tasks {
        for run_id in task.await.expect("claim task completes") {
            println!("{MARKER} {worker} {run_id}");
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires a PostgreSQL server at DATABASE_URL"]
async fn three_processes_claim_each_run_exactly_once() {
    let store = store(4).await;
    let queue = format!("multiprocess/{}", uuid::Uuid::new_v4());
    let mut enqueued = BTreeSet::new();
    for i in 0..RUNS {
        let run = NewRun::new(&queue, "multiprocess", "user", "agent", json!({ "n": i }));
        let record = store.enqueue(run).await.expect("enqueue").record().cloned().expect("created");
        enqueued.insert(record.run_id);
    }

    let exe = std::env::current_exe().expect("test binary path");
    let start = (unix_millis() + 3_000).to_string();
    let children: Vec<_> = (0..PROCESSES)
        .map(|i| {
            Command::new(&exe)
                .args(["claim_worker", "--exact", "--ignored", "--nocapture", "--test-threads=1"])
                .env(QUEUE_ENV, &queue)
                .env(WORKER_ENV, format!("process-{i}"))
                .env(START_ENV, &start)
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
                .expect("worker process starts")
        })
        .collect();

    // Run id → the processes that reported claiming it.
    let mut claims: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for child in children {
        let output = child.wait_with_output().expect("worker process exits");
        assert!(output.status.success(), "worker process failed: {:?}", output.status);
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            // libtest may print `test claim_worker ... ` ahead of the first line.
            if let Some(at) = line.find(MARKER) {
                let mut fields = line[at + MARKER.len()..].split_whitespace();
                let worker = fields.next().expect("worker id").to_string();
                let run_id = fields.next().expect("run id").to_string();
                claims.entry(run_id).or_default().push(worker);
            }
        }
    }

    let doubles: Vec<_> = claims.iter().filter(|(_, workers)| workers.len() > 1).collect();
    assert!(doubles.is_empty(), "runs claimed more than once: {doubles:?}");
    assert_eq!(claims.keys().cloned().collect::<BTreeSet<_>>(), enqueued, "every run is claimed");
    let per_process: BTreeSet<_> = claims.values().flatten().collect();
    assert_eq!(per_process.len(), PROCESSES, "every process claimed runs concurrently");

    let finished = store.list(RunFilter::default().queue(&queue)).await.expect("list");
    assert_eq!(finished.len(), RUNS);
    for run in finished {
        let claimer = &claims[&run.run_id][0];
        assert_eq!(
            (run.state, run.attempt, run.lease_epoch, run.result),
            (RunState::Completed, 1, 1, Some(json!({ "worker": claimer }))),
            "run {} was claimed and finished once, by {claimer}",
            run.run_id
        );
    }
}
