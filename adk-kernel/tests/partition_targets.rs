//! A kernel hosting partition agents (spec 3.2 §7.8, K-I15): each partition's runner is a
//! target bound to its partition, so a process of another partition cannot run it, and the
//! kernel's partition table and audit trail are the ones the partition harnesses use.

mod common;

use std::sync::Arc;

use adk_core::audit_log::{AuditActor, AuditTrail, InMemoryAuditRecorder};
use adk_core::{GovernanceControl, NewRun};
use adk_kernel::audit::PARTITION_CREATED;
use adk_kernel::{
    KernelError, Partition, PartitionId, Partitions, ProcessFilter, ProcessKind, ProcessSpec,
    QueueClass,
};
use common::{APP, World};
use serde_json::json;

const WORK_TARGET: &str = "adk-rust@work";

fn work_run() -> NewRun {
    NewRun::new(QueueClass::BACKGROUND, APP, "alice", WORK_TARGET, json!({ "text": "standup" }))
}

#[tokio::test]
async fn a_partition_agents_target_runs_only_its_partition() {
    let world = World::answer();
    let control = GovernanceControl::new();
    let partitions = Partitions::in_memory();
    let runner = world.runner(&control);
    let kernel = world.kernel_with("k1", &control, |builder| {
        builder.partitions(partitions.clone()).partition_agent("work", WORK_TARGET, runner)
    });
    assert_eq!(kernel.partition_of_target(WORK_TARGET), Some(&PartitionId::new("work")));
    assert_eq!(kernel.partition_of_target(common::TARGET), None);

    // Spawned without a partition, it runs in the target's.
    let process =
        kernel.processes().spawn(work_run(), ProcessSpec::new(ProcessKind::Goal)).await.unwrap();
    assert_eq!(process.meta.partition, PartitionId::new("work"));

    // Another partition cannot run it.
    let refused = kernel
        .processes()
        .spawn(work_run(), ProcessSpec::new(ProcessKind::Goal).with_partition("home"))
        .await
        .unwrap_err();
    assert!(
        matches!(&refused, KernelError::Config { reason } if reason.contains("K-I15")),
        "{refused}"
    );

    // A run of the target enqueued outside the kernel is listed in the target's partition.
    world.runs.enqueue(work_run()).await.unwrap();
    let listed = kernel.processes().ps(ProcessFilter::default().partition("work")).await.unwrap();
    assert_eq!(listed.len(), 2, "{listed:?}");
    assert!(
        kernel
            .processes()
            .ps(ProcessFilter::default().partition("default"))
            .await
            .unwrap()
            .is_empty()
    );
}

/// K-I15: a partition agent's process cannot continue a session another partition's agent
/// ran.
#[tokio::test]
async fn a_partition_agent_cannot_continue_another_partitions_session() {
    let world = World::answer();
    let control = GovernanceControl::new();
    let (home, work) = (world.runner(&control), world.runner(&control));
    let kernel = world.kernel_with("k1", &control, |builder| {
        builder.partition_agent("home", "adk-rust@home", home).partition_agent(
            "work",
            WORK_TARGET,
            work,
        )
    });
    let in_session = |target: &str| {
        NewRun::new(QueueClass::BACKGROUND, APP, "alice", target, json!({ "text": "x" }))
            .with_session_id("s-home")
    };
    let spec = || ProcessSpec::new(ProcessKind::Interactive);
    kernel.processes().spawn(in_session("adk-rust@home"), spec()).await.unwrap();
    let refused = kernel.processes().spawn(in_session(WORK_TARGET), spec()).await.unwrap_err();
    assert!(
        matches!(&refused, KernelError::Config { reason } if reason.contains("belongs to partition `home`")),
        "{refused}"
    );
    // The session's own partition continues it.
    kernel.processes().spawn(in_session("adk-rust@home"), spec()).await.unwrap();
}

#[tokio::test]
async fn the_kernel_shares_the_partition_table_and_writes_its_records_to_the_kernel_trail() {
    let world = World::answer();
    let control = GovernanceControl::new();
    let recorder = InMemoryAuditRecorder::new();
    let partitions = Partitions::in_memory();
    let kernel = world.kernel_with("k1", &control, |builder| {
        builder.partitions(partitions.clone()).audit(AuditTrail::new(Arc::new(recorder.clone())))
    });
    partitions.put(Partition::new("work", "work"), AuditActor::default()).await.unwrap();
    let listed = kernel.partitions().list().await.unwrap();
    assert_eq!(listed, [Partition::new("work", "work")]);
    assert!(recorder.records().iter().any(|draft| draft.action == PARTITION_CREATED));
}
