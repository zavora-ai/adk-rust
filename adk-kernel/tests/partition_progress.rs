//! Each partition's progress source (spec 3.2 §7.8, §7.10,
//! `KernelBuilder::partition_progress_source`): the kernel reads a process's usage,
//! loop-guard verdict and goal progress from its partition's own source, by the process's
//! partition, and hands every source the process table and the ledger; a partition without
//! a source falls back to the main one and keeps its own partition.

use crate::common;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use adk_core::{GovernanceControl, RunRecord, RunState};
use adk_kernel::{
    KernelError, ProcessHandle, ProcessKind, ProcessSpec, ProgressSource, QueueClass, RunUsage,
};
use adk_model::ScriptedResponse;
use async_trait::async_trait;
use common::{World, eventually, in_state, run_on, tool_called};
use serde_json::json;

/// A progress source the test moves by hand, recording what the kernel asked and gave it.
#[derive(Default)]
struct Source {
    name: &'static str,
    cost: AtomicU64,
    asked: Mutex<Vec<String>>,
    handle: Mutex<Option<ProcessHandle>>,
    #[cfg(feature = "proc")]
    ledger: Mutex<Option<Arc<dyn adk_kernel::proc::LedgerSink>>>,
}

impl Source {
    fn new(name: &'static str) -> Arc<Self> {
        Arc::new(Self { name, ..Self::default() })
    }

    fn asked(&self) -> Vec<String> {
        let mut asked = self.asked.lock().unwrap().clone();
        asked.sort();
        asked.dedup();
        asked
    }

    fn handle(&self) -> ProcessHandle {
        self.handle.lock().unwrap().clone().expect("the kernel attached its processes")
    }
}

#[async_trait]
impl ProgressSource for Source {
    async fn usage(&self, run: &RunRecord) -> adk_kernel::Result<Option<RunUsage>> {
        self.asked.lock().unwrap().push(run.run_id.clone());
        Ok(Some(RunUsage::new(0, 0, self.cost.load(Ordering::SeqCst))))
    }

    async fn loop_guard(&self, _run: &RunRecord) -> adk_kernel::Result<Option<String>> {
        Ok(Some(format!("{} verdict", self.name)))
    }

    fn attach(&self, processes: ProcessHandle) {
        *self.handle.lock().unwrap() = Some(processes);
    }

    #[cfg(feature = "proc")]
    fn attach_ledger(&self, ledger: Arc<dyn adk_kernel::proc::LedgerSink>) {
        *self.ledger.lock().unwrap() = Some(ledger);
    }
}

fn spawn_in(partition: &str) -> (adk_core::NewRun, ProcessSpec) {
    (
        run_on(QueueClass::Interactive, partition),
        ProcessSpec::new(ProcessKind::Interactive).with_partition(partition),
    )
}

