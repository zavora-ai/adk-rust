//! Hosting the agents a person adds (spec 3.3 §5.7, §10 K7, S4, S-I15, K-I11, K-I21): the
//! kernel refuses a foreign kill switch, keeps an agent it cannot build as "Can't start",
//! rebuilds active agents after a restart, converges replicas on one store within a tick,
//! routes progress by target, holds an agent's unattended work past its daily limit, starts
//! nothing while frozen, and never hands an agenda follow-up to an added agent.

mod agents_support;

use std::sync::Arc;
use std::sync::atomic::Ordering;

use adk_core::audit_log::AuditActor;
use adk_core::principal::SubjectRef;
use adk_core::{AdkError, NewRun, RunState};
use adk_kernel::agents::{AgentDefinition, AgentStore};
use adk_kernel::stores::SqliteAgentStore;
use adk_kernel::{
    AgendaItem, AgendaOrigin, AgendaState, Partition, PartitionMode, Partitions, ProcessKind,
    ProcessSpec, ProcessStore, QueueClass, SpendDelta,
};
use agents_support::{APP, PERSON, World, add, eventually, in_state, runner};
use serde_json::json;

fn finance() -> AgentDefinition {
    AgentDefinition::new("Finance", "default").with_mode(PartitionMode::Ask)
}

fn run(target: &str, queue: &str) -> NewRun {
    NewRun::new(queue, APP, PERSON, target, json!("hello"))
        .with_session_id(format!("s-{}", uuid::Uuid::new_v4().simple()))
}

async fn spawn(kernel: &adk_kernel::Kernel, target: &str) -> String {
    kernel
        .processes()
        .spawn(run(target, QueueClass::INTERACTIVE), ProcessSpec::new(ProcessKind::Interactive))
        .await
        .unwrap()
        .meta
        .pid
}

/// A runner under another kill switch is never hosted (S-I15): the agent shows "Can't
/// start", and its run is handed back for a later claim, not failed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_runner_under_another_kill_switch_is_never_hosted() {
    let world = World::new();
    world.factory.foreign.store(true, Ordering::SeqCst);
    let kernel = world.kernel("k1");
    kernel.start().await.unwrap();
    add(&kernel, finance()).await;
    let view = kernel.agents().get("finance").await.unwrap().unwrap();
    let reason = view.cant_start.expect("it cannot start");
    assert!(reason.contains("kill switch is not the kernel's"), "{reason}");
    let pid = spawn(&kernel, "finance").await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let waiting = world.runs.get(&pid).await.unwrap().unwrap();
    assert_ne!(waiting.state, RunState::Failed, "an expected target is handed back");
    assert_ne!(waiting.state, RunState::Completed);
    kernel.stop().await.unwrap();
}

/// A build error keeps the record and shows the reason; the agent runs once a later version
/// builds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_build_error_shows_cant_start_and_keeps_the_record() {
    let world = World::new();
    world.factory.broken.lock().unwrap().insert("finance".into());
    let kernel = world.kernel("k1");
    kernel.start().await.unwrap();
    let record = add(&kernel, finance()).await;
    let listed = kernel.agents().list().await.unwrap();
    let reason = listed[0].cant_start.clone().expect("it cannot start");
    assert!(reason.contains("No key for Anthropic"), "{reason}");
    assert_eq!(listed[0].agent.state, adk_kernel::agents::AgentRecordState::Active);

    world.factory.broken.lock().unwrap().clear();
    let change = adk_kernel::agents::AgentAction::Change {
        id: "finance".into(),
        expected_version: record.version,
        definition: record.definition().with_description("Now with a key"),
    };
    agents_support::apply(&kernel, &agents_support::local(), change).await.unwrap();
    assert_eq!(kernel.agents().get("finance").await.unwrap().unwrap().cant_start, None);
    let pid = spawn(&kernel, "finance").await;
    in_state(&world.runs, &pid, RunState::Completed).await;
    kernel.stop().await.unwrap();
}

/// A restarted kernel rebuilds every active agent before its first claim.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restart_rebuilds_active_agents() {
    let world = World::new();
    let first = world.kernel("k1");
    add(&first, finance()).await;
    drop(first);
    let restarted = world.kernel("k2");
    restarted.start().await.unwrap();
    let pid = spawn(&restarted, "finance").await;
    let done = in_state(&world.runs, &pid, RunState::Completed).await;
    assert_eq!(done.target, "finance");
    restarted.stop().await.unwrap();
}

/// Two kernels on one SQLite file converge within a tick: an agent one adds serves on the
/// other, which never saw the change.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_kernels_on_one_sqlite_file_converge_within_a_tick() {
    let path = std::env::temp_dir().join(format!("adk-kernel-agents-{}.db", uuid::Uuid::new_v4()));
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let open = || async {
        let store = SqliteAgentStore::new(sqlx::SqlitePool::connect(&url).await.unwrap());
        store.migrate().await.unwrap();
        Arc::new(store) as Arc<dyn AgentStore>
    };
    let world = World::new();
    let (first_store, second_store) = (open().await, open().await);
    let adding = world.kernel_with("k1", |builder| builder.agent_store(first_store));
    let serving = world.kernel_with("k2", |builder| builder.agent_store(second_store));
    serving.start().await.unwrap();
    let mut events = serving.events();
    add(&adding, finance()).await;
    // The serving replica hosts it on its next tick, without having seen the change.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let record = events.recv().await.expect("the bus stays open");
            if let adk_kernel::KernelEvent::Agent { id, change, .. } = record.event
                && id == "finance"
                && change == adk_kernel::agents::AgentChange::Hosted
            {
                return;
            }
        }
    })
    .await
    .expect("hosted within a tick");
    let pid = spawn(&serving, "finance").await;
    in_state(&world.runs, &pid, RunState::Completed).await;
    serving.stop().await.unwrap();
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

