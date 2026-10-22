//! The supervisor (spec 3.2 §7.2, §7.3): processes spawned, put to sleep, suspended,
//! frozen, killed, moved and drained on a real `DurableWorker` over a scripted model.
//!
//! Each test proves one behaviour on stores in memory.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use adk_core::audit_log::AuditActor;
use adk_core::{GovernanceControl, RunRecord, RunState, SubjectRef};
use adk_kernel::{
    KernelError, KernelEvent, KernelState, ProcessFilter, ProcessKind, ProcessSpec, ProcessStore,
    ProgressSource, QueueClass, RunUsage, Suspension, Wake, WakeSet,
};
use adk_server::rest::FreezeDelegate;
use async_trait::async_trait;
use common::*;

fn operator() -> AuditActor {
    AuditActor::subject(SubjectRef::human("operator"))
}

fn sleeper() -> ProcessSpec {
    ProcessSpec::new(ProcessKind::Goal).with_wake(WakeSet::any_of([Wake::Manual]))
}

async fn meta(world: &World, pid: &str) -> adk_kernel::ProcessMeta {
    world.processes.get(pid).await.unwrap().expect("the process has a side-table row")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_spawned_process_runs_to_completion_and_ps_shows_it() {
    let world = World::answer();
    let control = GovernanceControl::new();
    let kernel = world.kernel("k1", &control);
    let mut events = kernel.events();
    kernel.start().await.unwrap();

    let spawned = kernel
        .processes()
        .spawn(run_on(QueueClass::Interactive, "hi"), ProcessSpec::new(ProcessKind::Interactive))
        .await
        .unwrap();
    let pid = spawned.meta.pid.clone();
    in_state(&world.runs, &pid, RunState::Completed).await;

    let listed = kernel.processes().ps(ProcessFilter::default()).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].meta.kind, ProcessKind::Interactive);
    assert_eq!(listed[0].meta.queue, QueueClass::Interactive);
    assert_eq!(listed[0].view.model_calls, 1, "the transcript holds one model call");
    let mut transitions = Vec::new();
    while let Ok(Some(record)) =
        tokio::time::timeout(Duration::from_millis(200), events.recv()).await
    {
        if let KernelEvent::Process { pid: event_pid, to, .. } = record.event
            && event_pid == pid
        {
            transitions.push(to);
        }
    }
    assert_eq!(transitions, [RunState::Queued, RunState::Running, RunState::Completed]);
    kernel.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sleepers_park_before_their_first_model_call_and_hold_no_lease() {
    let world = World::answer();
    let control = GovernanceControl::new();
    let kernel = world.kernel("k1", &control);
    kernel.start().await.unwrap();

    let mut pids = Vec::new();
    for n in 0..10 {
        let process = kernel
            .processes()
            .spawn(run_on(QueueClass::Background, &format!("sleeper {n}")), sleeper())
            .await
            .unwrap();
        pids.push(process.meta.pid);
    }
    for pid in &pids {
        let run = in_state(&world.runs, pid, RunState::Waiting).await;
        assert_eq!(run.lease_owner, None, "a sleeping process holds no lease (K-I2)");
        let meta = meta(&world, pid).await;
        assert!(meta.wake.is_some(), "the wake set stays recorded while asleep");
        assert!(!meta.suspend_requested);
        assert_eq!(meta.suspended_by, None, "asleep is not suspended");
    }
    assert_eq!(world.model_calls(), 0, "sleep makes no model call (K-I2)");
    let waiting =
        kernel.processes().ps(ProcessFilter::default().states([RunState::Waiting])).await.unwrap();
    assert_eq!(waiting.len(), 10);
    assert!(waiting.iter().all(|process| process.meta.wake.is_some()));

    kernel.processes().resume(&pids[0]).await.unwrap();
    in_state(&world.runs, &pids[0], RunState::Completed).await;
    assert_eq!(world.model_calls(), 1, "only the woken process called the model");
    assert_eq!(meta(&world, &pids[0]).await.wake, None, "a woken process's wake set is spent");
    kernel.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn suspend_parks_a_running_process_and_resume_continues_it_without_rerunning_a_call() {
    let world = World::one_step();
    let control = GovernanceControl::new();
    let kernel = world.kernel("k1", &control);
    kernel.start().await.unwrap();
    let pid = kernel
        .processes()
        .spawn(run_on(QueueClass::Interactive, "go"), ProcessSpec::new(ProcessKind::Interactive))
        .await
        .unwrap()
        .meta
        .pid;
    tool_called(&world, 1).await;

    kernel.processes().suspend(&pid).await.unwrap();
    world.release.add_permits(1);
    in_state(&world.runs, &pid, RunState::Waiting).await;
    let parked = meta(&world, &pid).await;
    assert_eq!(parked.suspended_by, Some(Suspension::Manual));
    assert!(!parked.suspend_requested, "the request is cleared once the run parks");
    assert_eq!(world.model_calls(), 1, "no model call after the suspension");

    kernel.processes().resume(&pid).await.unwrap();
    in_state(&world.runs, &pid, RunState::Completed).await;
    assert_eq!(world.calls.load(Ordering::SeqCst), 1, "the completed call never runs twice");
    assert_eq!(world.model_calls(), 2);
    assert_eq!(meta(&world, &pid).await.suspended_by, None);
    kernel.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unfreeze_wakes_the_freeze_parked_process_and_no_manual_suspension() {
    let world = World::one_step();
    let control = GovernanceControl::new();
    let kernel = world.kernel("k1", &control);
    kernel.start().await.unwrap();
    let suspended = kernel
        .processes()
        .spawn(run_on(QueueClass::Background, "later"), sleeper())
        .await
        .unwrap()
        .meta
        .pid;
    in_state(&world.runs, &suspended, RunState::Waiting).await;
    kernel.processes().suspend(&suspended).await.unwrap();
    let running = kernel
        .processes()
        .spawn(run_on(QueueClass::Interactive, "go"), ProcessSpec::new(ProcessKind::Interactive))
        .await
        .unwrap()
        .meta
        .pid;
    tool_called(&world, 1).await;

    kernel.freeze("incident 12", operator()).await.unwrap();
    world.release.add_permits(1);
    in_state(&world.runs, &running, RunState::Waiting).await;
    assert_eq!(meta(&world, &running).await.suspended_by, Some(Suspension::Freeze));
    let refused = kernel.processes().resume(&running).await.unwrap_err();
    assert!(matches!(refused, KernelError::InvalidState { .. }), "{refused}");
    assert_eq!(world.model_calls(), 1);

    kernel.unfreeze(operator()).await.unwrap();
    in_state(&world.runs, &running, RunState::Completed).await;
    assert_eq!(world.calls.load(Ordering::SeqCst), 1);
    let manual = world.runs.get(&suspended).await.unwrap().unwrap();
    assert_eq!(manual.state, RunState::Waiting, "unfreeze wakes no manual suspension (K-I21)");
    assert_eq!(meta(&world, &suspended).await.suspended_by, Some(Suspension::Manual));
    kernel.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn kill_cancels_a_sleeping_process_and_refuses_a_finished_one() {
    let world = World::answer();
    let control = GovernanceControl::new();
    let kernel = world.kernel("k1", &control);
    kernel.start().await.unwrap();
    let pid = kernel
        .processes()
        .spawn(run_on(QueueClass::Background, "later"), sleeper())
        .await
        .unwrap()
        .meta
        .pid;
    in_state(&world.runs, &pid, RunState::Waiting).await;

    kernel.processes().kill(&pid, "not needed").await.unwrap();
    assert_eq!(world.runs.get(&pid).await.unwrap().unwrap().state, RunState::Cancelled);
    let refused = kernel.processes().kill(&pid, "again").await.unwrap_err();
    assert!(matches!(refused, KernelError::InvalidState { .. }), "{refused}");
    let unknown = kernel.processes().kill("no-such-run", "x").await.unwrap_err();
    assert!(matches!(unknown, KernelError::NotFound { .. }), "{unknown}");
    kernel.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nice_moves_a_queued_process_under_a_new_pid() {
    let world = World::answer();
    let control = GovernanceControl::new();
    let kernel = world.kernel("k1", &control);
    // Not started: the run stays queued.
    let old = kernel
        .processes()
        .spawn(run_on(QueueClass::Background, "hi"), ProcessSpec::new(ProcessKind::Interactive))
        .await
        .unwrap();
    let moved = kernel.processes().nice(&old.meta.pid, QueueClass::Interactive).await.unwrap();
    assert_ne!(moved.meta.pid, old.meta.pid, "the pid changes");
    assert_eq!(moved.meta.parent.as_deref(), Some(old.meta.pid.as_str()));
    assert_eq!(moved.meta.queue, QueueClass::Interactive);
    assert_eq!(moved.run.queue, "interactive");
    assert_eq!(world.runs.get(&old.meta.pid).await.unwrap().unwrap().state, RunState::Cancelled);

    kernel.start().await.unwrap();
    in_state(&world.runs, &moved.meta.pid, RunState::Completed).await;
    assert_eq!(world.model_calls(), 1);
    kernel.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nice_keeps_a_sleeper_asleep_and_refuses_a_running_process() {
    let world = World::one_step();
    let control = GovernanceControl::new();
    let kernel = world.kernel("k1", &control);
    kernel.start().await.unwrap();
    let asleep = kernel
        .processes()
        .spawn(run_on(QueueClass::Background, "later"), sleeper())
        .await
        .unwrap()
        .meta
        .pid;
    in_state(&world.runs, &asleep, RunState::Waiting).await;
    let moved = kernel.processes().nice(&asleep, QueueClass::Interactive).await.unwrap();
    let run = in_state(&world.runs, &moved.meta.pid, RunState::Waiting).await;
    assert_eq!(run.queue, "interactive");
    assert!(meta(&world, &moved.meta.pid).await.wake.is_some());
    assert_eq!(world.model_calls(), 0, "the moved sleeper is still asleep");

    let running = kernel
        .processes()
        .spawn(run_on(QueueClass::Interactive, "go"), ProcessSpec::new(ProcessKind::Interactive))
        .await
        .unwrap()
        .meta
        .pid;
    tool_called(&world, 1).await;
    let refused = kernel.processes().nice(&running, QueueClass::Background).await.unwrap_err();
    assert!(matches!(refused, KernelError::InvalidState { .. }), "{refused}");
    world.release.add_permits(1);
    in_state(&world.runs, &running, RunState::Completed).await;
    kernel.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn drain_parks_running_processes_and_resume_wakes_them() {
    let world = World::one_step();
    let control = GovernanceControl::new();
    let kernel = world.kernel("k1", &control);
    kernel.start().await.unwrap();
    let pid = kernel
        .processes()
        .spawn(run_on(QueueClass::Interactive, "go"), ProcessSpec::new(ProcessKind::Interactive))
        .await
        .unwrap()
        .meta
        .pid;
    tool_called(&world, 1).await;

    let draining = {
        let kernel = kernel.clone();
        tokio::spawn(async move { kernel.drain(Duration::from_secs(5)).await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    world.release.add_permits(1);
    draining.await.unwrap().unwrap();
    assert_eq!(world.runs.get(&pid).await.unwrap().unwrap().state, RunState::Waiting);
    assert_eq!(meta(&world, &pid).await.suspended_by, Some(Suspension::Drain));
    assert_eq!(kernel.health().state, KernelState::Paused);

    kernel.resume().await.unwrap();
    in_state(&world.runs, &pid, RunState::Completed).await;
    assert_eq!(world.calls.load(Ordering::SeqCst), 1);
    kernel.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_suspension_reaches_the_replica_that_holds_the_lease() {
    let world = World::one_step();
    let control = GovernanceControl::new();
    let holder = world.kernel("replica-a", &control);
    let other = world.kernel("replica-b", &control);
    holder.start().await.unwrap();
    let pid = holder
        .processes()
        .spawn(run_on(QueueClass::Interactive, "go"), ProcessSpec::new(ProcessKind::Interactive))
        .await
        .unwrap()
        .meta
        .pid;
    tool_called(&world, 1).await;

    // Asked on the replica that does not run it: the holder's mirror picks it up.
    other.processes().suspend(&pid).await.unwrap();
    assert!(meta(&world, &pid).await.suspend_requested);
    tokio::time::sleep(Duration::from_millis(400)).await;
    world.release.add_permits(1);
    in_state(&world.runs, &pid, RunState::Waiting).await;
    let parked = meta(&world, &pid).await;
    assert_eq!(parked.suspended_by, Some(Suspension::Manual));
    assert!(!parked.suspend_requested);
    assert_eq!(world.model_calls(), 1);
    holder.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lifecycle_calls_out_of_order_are_refused() {
    let world = World::answer();
    let control = GovernanceControl::new();
    let kernel = world.kernel("k1", &control);
    let early = kernel.pause().await.unwrap_err();
    assert!(matches!(early, KernelError::Lifecycle { state: KernelState::Built, .. }), "{early}");
    kernel.start().await.unwrap();
    let twice = kernel.start().await.unwrap_err();
    assert!(matches!(twice, KernelError::Lifecycle { state: KernelState::Running, .. }), "{twice}");
    let resume = kernel.resume().await.unwrap_err();
    assert!(matches!(resume, KernelError::Lifecycle { .. }), "{resume}");
    kernel.pause().await.unwrap();
    kernel.resume().await.unwrap();
    kernel.stop().await.unwrap();
    kernel.stop().await.unwrap();
    assert_eq!(kernel.health().state, KernelState::Stopped);
    kernel.start().await.unwrap();
    kernel.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn health_reports_each_queue_worker() {
    let world = World::answer();
    let control = GovernanceControl::new();
    let kernel = world.kernel("k1", &control);
    kernel.start().await.unwrap();
    let health = kernel.health();
    assert_eq!(health.state, KernelState::Running);
    assert!(!health.frozen);
    let queues: Vec<_> = health.workers.iter().map(|worker| worker.queue.clone()).collect();
    assert_eq!(queues, QueueClass::defaults());
    assert!(health.workers.iter().all(|worker| worker.claiming));
    assert_eq!(health.workers[1].worker_id, "k1:background");
    eventually("a store latency", || async { kernel.health().store_latency }).await;

    kernel.pause().await.unwrap();
    let paused = kernel.health();
    assert_eq!(paused.state, KernelState::Paused);
    assert!(paused.workers.iter().all(|worker| !worker.claiming));
    control.freeze("drill");
    assert!(kernel.health().frozen);
    kernel.stop().await.unwrap();
}

/// A session usage counter the test moves by hand.
struct Meter(AtomicU64);

#[async_trait]
impl ProgressSource for Meter {
    async fn usage(&self, _run: &RunRecord) -> adk_kernel::Result<Option<RunUsage>> {
        Ok(Some(RunUsage::new(0, 0, self.0.load(Ordering::SeqCst))))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn spend_is_attributed_to_the_process_that_made_it() {
    let world = World::one_step();
    let control = GovernanceControl::new();
    let meter = Arc::new(Meter(AtomicU64::new(1_000)));
    let kernel = world.kernel_with("k1", &control, |builder| {
        builder.progress_source(Arc::clone(&meter) as Arc<dyn ProgressSource>)
    });
    kernel.start().await.unwrap();
    let pid = kernel
        .processes()
        .spawn(run_on(QueueClass::Interactive, "go"), ProcessSpec::new(ProcessKind::Interactive))
        .await
        .unwrap()
        .meta
        .pid;
    tool_called(&world, 1).await;
    meter.0.fetch_add(5_000, Ordering::SeqCst);

    let view = eventually("today's spend", || async {
        let process = kernel.processes().get(&pid).await.unwrap().unwrap();
        (process.view.spend_today == 5_000).then_some(process.view)
    })
    .await;
    assert_eq!(view.spend_7d, 5_000, "spend before the claim is not the process's");
    meter.0.fetch_add(250, Ordering::SeqCst);
    world.release.add_permits(1);
    in_state(&world.runs, &pid, RunState::Completed).await;
    let finished = kernel.processes().get(&pid).await.unwrap().unwrap();
    assert_eq!(finished.view.spend_today, 5_250, "the finish attributes the last of it");
    kernel.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_admin_freeze_delegate_parks_and_reports_the_running_process() {
    let world = World::one_step();
    let control = GovernanceControl::new();
    let kernel = world.kernel("k1", &control);
    kernel.start().await.unwrap();
    let pid = kernel
        .processes()
        .spawn(run_on(QueueClass::Interactive, "go"), ProcessSpec::new(ProcessKind::Interactive))
        .await
        .unwrap()
        .meta
        .pid;
    tool_called(&world, 1).await;

    let delegate: Arc<dyn FreezeDelegate> = Arc::new(kernel.clone());
    let report = delegate.freeze("incident 13", operator()).await.unwrap();
    assert_eq!((report.parked_runs, report.cancelled_runs), (1, 0));
    world.release.add_permits(1);
    in_state(&world.runs, &pid, RunState::Waiting).await;
    delegate.unfreeze(operator()).await.unwrap();
    in_state(&world.runs, &pid, RunState::Completed).await;
    kernel.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn spawn_is_refused_while_frozen() {
    let world = World::answer();
    let control = GovernanceControl::new();
    let kernel = world.kernel("k1", &control);
    control.freeze("drill");
    let refused = kernel
        .processes()
        .spawn(run_on(QueueClass::Interactive, "hi"), ProcessSpec::new(ProcessKind::Interactive))
        .await
        .unwrap_err();
    assert!(matches!(refused, KernelError::Frozen { .. }), "{refused}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_file_saved_after_a_sleeper_is_spawned_wakes_it_within_two_seconds() {
    let dir = tempfile::tempdir().unwrap();
    let world = World::answer();
    let control = GovernanceControl::new();
    let kernel = world.kernel("k1", &control);
    kernel.start().await.unwrap();
    // Spawned after the kernel started, on a root nothing watched yet (live K3 in PR 3).
    let wake = WakeSet::any_of([Wake::FileChange {
        root: dir.path().to_path_buf(),
        glob: "notes.md".into(),
        debounce: Duration::from_millis(100),
    }]);
    let pid = kernel
        .processes()
        .spawn(
            run_on(QueueClass::Background, "read notes.md"),
            ProcessSpec::new(ProcessKind::Goal).with_wake(wake),
        )
        .await
        .unwrap()
        .meta
        .pid;
    in_state(&world.runs, &pid, RunState::Waiting).await;
    // The watcher subscribes once the spawn signals it.
    tokio::time::sleep(Duration::from_millis(300)).await;
    std::fs::write(dir.path().join("other.txt"), "x").unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(world.runs.get(&pid).await.unwrap().unwrap().state, RunState::Waiting);

    let saved = tokio::time::Instant::now();
    std::fs::write(dir.path().join("notes.md"), "remember the milk").unwrap();
    in_state(&world.runs, &pid, RunState::Completed).await;
    assert!(saved.elapsed() < Duration::from_secs(2), "woke in {:?}", saved.elapsed());
    assert_eq!(world.model_calls(), 1, "only the woken run called the model");
    kernel.stop().await.unwrap();
}
