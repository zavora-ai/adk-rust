//! Spec 3.3 §12 phase 4, "Work stays at work", as one run of a kernel hosting two areas'
//! harnesses on a scripted model and a `ManualClock`, on SQLite (CI) and on PostgreSQL
//! (`work_stays_at_work_on_postgres`, `#[ignore]` and run by hand):
//!
//! ```bash
//! DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres \
//!   cargo nextest run -p adk-harness --features kernel,memory,postgres --run-ignored only \
//!   --test integration -E 'test(/^work_stays_at_work::/)'
//! ```
//!
//! | When (UTC) | What happens | What holds |
//! |---|---|---|
//! | Fri 08:50 | Home's agent saves a file | Home asks first: the call waits for a person |
//! | Fri 08:50 | Work's agent, in Go ahead, reads Home's calendar | The crossing waits for a person; denied, nothing of Home reaches Work |
//! | Fri 09:00 | Work's weekday routine (`0 9 * * 1-5`) and a Home errand come due | The routine runs without asking; Home's limit, already spent, holds the errand |
//! | Fri | Spending | Each agent spent; per agent it sums to the areas' totals, which sum to the day's total |
//! | Sat, Sun 09:00 | | The routine does not run |
//! | Mon 09:00, 10:00 | The routine runs; then Work's limit drops below its spend and a Work errand comes due | Work's limit holds the errand |
//!
//! Work is the coordinator: spec 3.2 §7.8 (K7, K-I15) lets only the coordinator cross, under a
//! non-waivable approval, and denies any other agent's crossing at the policy
//! (`kernel_partitions.rs`). The calendar is Home's memory; the fact store stays in memory on
//! both legs.

#![cfg(all(feature = "kernel", feature = "memory"))]

use crate::common;

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use adk_core::audit_log::AuditActor;
use adk_core::{
    ApprovalStore, DurableRunStore, ManualClock, NewRun, RunState, SessionId, UsageMetadata,
};
use adk_harness::{
    FactKind, FactScope, FactSource, FactStatus, FactStore, Harness, HarnessAgent, HarnessDecision,
    HarnessMode, InMemoryFactStore, LongTermMemoryConfig, MemoryFact, PARTITION_MEMORY_SEARCH,
    Provenance,
};
use adk_kernel::control::{ControlClient, ControlPlane, ControlPlaneConfig, ControlStore};
use adk_kernel::{
    CronWatermarkStore, CrossPartition, CrossingFilter, CrossingOutcome, Kernel, KernelConfig,
    Partition, PartitionId, PartitionMode, PartitionStore, Partitions, ProcessKind, ProcessSpec,
    ProcessStore, QueueClass, Wake, WakeSet,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_server::background::DurableWorkerConfig;
use adk_session::SessionService;
use chrono::{DateTime, TimeZone, Utc};
use common::{APP, no_user_file, user};
use serde_json::json;

/// What each model call costs: $0.0015.
const CALL: u64 = 1_500;
const ROUTINE: &str = "0 9 * * 1-5";
const CALENDAR: &str = "dentist on Tuesday at 10:00";

/// 23 October 2026 is a Friday.
fn at(day: u32, hour: u32, minute: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, day, hour, minute, 0).unwrap()
}

fn priced(response: ScriptedResponse) -> ScriptedResponse {
    response.with_usage(UsageMetadata {
        total_token_count: 100,
        cost: Some(CALL as f64 / 1_000_000.0),
        ..UsageMetadata::default()
    })
}

fn write(path: &str, content: &str) -> ScriptedResponse {
    priced(ScriptedResponse::function_call(
        "write_file",
        json!({ "path": path, "content": content }),
    ))
}

fn text(text: &str) -> ScriptedResponse {
    priced(ScriptedResponse::text(text))
}

/// The stores one leg keeps in its database.
struct Stores {
    runs: Arc<dyn DurableRunStore>,
    processes: Arc<dyn ProcessStore>,
    sessions: Arc<dyn SessionService>,
    approvals: Arc<dyn ApprovalStore>,
    partitions: Arc<dyn PartitionStore>,
    control: Arc<dyn ControlStore>,
    watermarks: Arc<dyn CronWatermarkStore>,
}

