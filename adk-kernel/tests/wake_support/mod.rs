//! Shared pieces of the wake tests: a run store that counts wakes, and helpers that park a
//! run asleep and wait for a condition.

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_core::{
    ClaimedRun, DurableRunStore, Enqueued, InMemoryRunStore, NewRun, RunCheckpointRef, RunFilter,
    RunOutcome, RunRecord, RunState,
};
use adk_kernel::{ProcessKind, ProcessMeta, ProcessStore, QueueClass, WakeSet};
use async_trait::async_trait;
use chrono::{DateTime, Utc};

/// An in-memory run store that counts `wake` calls per run.
#[derive(Default)]
pub struct CountingRuns {
    pub inner: InMemoryRunStore,
    pub wakes: Mutex<HashMap<String, usize>>,
}

impl CountingRuns {
    pub fn wakes_of(&self, run_id: &str) -> usize {
        self.wakes.lock().unwrap().get(run_id).copied().unwrap_or_default()
    }
}

#[async_trait]
impl DurableRunStore for CountingRuns {
    async fn enqueue(&self, run: NewRun) -> adk_core::Result<Enqueued> {
        self.inner.enqueue(run).await
    }

    async fn claim(
        &self,
        queue: &str,
        worker: &str,
        ttl: Duration,
    ) -> adk_core::Result<Option<ClaimedRun>> {
        self.inner.claim(queue, worker, ttl).await
    }

    async fn heartbeat(&self, run_id: &str, epoch: u64, ttl: Duration) -> adk_core::Result<bool> {
        self.inner.heartbeat(run_id, epoch, ttl).await
    }

    async fn set_checkpoint(
        &self,
        run_id: &str,
        epoch: u64,
        checkpoint: RunCheckpointRef,
    ) -> adk_core::Result<()> {
        self.inner.set_checkpoint(run_id, epoch, checkpoint).await
    }

    async fn wait(
        &self,
        run_id: &str,
        epoch: u64,
        until: Option<DateTime<Utc>>,
    ) -> adk_core::Result<()> {
        self.inner.wait(run_id, epoch, until).await
    }

    async fn wake(&self, run_id: &str) -> adk_core::Result<()> {
        *self.wakes.lock().unwrap().entry(run_id.to_string()).or_default() += 1;
        self.inner.wake(run_id).await
    }

    async fn finish(
        &self,
        run_id: &str,
        epoch: u64,
        outcome: RunOutcome,
    ) -> adk_core::Result<RunRecord> {
        self.inner.finish(run_id, epoch, outcome).await
    }

    async fn request_cancel(&self, run_id: &str) -> adk_core::Result<()> {
        self.inner.request_cancel(run_id).await
    }

    async fn get(&self, run_id: &str) -> adk_core::Result<Option<RunRecord>> {
        self.inner.get(run_id).await
    }

    async fn list(&self, filter: RunFilter) -> adk_core::Result<Vec<RunRecord>> {
        self.inner.list(filter).await
    }

    async fn prune(&self, finished_before: DateTime<Utc>) -> adk_core::Result<u64> {
        self.inner.prune(finished_before).await
    }
}

/// Enqueues `run`, claims it and parks it `Waiting` with no lease and no timer, as a process
/// asleep on a wake set is; records its process with `wake`. Returns the run id.
pub async fn sleeper(
    runs: &Arc<dyn DurableRunStore>,
    processes: &Arc<dyn ProcessStore>,
    run: NewRun,
    wake: WakeSet,
) -> String {
    let queue = run.queue.clone();
    let record = runs.enqueue(run).await.unwrap().record().cloned().unwrap();
    let claimed = runs.claim(&queue, "parker", Duration::from_secs(30)).await.unwrap().unwrap();
    assert_eq!(claimed.record.run_id, record.run_id, "the sleeper must be the only queued run");
    runs.wait(&record.run_id, claimed.epoch, None).await.unwrap();
    processes
        .put(
            ProcessMeta::new(
                record.run_id.clone(),
                QueueClass::from_name(&queue),
                ProcessKind::Goal,
            )
            .with_wake(wake),
        )
        .await
        .unwrap();
    record.run_id
}

/// The run's current state.
pub async fn state(runs: &Arc<dyn DurableRunStore>, run_id: &str) -> RunState {
    runs.get(run_id).await.unwrap().unwrap().state
}

/// Whether a `Waiting` run has been made claimable by a wake.
pub async fn woken(runs: &Arc<dyn DurableRunStore>, run_id: &str) -> bool {
    let run = runs.get(run_id).await.unwrap().unwrap();
    run.state == RunState::Waiting && run.not_before.is_some_and(|at| at <= Utc::now())
}

/// Polls `probe` every 10 ms until it returns `Some`, for at most `limit`.
pub async fn eventually<T, F, Fut>(what: &str, limit: Duration, mut probe: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let started = tokio::time::Instant::now();
    loop {
        if let Some(value) = probe().await {
            return value;
        }
        assert!(started.elapsed() <= limit, "timed out after {limit:?} waiting for {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
