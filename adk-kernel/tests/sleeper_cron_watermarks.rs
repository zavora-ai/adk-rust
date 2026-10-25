//! Cron sleepers across kernel restarts and replicas (spec 3.2 §7.4): a sleeper's cron wake
//! keeps its cursor in the kernel's `CronWatermarkStore`, so the occurrences that came due
//! while no kernel ran fire once each, as each sleeper's `missed` policy picks them, and none
//! that already fired fires again. On `origin/main` the cursor lives in the kernel's memory,
//! so a restarted kernel starts at its first look and nothing missed while it was down fires.
//!
//! Every scenario runs real kernels over one SQLite file on a `ManualClock`: phase one fires
//! each series' 09:0x occurrence live, the kernel stops, the clock moves to 12:02:30, and the
//! kernel that starts next — a restart, or two replicas at once — fires the missed ones.

#![cfg(feature = "sqlite")]

use std::collections::BTreeMap;
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use adk_agent::LlmAgentBuilder;
use adk_core::schedule::MissedFirePolicy;
use adk_core::{Clock, DurableRunStore, Llm, ManualClock, NewRun, RunFilter, RunRecord, RunState};
use adk_kernel::control::ControlPlaneConfig;
use adk_kernel::{
    CronWatermarkStore, Kernel, KernelConfig, ProcessKind, ProcessSpec, ProcessStore, QueueClass,
    SqliteCronWatermarkStore, SqliteProcessStore, Wake, WakeSet,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_server::background::DurableWorkerConfig;
use adk_session::{SessionService, SqliteRunStore, SqliteSessionService};
use chrono::{DateTime, TimeZone, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

const APP: &str = "adk-rust";
const TARGET: &str = "adk-rust";

/// Every hour at :00, fired once however late.
const ONCE: &str = "0 0 * * * *";
/// Every hour at :01, the latest two missed.
const TWO: &str = "0 1 * * * *";
/// Every hour at :02, skipped unless on time.
const SKIP: &str = "0 2 * * * *";
/// Every hour at :05, skipped unless on time.
const LATE: &str = "0 5 * * * *";

fn at(hour: u32, minute: u32, second: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 26, hour, minute, second).unwrap()
}

/// One kernel replica's handles on the database file.
struct Stores {
    runs: Arc<dyn DurableRunStore>,
    processes: Arc<dyn ProcessStore>,
    sessions: Arc<SqliteSessionService>,
    watermarks: Arc<dyn CronWatermarkStore>,
}

/// Opens a new pool over `path`, as a kernel process opens its store.
async fn open(path: &Path, clock: Arc<dyn Clock>) -> Stores {
    let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
        .expect("valid URL")
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_secs(30));
    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .connect_with(options)
        .await
        .expect("SQLite opens");
    let runs = SqliteRunStore::from_pool(pool.clone()).with_clock(clock);
    runs.migrate().await.expect("run schema migrates");
    let processes = SqliteProcessStore::new(pool.clone());
    processes.migrate().await.expect("process schema migrates");
    let sessions = SqliteSessionService::from_pool(pool.clone());
    sessions.migrate().await.expect("session schema migrates");
    let watermarks = SqliteCronWatermarkStore::new(pool);
    watermarks.migrate().await.expect("watermark schema migrates");
    Stores {
        runs: Arc::new(runs),
        processes: Arc::new(processes),
        sessions: Arc::new(sessions),
        watermarks: Arc::new(watermarks),
    }
}

/// A kernel replica named `name` over `stores`, on `clock`, whose agent answers from
/// `model`.
fn kernel(name: &str, stores: &Stores, clock: &ManualClock, model: &Arc<ScriptedLlm>) -> Kernel {
    let agent = LlmAgentBuilder::new("agent")
        .model(Arc::clone(model) as Arc<dyn Llm>)
        .build()
        .expect("the agent builds");
    let runner = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(agent))
        .session_service(Arc::clone(&stores.sessions) as Arc<dyn SessionService>)
        .build()
        .expect("the runner builds");
    let config = KernelConfig::default()
        .with_app_name(APP)
        .with_worker(
            DurableWorkerConfig::default()
                .with_worker_id(name)
                .with_poll_interval(Duration::from_millis(20)),
        )
        .with_scheduler_tick(Duration::from_millis(50))
        .with_clock(Arc::new(clock.clone()))
        .with_control(ControlPlaneConfig::default().without_socket());
    Kernel::builder()
        .config(config)
        .run_store(Arc::clone(&stores.runs))
        .process_store(Arc::clone(&stores.processes))
        .session_service(Arc::clone(&stores.sessions) as Arc<dyn SessionService>)
        .watermark_store(Arc::clone(&stores.watermarks))
        .agent(TARGET, Arc::new(runner))
        .build()
        .expect("the kernel builds")
}