/// An added agent's processes are read through its own progress source, before its area's.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn progress_is_routed_by_target_before_area() {
    let world = World::new();
    let kernel = world.kernel("k1");
    kernel.start().await.unwrap();
    add(&kernel, finance()).await;
    let mine = spawn(&kernel, "finance").await;
    let primary = spawn(&kernel, APP).await;
    in_state(&world.runs, &mine, RunState::Completed).await;
    let view = kernel.processes().get(&mine).await.unwrap().unwrap().view;
    assert_eq!(view.loop_guard.as_deref(), Some("guard of finance"));
    let other = kernel.processes().get(&primary).await.unwrap().unwrap().view;
    assert_eq!(other.loop_guard, None, "the primary agent keeps its own source");
    kernel.stop().await.unwrap();
}

/// An agent's own daily limit holds its unattended work until the next UTC day (K-I11); a
/// turn the person starts still runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agents_daily_limit_holds_its_unattended_work() {
    let world = World::new();
    let kernel = world.kernel("k1");
    kernel.start().await.unwrap();
    add(&kernel, finance().with_daily_limit(1_000)).await;
    let earlier = spawn(&kernel, "finance").await;
    in_state(&world.runs, &earlier, RunState::Completed).await;
    let today = chrono::Utc::now().date_naive();
    world
        .processes
        .add_spend(&earlier, &"default".into(), today, SpendDelta::new(1_000))
        .await
        .unwrap();

    let routine = kernel
        .processes()
        .spawn(run("finance", QueueClass::BACKGROUND), ProcessSpec::new(ProcessKind::Scheduled))
        .await
        .unwrap()
        .meta
        .pid;
    let held = eventually("the routine to be held", || async {
        world.processes.get(&routine).await.ok().flatten().filter(|meta| meta.wake.is_some())
    })
    .await;
    assert_eq!(
        held.labels.get("kernel.admission").map(String::as_str),
        Some("the agent's daily limit")
    );
    in_state(&world.runs, &routine, RunState::Waiting).await;

    let turn = spawn(&kernel, "finance").await;
    in_state(&world.runs, &turn, RunState::Completed).await;
    kernel.stop().await.unwrap();
}

/// Adding an agent works while everything is stopped, and nothing it does starts until the
/// freeze is lifted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn adding_while_frozen_starts_nothing_until_unfreeze() {
    let world = World::new();
    let kernel = world.kernel("k1");
    kernel.start().await.unwrap();
    kernel.freeze("stop all", AuditActor::default()).await.unwrap();
    add(&kernel, finance()).await;
    let refused = kernel
        .processes()
        .spawn(run("finance", QueueClass::INTERACTIVE), ProcessSpec::new(ProcessKind::Interactive))
        .await
        .unwrap_err();
    assert_eq!(AdkError::from(refused).code, "kernel.frozen");
    kernel.unfreeze(AuditActor::default()).await.unwrap();
    let pid = spawn(&kernel, "finance").await;
    in_state(&world.runs, &pid, RunState::Completed).await;
    kernel.stop().await.unwrap();
}

/// A follow-up without a proposing run goes to its area's own agent, never to an added one,
/// even one whose id sorts first (spec 3.3 §5.7).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_follow_up_without_a_proposer_never_goes_to_an_added_agent() {
    let world = World::new();
    let partitions = Partitions::in_memory();
    partitions
        .put(Partition::new("home", "home").with_mode(PartitionMode::Ask), AuditActor::default())
        .await
        .unwrap();
    let home_runner = runner("adk-rust@home", &world.sessions, &world.control);
    let kernel = world.kernel_with("k1", |builder| {
        builder.partitions(partitions).partition_agent("home", "adk-rust@home", home_runner)
    });
    let accounts =
        add(&kernel, AgentDefinition::new("Accounts", "home").with_mode(PartitionMode::Ask)).await;
    assert_eq!(accounts.id, "accounts");
    let item = AgendaItem::new(
        "item-1",
        SubjectRef::human(PERSON),
        "Tidy the inbox",
        "Weekly",
        AgendaOrigin::User,
        chrono::Utc::now(),
    )
    .with_partition("home");
    kernel.agenda().propose(item, PartitionMode::Ask).await.unwrap();
    let approved = kernel.agenda().approve("item-1", &SubjectRef::human(PERSON)).await.unwrap();
    assert!(matches!(approved.state, AgendaState::Scheduled { .. }), "{:?}", approved.state);
    let process = kernel
        .processes()
        .ps(adk_kernel::ProcessFilter::default().partition("home"))
        .await
        .unwrap()
        .into_iter()
        .find(|process| process.meta.agenda_item.as_deref() == Some("item-1"))
        .expect("the follow-up's process");
    assert_eq!(process.run.target, "adk-rust@home");
}
