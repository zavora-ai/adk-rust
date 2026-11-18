//! Moving a scheduled follow-up (spec 3.2 §7.5, §8.1 `adk-rust agenda schedule`) while a
//! worker holds its process's run only to park it on its wake set (K-I2): the run reads
//! `Running`, though nothing of it ran. A replica stopped by `kill -9` in that moment leaves
//! the run so until its lease lapses.

use crate::common;

use std::time::Duration;

use adk_core::principal::SubjectRef;
use adk_core::{GovernanceControl, RunState};
use adk_kernel::{
    AgendaItem, AgendaOrigin, AgendaState, Kernel, KernelError, ProcessFilter, ProcessStore,
    QueueClass, Wake, WakeSet,
};
use chrono::{DateTime, Duration as Span, Timelike, Utc};
use common::*;

/// The whole hour eight hours from now, so nothing wakes during a test.
fn evening() -> DateTime<Utc> {
    let later = Utc::now() + Span::hours(8);
    later.with_minute(0).unwrap().with_second(0).unwrap().with_nanosecond(0).unwrap()
}

fn at(time: DateTime<Utc>) -> WakeSet {
    WakeSet::any_of([Wake::At(time)])
}

/// Puts the approved follow-up `id` on the agenda and schedules it at `time`: its process is
/// spawned asleep on `time`, its run queued. Returns the process's pid.
async fn scheduled(kernel: &Kernel, id: &str, time: DateTime<Utc>) -> String {
    let item = AgendaItem::new(
        id,
        SubjectRef::human("alice"),
        "News digest",
        "the evening edition lands at 18:30",
        AgendaOrigin::User,
        Utc::now(),
    )
    .with_state(AgendaState::Approved);
    kernel.agenda().store().put(item).await.unwrap();
    kernel.agenda().schedule(id, at(time), &SubjectRef::human("alice")).await.unwrap();
    let processes = kernel.processes().ps(ProcessFilter::default()).await.unwrap();
    let process =
        processes.into_iter().find(|process| process.meta.agenda_item.as_deref() == Some(id));
    process.expect("scheduling spawns the item's process").meta.pid
}

/// A replica claims the next background run, `pid`'s, for `lease` and stops before it parks.
async fn claimed_by_a_stopped_replica(world: &World, pid: &str, lease: Duration) {
    let claimed = world.runs.claim(QueueClass::BACKGROUND, "stopped", lease).await.unwrap();
    assert_eq!(claimed.map(|claimed| claimed.record.run_id).as_deref(), Some(pid));
}

/// A follow-up claimed only to park moves to its new time, and its process, once a worker
/// recovers the run, sleeps on that time without a model call.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_follow_up_claimed_only_to_park_moves_and_sleeps_on_its_new_time() {
    let world = World::answer();
    let control = GovernanceControl::new();
    // Not started: no worker of this kernel claims the run first.
    let kernel = world.kernel("k1", &control);
    let evening = evening();
    let pid = scheduled(&kernel, "digest", evening).await;
    claimed_by_a_stopped_replica(&world, &pid, Duration::from_millis(300)).await;
    let later = evening + Span::hours(1);

    let moved =
        kernel.agenda().schedule("digest", at(later), &SubjectRef::human("alice")).await.unwrap();
    assert_eq!(moved.state, AgendaState::Scheduled { wake: at(later) });
    assert_eq!(kernel.agenda().get("digest").await.unwrap().unwrap().state, moved.state);
    assert_eq!(world.processes.get(&pid).await.unwrap().unwrap().wake, Some(at(later)));

    kernel.start().await.unwrap();
    in_state(&world.runs, &pid, RunState::Waiting).await;
    let parked = eventually("the park to be recorded", || async {
        world.processes.get(&pid).await.unwrap().filter(|meta| !meta.suspend_requested)
    })
    .await;
    assert_eq!(parked.wake, Some(at(later)));
    assert_eq!(world.model_calls(), 0, "a sleeper parks before its first model call");
    kernel.stop().await.unwrap();
}

/// A process whose wake was consumed has started: its item is not moved.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_follow_up_whose_process_started_is_not_moved() {
    let world = World::answer();
    let control = GovernanceControl::new();
    let kernel = world.kernel("k1", &control);
    let evening = evening();
    let pid = scheduled(&kernel, "digest", evening).await;
    // A wake clears the set before it makes the run claimable (K-I2).
    world.processes.set_wake(&pid, None).await.unwrap();
    claimed_by_a_stopped_replica(&world, &pid, Duration::from_secs(60)).await;

    let error = kernel
        .agenda()
        .schedule("digest", at(evening + Span::hours(1)), &SubjectRef::human("alice"))
        .await
        .unwrap_err();
    assert!(matches!(error, KernelError::AgendaState { .. }), "{error}");
    assert!(error.to_string().contains("already started"), "{error}");
    assert_eq!(
        kernel.agenda().get("digest").await.unwrap().unwrap().state,
        AgendaState::Scheduled { wake: at(evening) }
    );
}