fn model() -> Arc<ScriptedLlm> {
    Arc::new(ScriptedLlm::new("scripted", (0..40).map(|_| ScriptedResponse::text("Done."))))
}

/// Puts a cron series named `name` to sleep on `expr` with `missed`.
async fn sleep_on(kernel: &Kernel, name: &str, expr: &str, missed: MissedFirePolicy) -> String {
    let run = NewRun::new(
        QueueClass::BACKGROUND,
        APP,
        "alice",
        TARGET,
        serde_json::json!({ "text": name }),
    );
    let wake = WakeSet::any_of([Wake::Cron { expr: expr.into(), missed }]);
    kernel
        .processes()
        .spawn(run, ProcessSpec::new(ProcessKind::Scheduled).with_wake(wake))
        .await
        .expect("the sleeper spawns")
        .meta
        .pid
}

/// Polls `probe` every 20 ms until it answers, for at most twenty seconds.
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

/// Every run on the kernel's queues.
async fn all_runs(runs: &Arc<dyn DurableRunStore>) -> Vec<RunRecord> {
    let mut all = Vec::new();
    for queue in QueueClass::defaults() {
        all.extend(runs.list(RunFilter::default().queue(queue.as_str())).await.unwrap());
    }
    all
}

/// Where the cron series stand: by series name, how many of its processes completed, and
/// whether the latest one sleeps with nothing else queued or running.
async fn series(runs: &Arc<dyn DurableRunStore>) -> BTreeMap<String, (usize, bool)> {
    let mut series: BTreeMap<String, (usize, bool)> = BTreeMap::new();
    let mut busy = false;
    for run in all_runs(runs).await {
        if run.queue == QueueClass::MAINTENANCE {
            busy |= !run.state.is_terminal();
            continue;
        }
        let name = run.input["text"].as_str().unwrap_or_default().to_string();
        let entry = series.entry(name).or_default();
        match run.state {
            RunState::Completed => entry.0 += 1,
            RunState::Waiting => entry.1 = true,
            _ => busy = true,
        }
    }
    if busy {
        for entry in series.values_mut() {
            entry.1 = false;
        }
    }
    series
}

/// The occurrences each expression's `kernel.wake` runs fired, oldest first.
async fn fired(runs: &Arc<dyn DurableRunStore>) -> BTreeMap<String, Vec<DateTime<Utc>>> {
    let mut fired: BTreeMap<String, Vec<DateTime<Utc>>> = BTreeMap::new();
    for run in runs.list(RunFilter::default().queue(QueueClass::MAINTENANCE)).await.unwrap() {
        // `cron:kernel:<expression>:<RFC 3339 time>`: the time holds colons, the expression
        // none.
        let Some((expr, tick)) = run
            .idempotency_key
            .as_deref()
            .and_then(|key| key.strip_prefix("cron:kernel:"))
            .and_then(|rest| rest.split_once(':'))
        else {
            continue;
        };
        let tick = DateTime::parse_from_rfc3339(tick).unwrap().with_timezone(&Utc);
        fired.entry(expr.to_string()).or_default().push(tick);
    }
    for ticks in fired.values_mut() {
        ticks.sort();
    }
    fired
}

