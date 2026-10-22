//! A run-finished wake source wired by hand on a worker that routes wakes (spec 3.2 §7.4):
//! the finish of a `kernel.wake` run is not a wake, or each routed event would enqueue
//! another `kernel.wake` run without end.

use std::sync::Arc;
use std::time::Duration;

use adk_core::{DurableRunStore, InMemoryRunStore, NewRun, RunState};
use adk_kernel::{
    InMemoryProcessStore, KernelWakeExecutor, QueueClass, RunFinishedWakeSource, WakeEvent,
    WakeRouter, WakeSource,
};
use adk_server::background::{DurableWorker, DurableWorkerConfig, WorkerHooks};
use futures::StreamExt;
use serde_json::json;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_finish_of_a_kernel_wake_run_publishes_no_wake() {
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let router = WakeRouter::new(Arc::clone(&runs), Arc::new(InMemoryProcessStore::new()));
    let source = Arc::new(RunFinishedWakeSource::new());
    let mut wakes = source.subscribe().await.unwrap();
    let worker = DurableWorker::new(Arc::clone(&runs))
        .with_workflows(Arc::new(KernelWakeExecutor::new(router.clone())))
        .with_hooks(source.clone())
        .with_config(DurableWorkerConfig::default().with_queue(QueueClass::MAINTENANCE));

    let event = WakeEvent::new("lockfile", "file:/w:/w/Cargo.lock:2026-10-23T09:00:00Z", json!({}));
    router.submit(&event, "adk-rust").await.unwrap();
    let finished = worker.run_next().await.unwrap().expect("the kernel.wake run ran");
    assert_eq!(finished.state, RunState::Completed);
    let published = tokio::time::timeout(Duration::from_millis(300), wakes.next()).await;
    assert!(published.is_err(), "a kernel.wake run's finish is not a wake: {published:?}");

    // Any other finished run still wakes its watchers.
    let mut done = adk_core::RunRecord::queued(
        "run-7",
        NewRun::new("background", "adk-rust", "alice", "adk-rust", json!({})),
        chrono::Utc::now(),
    );
    done.state = RunState::Completed;
    source.on_finish(&done).await;
    let wake = tokio::time::timeout(Duration::from_secs(1), wakes.next()).await.unwrap().unwrap();
    assert_eq!(wake.key, "run:run-7:completed");
}