async fn sqlite(path: &std::path::Path, clock: &ManualClock) -> Stores {
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
    use std::str::FromStr;
    let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
        .unwrap()
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_secs(30));
    let pool = SqlitePoolOptions::new().max_connections(8).connect_with(options).await.unwrap();
    let runs =
        adk_session::SqliteRunStore::from_pool(pool.clone()).with_clock(Arc::new(clock.clone()));
    runs.migrate().await.unwrap();
    let sessions = adk_session::SqliteSessionService::from_pool(pool.clone());
    sessions.migrate().await.unwrap();
    let approvals = adk_session::SqliteApprovalStore::from_pool(pool.clone());
    approvals.migrate().await.unwrap();
    let processes = adk_kernel::SqliteProcessStore::new(pool.clone());
    processes.migrate().await.unwrap();
    let partitions = adk_kernel::SqlitePartitionStore::new(pool.clone());
    partitions.migrate().await.unwrap();
    let control = adk_kernel::control::SqliteControlStore::new(pool.clone());
    control.migrate().await.unwrap();
    let watermarks = adk_kernel::SqliteCronWatermarkStore::new(pool);
    watermarks.migrate().await.unwrap();
    Stores {
        runs: Arc::new(runs),
        processes: Arc::new(processes),
        sessions: Arc::new(sessions),
        approvals: Arc::new(approvals),
        partitions: Arc::new(partitions),
        control: Arc::new(control),
        watermarks: Arc::new(watermarks),
    }
}

/// Home and Work, and Work's agent the coordinator.
struct World {
    clock: ManualClock,
    partitions: Partitions,
    facts: Arc<InMemoryFactStore>,
    stores: Stores,
    dirs: Vec<tempfile::TempDir>,
}

impl World {
    async fn new(stores: Stores, clock: ManualClock) -> Self {
        let partitions = Partitions::new(Arc::clone(&stores.partitions));
        for (id, mode) in [("home", PartitionMode::Ask), ("work", PartitionMode::Auto)] {
            let partition = Partition::new(id, format!("{id}-agents"))
                .with_mode(mode)
                .with_cross_partition(CrossPartition::CoordinatorWithApproval);
            partitions.put(partition, AuditActor::default()).await.unwrap();
        }
        let facts = Arc::new(InMemoryFactStore::new());
        let scope = FactScope::new(APP, "alice", Some("home".into()));
        let provenance = Provenance::new(FactSource::UserStated, "seed", "seed");
        facts
            .put(MemoryFact::new(
                scope,
                FactKind::ProjectFact,
                CALENDAR,
                FactStatus::Known,
                provenance,
            ))
            .await
            .unwrap();
        Self { clock, partitions, facts, stores, dirs: Vec::new() }
    }

    /// Area `id`'s harness in `mode`, over `script`.
    async fn harness(
        &mut self,
        id: &str,
        mode: HarnessMode,
        coordinator: bool,
        script: Vec<ScriptedResponse>,
    ) -> (Harness, Arc<ScriptedLlm>, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().to_path_buf();
        let model = Arc::new(ScriptedLlm::new(id, script));
        let partition = self.partitions.get(&PartitionId::new(id)).await.unwrap().unwrap();
        let mut builder = HarnessAgent::builder("harness")
            .model(model.clone())
            .workspace(adk_devtools::Workspace::new(dir.path()))
            .project_instructions(no_user_file())
            .without_compaction()
            .without_default_members()
            .mode(mode)
            .approval_store(Arc::clone(&self.stores.approvals))
            .long_term_memory(LongTermMemoryConfig {
                distill: false,
                ..LongTermMemoryConfig::default()
            })
            .fact_store(self.facts.clone())
            .partition(partition, self.partitions.clone());
        if coordinator {
            builder = builder.partition_coordinator();
        }
        let harness = builder
            .build()
            .unwrap()
            .into_harness(APP, Arc::clone(&self.stores.sessions))
            .unwrap()
            .with_run_store(Arc::clone(&self.stores.runs));
        self.dirs.push(dir);
        (harness, model, workspace)
    }
}