/// Waits until every series reached `completed` runs and sleeps again, with nothing else
/// queued or running.
async fn settled(runs: &Arc<dyn DurableRunStore>, completed: &[(&str, usize)]) {
    let wanted: BTreeMap<String, (usize, bool)> =
        completed.iter().map(|(name, count)| (name.to_string(), (*count, true))).collect();
    let last = std::sync::Mutex::new(BTreeMap::new());
    let reached = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let now = series(runs).await;
            if now == wanted {
                return;
            }
            *last.lock().unwrap() = now;
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    if reached.is_err() {
        let detail: Vec<String> = all_runs(runs)
            .await
            .iter()
            .map(|run| {
                format!(
                    "{} {} {:?} {:?} {:?} {:?} created={} updated={}",
                    run.run_id,
                    run.queue,
                    run.state,
                    run.idempotency_key,
                    run.input.get("text"),
                    run.result,
                    run.created_at,
                    run.updated_at
                )
            })
            .collect();
        panic!(
            "the series did not settle at {wanted:?}: {:?}\n{}",
            last.lock().unwrap(),
            detail.join("\n")
        );
    }
}

/// Phase one: a kernel puts four series to sleep at 08:50, fires each one's 09:0x
/// occurrence live at 09:06, and stops with each series asleep again.
async fn first_kernel(path: &Path, clock: &ManualClock, model: &Arc<ScriptedLlm>) {
    let stores = open(path, Arc::new(clock.clone())).await;
    let first = kernel("replica-1", &stores, clock, model);
    first.start().await.unwrap();
    for (name, expr, missed) in [
        ("once", ONCE, MissedFirePolicy::FireOnce),
        ("two", TWO, MissedFirePolicy::CatchUp(2)),
        ("skip", SKIP, MissedFirePolicy::Skip),
        ("late", LATE, MissedFirePolicy::Skip),
    ] {
        sleep_on(&first, name, expr, missed).await;
    }
    settled(&stores.runs, &[("once", 0), ("two", 0), ("skip", 0), ("late", 0)]).await;
    // Each expression is watched, its cursor at the first look.
    for expr in [ONCE, TWO, SKIP, LATE] {
        let source = format!("kernel:{expr}");
        let watermarks = Arc::clone(&stores.watermarks);
        eventually("the first look at each expression", || {
            let (watermarks, source) = (Arc::clone(&watermarks), source.clone());
            async move { watermarks.read(&source).await.unwrap() }
        })
        .await;
    }
    clock.set(at(9, 6, 0));
    settled(&stores.runs, &[("once", 1), ("two", 1), ("skip", 1), ("late", 1)]).await;
    first.stop().await.unwrap();
}

fn phase_one() -> BTreeMap<String, Vec<DateTime<Utc>>> {
    BTreeMap::from([
        (ONCE.to_string(), vec![at(9, 0, 0)]),
        (TWO.to_string(), vec![at(9, 1, 0)]),
        (SKIP.to_string(), vec![at(9, 2, 0)]),
        (LATE.to_string(), vec![at(9, 5, 0)]),
    ])
}

/// What the kernel after the outage fires: `fireOnce` the latest, `catchUp(2)` the latest
/// two, `skip` the latest because it is 30 s late, and nothing for the `skip` series whose
/// latest is 57 minutes late.
fn after_outage() -> BTreeMap<String, Vec<DateTime<Utc>>> {
    BTreeMap::from([
        (ONCE.to_string(), vec![at(9, 0, 0), at(12, 0, 0)]),
        (TWO.to_string(), vec![at(9, 1, 0), at(11, 1, 0), at(12, 1, 0)]),
        (SKIP.to_string(), vec![at(9, 2, 0), at(12, 2, 0)]),
        (LATE.to_string(), vec![at(9, 5, 0)]),
    ])
}

