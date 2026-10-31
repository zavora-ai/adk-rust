//! Properties of the supervisor under random operation sequences (spec 3.2 §9.2, §11):
//! a `Waiting` process never holds a lease (K-I2); a process spawned asleep makes no model
//! call until it is resumed, however slowly the side table is written; and a freeze is
//! monotone — within one second of `freeze` returning no process is `Running`, and no model
//! call is made until `unfreeze` (K-I8). Each case runs over a process store whose writes
//! take a generated 0–120 ms.

use crate::common;

use std::time::Duration;

use adk_core::audit_log::AuditActor;
use adk_core::{GovernanceControl, RunFilter, RunState, SubjectRef};
use adk_kernel::{ProcessKind, ProcessSpec, QueueClass, Wake, WakeSet};
use adk_model::ScriptedResponse;
use common::*;
use proptest::prelude::*;
use serde_json::json;

#[derive(Debug, Clone)]
enum Op {
    Spawn,
    SpawnAsleep,
    Suspend(usize),
    Resume(usize),
    Kill(usize),
    Freeze,
    Unfreeze,
    Settle,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => Just(Op::Spawn),
        2 => Just(Op::SpawnAsleep),
        2 => any::<usize>().prop_map(Op::Suspend),
        2 => any::<usize>().prop_map(Op::Resume),
        1 => any::<usize>().prop_map(Op::Kill),
        2 => Just(Op::Freeze),
        2 => Just(Op::Unfreeze),
        2 => Just(Op::Settle),
    ]
}

fn operator() -> AuditActor {
    AuditActor::subject(SubjectRef::human("operator"))
}

/// Every run, as the store holds it now.
async fn all_runs(world: &World) -> Vec<adk_core::RunRecord> {
    world.runs.list(RunFilter::default()).await.unwrap()
}

/// K-I8: within one second of a freeze no process is `Running`. A claim that raced the
/// freeze holds its run `Running` for the instant before the worker hands it back
/// unstarted, so the check waits out that instant; the model-call count stays exact.
async fn nothing_runs_within_a_second(world: &World) {
    let started = tokio::time::Instant::now();
    loop {
        let running: Vec<String> = all_runs(world)
            .await
            .into_iter()
            .filter(|run| run.state == RunState::Running)
            .map(|run| run.run_id)
            .collect();
        if running.is_empty() {
            return;
        }
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "{running:?} still running a second after the freeze (K-I8)"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn check(ops: Vec<Op>, write_delay: Duration) {
    let mut script = Vec::new();
    for _ in 0..64 {
        script.push(ScriptedResponse::function_call("step", json!({})));
        script.push(ScriptedResponse::text("done"));
    }
    let world = World::new(script);
    world.release.add_permits(10_000);
    let control = GovernanceControl::new();
    let slow: std::sync::Arc<dyn adk_kernel::ProcessStore> =
        std::sync::Arc::new(SlowPuts { inner: world.processes.clone(), delay: write_delay });
    let kernel = world.kernel_with("k1", &control, |builder| builder.process_store(slow));
    kernel.start().await.unwrap();
    let mut pids: Vec<String> = Vec::new();
    // Sleepers no `Resume` has touched: they must never call the model.
    let mut asleep: Vec<String> = Vec::new();
    // Model calls when the freeze took hold, while frozen.
    let mut frozen_at: Option<usize> = None;
    for op in ops {
        let pick = |n: usize| (!pids.is_empty()).then(|| pids[n % pids.len()].clone());
        match op {
            Op::Spawn | Op::SpawnAsleep => {
                let mut spec = ProcessSpec::new(ProcessKind::Interactive);
                if matches!(op, Op::SpawnAsleep) {
                    spec = spec.with_wake(WakeSet::any_of([Wake::Manual]));
                }
                if let Ok(process) =
                    kernel.processes().spawn(run_on(QueueClass::Interactive, "go"), spec).await
                {
                    if matches!(op, Op::SpawnAsleep) {
                        asleep.push(process.meta.pid.clone());
                    }
                    pids.push(process.meta.pid);
                }
            }
            Op::Suspend(n) => {
                if let Some(pid) = pick(n) {
                    let _ = kernel.processes().suspend(&pid).await;
                }
            }
            Op::Resume(n) => {
                if let Some(pid) = pick(n) {
                    asleep.retain(|sleeper| *sleeper != pid);
                    let _ = kernel.processes().resume(&pid).await;
                }
            }
            Op::Kill(n) => {
                if let Some(pid) = pick(n) {
                    let _ = kernel.processes().kill(&pid, "property").await;
                }
            }
            Op::Freeze => {
                kernel.freeze("property", operator()).await.unwrap();
                nothing_runs_within_a_second(&world).await;
                frozen_at.get_or_insert(world.model_calls());
            }
            Op::Unfreeze => {
                kernel.unfreeze(operator()).await.unwrap();
                frozen_at = None;
            }
            Op::Settle => tokio::time::sleep(Duration::from_millis(50)).await,
        }
        for run in all_runs(&world).await {
            if run.state == RunState::Waiting {
                assert_eq!(
                    run.lease_owner, None,
                    "waiting run {} holds a lease (K-I2)",
                    run.run_id
                );
            }
        }
        if frozen_at.is_some() {
            nothing_runs_within_a_second(&world).await;
        }
        if let Some(calls) = frozen_at {
            assert_eq!(world.model_calls(), calls, "a model call was made while frozen (K-I8)");
        }
        for pid in &asleep {
            assert!(!made_a_model_call(&world, pid).await, "sleeper {pid} called the model (K-I2)");
        }
    }
    // Give a sleeper that would wrongly run time to reach the model.
    tokio::time::sleep(Duration::from_millis(100)).await;
    for pid in &asleep {
        assert!(!made_a_model_call(&world, pid).await, "sleeper {pid} called the model (K-I2)");
    }
    if frozen_at.is_some() {
        kernel.unfreeze(operator()).await.unwrap();
    }
    kernel.stop().await.unwrap();
}

proptest! {
    // `PROPTEST_CASES` raises the count for a soak run.
    #![proptest_config(ProptestConfig {
        cases: std::env::var("PROPTEST_CASES")
            .ok()
            .and_then(|cases| cases.parse().ok())
            .unwrap_or(100),
        ..ProptestConfig::default()
    })]

    #[test]
    fn waiting_processes_hold_no_lease_sleepers_stay_asleep_and_nothing_runs_while_frozen(
        ops in prop::collection::vec(op(), 1..8),
        write_delay_ms in 0u64..120,
    ) {
        run(ops, Duration::from_millis(write_delay_ms));
    }
}

fn run(ops: Vec<Op>, write_delay: Duration) {
    let runtime =
        tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    runtime.block_on(check(ops, write_delay));
}

/// The case the property shrank to on 793c621a, a sleeper that called the model: replayed
/// on every run, since the repository ignores proptest's seed files.
#[test]
fn a_sleeper_spawned_then_frozen_suspended_and_unfrozen_stays_asleep() {
    run(vec![Op::SpawnAsleep, Op::Freeze, Op::Suspend(0), Op::Unfreeze], Duration::ZERO);
}

/// A case a 2000-case run found: the resumed process's claim races the freeze, holds the
/// run `Running` for an instant, and the worker hands it back unstarted.
#[test]
fn a_claim_that_races_the_freeze_is_handed_back_without_running() {
    run(
        vec![Op::Settle, Op::SpawnAsleep, Op::Suspend(0), Op::Resume(0), Op::Freeze],
        Duration::from_millis(49),
    );
}