async fn eventually<T, F, Fut>(what: &str, mut probe: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let started = tokio::time::Instant::now();
    while started.elapsed() <= Duration::from_secs(20) {
        if let Some(value) = probe().await {
            return value;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {what}");
}

async fn in_state(runs: &Arc<dyn DurableRunStore>, pid: &str, state: RunState) {
    eventually(&format!("{pid} to be {state:?}"), || async {
        runs.get(pid).await.ok().flatten().filter(|run| run.state == state).map(|_| ())
    })
    .await;
}

fn turn(target: &str, session: &str, message: &str) -> NewRun {
    NewRun::new(QueueClass::INTERACTIVE, APP, "alice", target, json!({ "text": message }))
        .with_session_id(session)
}

fn errand(target: &str, message: &str) -> NewRun {
    NewRun::new(QueueClass::BACKGROUND, APP, "alice", target, json!({ "text": message }))
}

/// The completed runs of `target`.
async fn completed(kernel: &Kernel, target: &str) -> usize {
    let all = kernel.processes().ps(adk_kernel::ProcessFilter::default()).await.unwrap();
    all.iter()
        .filter(|process| process.run.target == target && process.run.state == RunState::Completed)
        .count()
}

/// Moves the clock to `to` and gives the kernel a few scheduler ticks to act on it.
async fn move_to(world: &World, to: DateTime<Utc>) {
    world.clock.set(to);
    tokio::time::sleep(Duration::from_millis(400)).await;
}

/// The admission label `pid` carries once admission held it (spec 3.2 §7.9).
async fn held_for(kernel: &Kernel, pid: &str) -> String {
    eventually(&format!("{pid} to be held"), || async {
        let process = kernel.processes().get(pid).await.ok().flatten()?;
        process.meta.labels.get("kernel.admission").cloned()
    })
    .await
}

async fn scenario(stores: Stores, clock: ManualClock) {
    let mut world = World::new(stores, clock).await;
    let alice = user("alice");
    let (home, home_model, _) = world
        .harness(
            "home",
            HarnessMode::Ask,
            false,
            vec![write("shopping.md", "milk"), text("Saved the shopping list.")],
        )
        .await;
    let (work, work_model, work_dir) = world
        .harness(
            "work",
            HarnessMode::Auto,
            true,
            vec![
                priced(ScriptedResponse::function_call(
                    PARTITION_MEMORY_SEARCH,
                    json!({ "partition": "home", "query": "calendar" }),
                )),
                text("I can't see Home's calendar."),
                write("standup-friday.md", "shipped the release"),
                text("Posted Friday's standup."),
                write("standup-monday.md", "planning"),
                text("Posted Monday's standup."),
            ],
        )
        .await;
    let control = ControlPlaneConfig::default()
        .without_socket()
        .with_http(SocketAddr::from(([127, 0, 0, 1], 0)))
        .with_control_store(Arc::clone(&world.stores.control))
        .with_start_mode("home", PartitionMode::Ask)
        .with_start_mode("work", PartitionMode::Auto);
    let kernel = home
        .host_partition(work.kernel())
        .config(
            KernelConfig::default()
                .with_app_name(APP)
                .with_clock(Arc::new(world.clock.clone()))
                .with_worker(
                    DurableWorkerConfig::default()
                        .with_worker_id("k1")
                        .with_poll_interval(Duration::from_millis(20)),
                )
                .with_scheduler_tick(Duration::from_millis(50))
                .with_control(control),
        )
        .process_store(Arc::clone(&world.stores.processes))
        .watermark_store(Arc::clone(&world.stores.watermarks))
        .build()
        .unwrap();
    kernel.start().await.unwrap();
    let plane = ControlPlane::start(&kernel).await.unwrap();
    let person = ControlClient::http(format!("http://{}", plane.http_addr().unwrap()));
    let home_target = adk_harness::partition_target(APP, &PartitionId::new("home"));
    let work_target = adk_harness::partition_target(APP, &PartitionId::new("work"));

    // Each area has its own daily limit, set as the home sets it.
    person.set_spending_limit("home", Some(2_000)).await.unwrap();
    person.set_spending_limit("work", Some(1_000_000)).await.unwrap();
    let modes: BTreeMap<String, Option<PartitionMode>> = person
        .settings()
        .await
        .unwrap()
        .into_iter()
        .map(|area| (area.partition.to_string(), area.mode))
        .collect();
    assert_eq!(
        modes,
        BTreeMap::from([
            ("home".to_string(), Some(PartitionMode::Ask)),
            ("work".to_string(), Some(PartitionMode::Auto)),
        ])
    );

    // Home asks first: saving a file waits for a person.
    let saving = kernel
        .processes()
        .spawn(
            turn(&home_target, "h1", "save the shopping list"),
            ProcessSpec::new(ProcessKind::Interactive),
        )
        .await
        .unwrap()
        .meta
        .pid;
    in_state(&world.stores.runs, &saving, RunState::Waiting).await;
    let h1 = SessionId::new("h1").unwrap();
    let pending = home.pending_approvals(&alice, &h1).await.unwrap();
    assert_eq!(pending.len(), 1, "Home asks before it writes");
    assert_eq!(pending[0].request.tool_name, "write_file");
    home.decide(&alice, &h1, &pending[0].fingerprint, HarnessDecision::Approve).await.unwrap();
    in_state(&world.stores.runs, &saving, RunState::Completed).await;

    // Work goes ahead, yet its read of Home's calendar waits for a person.
    let reading = kernel
        .processes()
        .spawn(
            turn(&work_target, "w1", "what's on my calendar?"),
            ProcessSpec::new(ProcessKind::Interactive),
        )
        .await
        .unwrap()
        .meta
        .pid;
    in_state(&world.stores.runs, &reading, RunState::Waiting).await;
    let w1 = SessionId::new("w1").unwrap();
    let pending = work.pending_approvals(&alice, &w1).await.unwrap();
    assert_eq!(pending.len(), 1, "the crossing waits even in Go ahead");
    assert_eq!(pending[0].request.tool_name, PARTITION_MEMORY_SEARCH);
    assert_eq!(pending[0].request.args["partition"], "home");
    work.decide(&alice, &w1, &pending[0].fingerprint, HarnessDecision::Deny { message: None })
        .await
        .unwrap();
    in_state(&world.stores.runs, &reading, RunState::Completed).await;
    assert!(
        work_model.requests().iter().all(|request| !format!("{request:?}").contains("dentist")),
        "nothing of Home's calendar reached Work"
    );
    let crossings: Vec<(String, CrossingOutcome)> = world
        .partitions
        .crossings(&CrossingFilter::default().from("work"))
        .await
        .unwrap()
        .into_iter()
        .map(|crossing| (crossing.to.to_string(), crossing.outcome))
        .collect();
    // Held, and never approved: the person's answer is the inbox's record.
    assert_eq!(
        crossings,
        [("home".to_string(), CrossingOutcome::Held)],
        "the crossing is on record"
    );

    // Work's weekday routine, and a Home errand at the same time.
    let routine = WakeSet::any_of([Wake::Cron {
        expr: ROUTINE.into(),
        missed: adk_core::MissedFirePolicy::Skip,
    }]);
    kernel
        .processes()
        .spawn(
            errand(&work_target, "post the standup"),
            ProcessSpec::new(ProcessKind::Scheduled).with_wake(routine),
        )
        .await
        .unwrap();
    let home_errand = kernel
        .processes()
        .spawn(
            errand(&home_target, "order the groceries"),
            ProcessSpec::new(ProcessKind::Scheduled)
                .with_wake(WakeSet::any_of([Wake::At(at(23, 9, 0))])),
        )
        .await
        .unwrap()
        .meta
        .pid;

    // Friday 09:00: the routine runs without asking; Home's limit holds Home's errand.
    // The kernel starts watching the routine's expression before 09:00.
    move_to(&world, at(23, 8, 55)).await;
    move_to(&world, at(23, 9, 0) + chrono::Duration::seconds(30)).await;
    eventually("Friday's routine to finish", || async {
        (completed(&kernel, &work_target).await == 2).then_some(())
    })
    .await;
    assert!(work_dir.join("standup-friday.md").exists(), "Work went ahead");
    assert_eq!(held_for(&kernel, &home_errand).await, "budget", "Home's own limit holds it");
    assert_eq!(home_model.requests().len(), 2, "the held errand made no model call");

    // Spending per agent sums to the day's total.
    let processes =
        person.processes(&adk_kernel::control::api::ProcessesQuery::default()).await.unwrap();
    let mut per_agent: BTreeMap<String, u64> = BTreeMap::new();
    for process in &processes {
        *per_agent.entry(process.run.target.clone()).or_default() += process.view.spend_today;
    }
    per_agent.retain(|_, spent| *spent > 0);
    assert_eq!(
        per_agent,
        BTreeMap::from([(home_target.clone(), 2 * CALL), (work_target.clone(), 4 * CALL)]),
        "every model call is counted once, to its agent"
    );
    let areas: BTreeMap<String, u64> = person
        .settings()
        .await
        .unwrap()
        .into_iter()
        .map(|area| (area.partition.to_string(), area.spent_today))
        .collect();
    assert_eq!(
        areas,
        BTreeMap::from([("home".to_string(), 2 * CALL), ("work".to_string(), 4 * CALL)])
    );
    assert_eq!(per_agent.values().sum::<u64>(), areas.values().sum::<u64>());
    assert_eq!(areas.values().sum::<u64>(), 6 * CALL, "the day's total");
    kernel.processes().kill(&home_errand, "the scenario is done with it").await.unwrap();

    // The weekend: the routine does not run.
    for saturday_or_sunday in [24, 25] {
        move_to(&world, at(saturday_or_sunday, 9, 0) + chrono::Duration::seconds(30)).await;
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(
            completed(&kernel, &work_target).await,
            2,
            "nothing runs on day {saturday_or_sunday}"
        );
    }

    // Monday 09:00: it runs again.
    move_to(&world, at(26, 9, 0) + chrono::Duration::seconds(30)).await;
    eventually("Monday's routine to finish", || async {
        (completed(&kernel, &work_target).await == 3).then_some(())
    })
    .await;
    assert!(work_dir.join("standup-monday.md").exists());

    // Work's own limit, once below what it spent today, holds Work's next errand.
    person.set_spending_limit("work", Some(CALL)).await.unwrap();
    let work_errand = kernel
        .processes()
        .spawn(
            errand(&work_target, "file the expenses"),
            ProcessSpec::new(ProcessKind::Scheduled)
                .with_wake(WakeSet::any_of([Wake::At(at(26, 10, 0))])),
        )
        .await
        .unwrap()
        .meta
        .pid;
    move_to(&world, at(26, 10, 0) + chrono::Duration::seconds(30)).await;
    assert_eq!(held_for(&kernel, &work_errand).await, "budget", "Work's own limit holds it");
    assert_eq!(work_model.requests().len(), 6, "the held errand made no model call");
    kernel.processes().kill(&work_errand, "the scenario is done with it").await.unwrap();

    plane.shutdown().await.unwrap();
    kernel.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn work_stays_at_work_on_sqlite() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::new(at(23, 8, 50));
    let stores = sqlite(&dir.path().join("harness.db"), &clock).await;
    scenario(stores, clock).await;
}

#[cfg(feature = "postgres")]
mod postgres {
    use super::*;

    fn database_url() -> String {
        std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://postgres@localhost:5432/postgres".to_string())
    }

    /// Every store in a scratch schema of one PostgreSQL database.
    async fn stores(clock: &ManualClock) -> Stores {
        use sqlx::Connection as _;
        use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
        use std::str::FromStr;
        let schema = format!("adk_work_stays_{}", uuid_like());
        let mut admin = sqlx::PgConnection::connect(&database_url()).await.unwrap();
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
            .execute(&mut admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&database_url())
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new().max_connections(8).connect_with(options).await.unwrap();
        let runs = adk_session::PostgresRunStore::from_pool(pool.clone())
            .with_clock(Arc::new(clock.clone()));
        runs.migrate().await.unwrap();
        let sessions = adk_session::PostgresSessionService::from_pool(pool.clone());
        sessions.migrate().await.unwrap();
        let approvals = adk_session::PostgresApprovalStore::from_pool(pool.clone());
        approvals.migrate().await.unwrap();
        let processes = adk_kernel::PostgresProcessStore::new(pool.clone());
        processes.migrate().await.unwrap();
        let partitions = adk_kernel::PostgresPartitionStore::new(pool.clone());
        partitions.migrate().await.unwrap();
        let control = adk_kernel::control::PostgresControlStore::new(pool.clone());
        control.migrate().await.unwrap();
        let watermarks = adk_kernel::PostgresCronWatermarkStore::new(pool);
        watermarks.migrate().await.unwrap();
        Stores {
            runs: Arc::new(runs),
            processes: Arc::new(processes),
            sessions: Arc::new(sessions),
            approvals: Arc::new(approvals),
            partitions: Arc::new(partitions),
            control: Arc::new(control),
            watermarks: Arc::new(watermarks),
        }
    }

    /// A schema-name suffix unique to this run.
    fn uuid_like() -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        format!("{nanos:x}{:x}", std::process::id())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "needs DATABASE_URL; CI's postgres job runs it"]
    async fn work_stays_at_work_on_postgres() {
        let clock = ManualClock::new(at(23, 8, 50));
        let stores = stores(&clock).await;
        scenario(stores, clock).await;
    }
}