const AFTER_OUTAGE: [(&str, usize); 4] = [("once", 2), ("two", 3), ("skip", 2), ("late", 1)];

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restarted_kernel_fires_the_missed_occurrences_each_policy_picks_once() {
    let db = tempfile::tempdir().unwrap();
    let path = db.path().join("kernel.db");
    let clock = ManualClock::new(at(8, 50, 0));
    let model = model();
    first_kernel(&path, &clock, &model).await;
    let stores = open(&path, Arc::new(clock.clone())).await;
    assert_eq!(fired(&stores.runs).await, phase_one());

    // Down from 09:06 to 12:02:30: three occurrences of each expression came due.
    clock.set(at(12, 2, 30));
    let restarted = kernel("replica-1", &stores, &clock, &model);
    restarted.start().await.unwrap();
    settled(&stores.runs, &AFTER_OUTAGE).await;
    // Nothing more goes out on the stopped clock.
    tokio::time::sleep(Duration::from_millis(500)).await;
    restarted.stop().await.unwrap();
    assert_eq!(fired(&stores.runs).await, after_outage(), "each picked occurrence, once");
    assert_eq!(series(&stores.runs).await.values().map(|(done, _)| done).sum::<usize>(), 8);
    assert_eq!(
        stores.watermarks.read(&format!("kernel:{TWO}")).await.unwrap(),
        Some(at(12, 2, 30)),
        "the cursor is past the outage"
    );

    // A second restart replays nothing.
    let again = kernel("replica-1", &stores, &clock, &model);
    again.start().await.unwrap();
    tokio::time::sleep(Duration::from_millis(800)).await;
    again.stop().await.unwrap();
    assert_eq!(fired(&stores.runs).await, after_outage(), "a restart replays nothing again");
    settled(&stores.runs, &AFTER_OUTAGE).await;
    assert_eq!(model.requests().len(), 8, "one model call per fired occurrence");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_replicas_taking_over_fire_each_missed_occurrence_once() {
    let db = tempfile::tempdir().unwrap();
    let path = db.path().join("kernel.db");
    let clock = ManualClock::new(at(8, 50, 0));
    let model = model();
    first_kernel(&path, &clock, &model).await;

    // Two replicas take over together after the outage, each over its own pool.
    clock.set(at(12, 2, 30));
    let (a, b) =
        (open(&path, Arc::new(clock.clone())).await, open(&path, Arc::new(clock.clone())).await);
    let replicas =
        [kernel("replica-a", &a, &clock, &model), kernel("replica-b", &b, &clock, &model)];
    let (started_a, started_b) = tokio::join!(replicas[0].start(), replicas[1].start());
    started_a.unwrap();
    started_b.unwrap();
    settled(&a.runs, &AFTER_OUTAGE).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let (_, _) = tokio::join!(replicas[0].stop(), replicas[1].stop());
    assert_eq!(fired(&a.runs).await, after_outage(), "each picked occurrence, once");
    settled(&a.runs, &AFTER_OUTAGE).await;
    assert_eq!(model.requests().len(), 8, "no occurrence ran twice");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_series_replays_nothing_from_before_it_began() {
    let db = tempfile::tempdir().unwrap();
    let path = db.path().join("kernel.db");
    let clock = ManualClock::new(at(8, 50, 0));
    let model = model();
    let stores = open(&path, Arc::new(clock.clone())).await;
    let first = kernel("replica-1", &stores, &clock, &model);
    first.start().await.unwrap();
    sleep_on(&first, "old", ONCE, MissedFirePolicy::FireOnce).await;
    settled(&stores.runs, &[("old", 0)]).await;
    let source = format!("kernel:{ONCE}");
    eventually("the first look", || async { stores.watermarks.read(&source).await.unwrap() }).await;
    clock.set(at(9, 6, 0));
    settled(&stores.runs, &[("old", 1)]).await;
    first.stop().await.unwrap();

    // After the outage a new series sleeps on the same expression beside the old one's
    // successor. The missed 12:00 occurrence is the old series' to replay; the new series
    // began at 12:02:30 and sleeps until 13:00.
    clock.set(at(12, 2, 30));
    let restarted = kernel("replica-1", &stores, &clock, &model);
    sleep_on(&restarted, "new", ONCE, MissedFirePolicy::FireOnce).await;
    restarted.start().await.unwrap();
    settled(&stores.runs, &[("old", 2), ("new", 0)]).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        series(&stores.runs).await,
        BTreeMap::from([("old".into(), (2, true)), ("new".into(), (0, true))])
    );

    clock.set(at(13, 0, 30));
    settled(&stores.runs, &[("old", 3), ("new", 1)]).await;
    restarted.stop().await.unwrap();
    assert_eq!(
        fired(&stores.runs).await,
        BTreeMap::from([(ONCE.to_string(), vec![at(9, 0, 0), at(12, 0, 0), at(13, 0, 0)])])
    );
}