/// Spec 3.2 §7.8: home and work each have their own source and ops has none. Each
/// process's spend comes from its partition's source, ops's from the main one; each view
/// shows its own source's verdict; a source parks its own partition's process through the
/// table it was given; and ops's process rows keep `ops`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn each_partition_reads_its_own_progress_source() {
    let call = || ScriptedResponse::function_call("step", json!({}));
    let done = || ScriptedResponse::text("done");
    let world = World::new(vec![call(), call(), call(), done(), done(), done()]);
    let control = GovernanceControl::new();
    let (main, home, work) = (Source::new("main"), Source::new("home"), Source::new("work"));
    #[cfg(feature = "proc")]
    let ledger_store = Arc::new(adk_kernel::proc::InMemoryLedgerStore::new());
    let kernel = world.kernel_with("k1", &control, |builder| {
        let builder = builder
            .progress_source(main.clone())
            .partition_progress_source("home", home.clone())
            .partition_progress_source("work", work.clone());
        #[cfg(feature = "proc")]
        let builder = builder.ledger(adk_kernel::proc::Ledger::new(
            ledger_store.clone(),
            adk_kernel::proc::LedgerKey::from_bytes([7; 32]),
        ));
        builder
    });
    kernel.start().await.unwrap();
    let mut pids = Vec::new();
    for partition in ["home", "work", "ops"] {
        let (run, spec) = spawn_in(partition);
        pids.push(kernel.processes().spawn(run, spec).await.unwrap().meta.pid);
    }
    let (home_pid, work_pid, ops_pid) = (pids[0].clone(), pids[1].clone(), pids[2].clone());
    tool_called(&world, 3).await;
    home.cost.fetch_add(1_000, Ordering::SeqCst);
    work.cost.fetch_add(7_000, Ordering::SeqCst);
    main.cost.fetch_add(300, Ordering::SeqCst);

    for (pid, spend, verdict) in [
        (&home_pid, 1_000, "home verdict"),
        (&work_pid, 7_000, "work verdict"),
        (&ops_pid, 300, "main verdict"),
    ] {
        let view = eventually(&format!("{pid}'s spend"), || async {
            let process = kernel.processes().get(pid).await.unwrap().unwrap();
            (process.view.spend_today == spend).then_some(process.view)
        })
        .await;
        assert_eq!(view.loop_guard.as_deref(), Some(verdict), "{pid}");
    }
    assert_eq!(home.asked(), [home_pid.as_str()]);
    assert_eq!(work.asked(), [work_pid.as_str()]);
    // Ops has no source of its own; the main one also answers for the kernel's own
    // processes in `default`, and never for home's or work's.
    let asked_main = main.asked();
    assert!(asked_main.contains(&ops_pid), "{asked_main:?}");
    assert!(!asked_main.contains(&home_pid) && !asked_main.contains(&work_pid), "{asked_main:?}");

    // Each source was handed the table and parks its own process with it.
    for source in [&main, &home, &work] {
        assert!(source.handle.lock().unwrap().is_some(), "{} was attached", source.name);
    }
    assert!(work.handle().suspend(&work_pid).await.unwrap());
    world.release.add_permits(3);
    in_state(&world.runs, &home_pid, RunState::Completed).await;
    in_state(&world.runs, &ops_pid, RunState::Completed).await;
    in_state(&world.runs, &work_pid, RunState::Waiting).await;
    let parked = kernel.processes().get(&work_pid).await.unwrap().unwrap();
    assert!(parked.meta.suspended_by.is_some(), "{:?}", parked.meta);

    #[cfg(feature = "proc")]
    {
        use adk_kernel::proc::{Activity, LedgerKind, LedgerQuery, LedgerStore as _};

        for source in [&main, &home, &work] {
            let sink = source.ledger.lock().unwrap().clone().expect("the ledger was attached");
            sink.record(Activity::new(LedgerKind::Intent, source.name).with_partition(source.name))
                .await
                .unwrap();
        }
        let rows = ledger_store.list(&LedgerQuery::default()).await.unwrap();
        for name in ["home", "work"] {
            assert!(
                rows.iter().any(|row| row.agent == name && row.partition.as_str() == name),
                "{name}'s source writes in {name}: {rows:?}"
            );
        }
        // The kernel's rows of the three processes carry each one's own partition, ops's
        // included, whose progress the main source answers; so do the wakes of home's and
        // ops's finished runs, which nothing waited on.
        let partitions = |kind: &str| -> std::collections::BTreeSet<&str> {
            rows.iter()
                .filter(|row| row.kind == kind && row.agent != "kernel")
                .map(|row| row.partition.as_str())
                .collect()
        };
        assert_eq!(partitions("process"), ["home", "ops", "work"].into(), "{rows:?}");
        let wakes: std::collections::BTreeSet<&str> = rows
            .iter()
            .filter(|row| row.kind == "wake")
            .map(|row| row.partition.as_str())
            .collect();
        assert!(wakes.contains("home") && wakes.contains("ops"), "{rows:?}");
        assert!(!wakes.contains("work"), "work's run did not finish: {rows:?}");
        // Only the kernel's own work is `default`'s: its maintenance processes, the
        // processes routing wakes, and the wake of its own finished run. The main source
        // answers for those processes too, so one still running when its cost rose spends
        // under the kernel process's identity.
        assert!(
            rows.iter().filter(|row| row.partition.as_str() == "default").all(|row| {
                row.agent == "kernel"
                    || (row.kind == "spend" && row.agent.ends_with("/kernel.proc"))
            }),
            "{rows:?}"
        );
    }
    kernel.stop().await.unwrap();
}

/// Two sources for one partition are refused at build, naming the partition; never
/// last-wins.
#[tokio::test]
async fn a_second_source_for_a_partition_is_refused() {
    let world = World::answer();
    let runs = world.runs.clone();
    let refused = adk_kernel::Kernel::builder()
        .run_store(runs)
        .partition_progress_source("work", Source::new("first"))
        .partition_progress_source("home", Source::new("home"))
        .partition_progress_source("work", Source::new("second"))
        .build()
        .unwrap_err();
    assert!(
        matches!(&refused, KernelError::Config { reason } if reason.contains("partition `work` has two progress sources")),
        "{refused}"
    );
}
