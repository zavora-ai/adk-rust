//! Crash gate across OS processes on durable stores.
//!
//! The parent test starts this test binary as a worker process running only
//! [`crash_worker`]. Process A charges a card through a non-idempotent tool and hangs
//! inside the call; the parent kills it with SIGKILL. Process B recovers the run once
//! A's lease expires, answers the stopped call from the durable action ledger, and
//! completes the run. The card is charged once, and every store — runs, sessions, and the
//! action ledger — is shared only through the database.
//!
//! The SQLite gate runs by default. The PostgreSQL gate needs a server at `DATABASE_URL`:
//!
//! ```bash
//! DATABASE_URL=postgres://postgres@localhost:5432/postgres \
//!   cargo nextest run -p adk-server --features background --run-ignored only \
//!   --test durable_multiprocess
//! ```

#![cfg(all(feature = "background", unix))]

use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use adk_agent::LlmAgentBuilder;
use adk_core::{
    ActionLedger, Content, DurableRunStore, Llm, LlmRequest, LlmResponse, LlmResponseStream,
    NewRun, Part, Result, RunCheckpointRef, RunState, Tool, ToolContext, ToolEffect,
};
use adk_runner::Runner;
use adk_server::background::{DurableWorker, DurableWorkerConfig};
use adk_session::{GetRequest, SessionService};
use async_trait::async_trait;
use serde_json::{Value, json};

const BACKEND_ENV: &str = "ADK_CRASH_BACKEND";
const URL_ENV: &str = "ADK_CRASH_URL";
const QUEUE_ENV: &str = "ADK_CRASH_QUEUE";
const RUN_ENV: &str = "ADK_CRASH_RUN";
const WORKER_ENV: &str = "ADK_CRASH_WORKER";
const HANG_ENV: &str = "ADK_CRASH_HANG";
const CHARGES_ENV: &str = "ADK_CRASH_CHARGES";
const APP: &str = "billing";
const USER: &str = "customer-1";
const LEASE: Duration = Duration::from_secs(2);

/// The durable stores one process opens; SQLite gets one file per store.
struct Stores {
    runs: Arc<dyn DurableRunStore>,
    sessions: Arc<dyn SessionService>,
    ledger: Arc<dyn ActionLedger>,
}

async fn open(backend: &str, url: &str) -> Stores {
    match backend {
        "sqlite" => {
            let file = |name: &str| format!("sqlite:{url}/{name}.db?mode=rwc");
            let runs = adk_session::SqliteRunStore::new(&file("runs")).await.expect("run store");
            runs.migrate().await.expect("run schema");
            let sessions =
                adk_session::SqliteSessionService::new(&file("sessions")).await.expect("sessions");
            sessions.migrate().await.expect("session schema");
            let ledger =
                adk_session::SqliteActionLedger::new(&file("ledger")).await.expect("ledger");
            ledger.migrate().await.expect("ledger schema");
            Stores { runs: Arc::new(runs), sessions: Arc::new(sessions), ledger: Arc::new(ledger) }
        }
        "postgres" => {
            let runs = adk_session::PostgresRunStore::new(url).await.expect("run store");
            runs.migrate().await.expect("run schema");
            let sessions = adk_session::PostgresSessionService::new(url).await.expect("sessions");
            sessions.migrate().await.expect("session schema");
            let ledger = adk_session::PostgresActionLedger::new(url).await.expect("ledger");
            ledger.migrate().await.expect("ledger schema");
            Stores { runs: Arc::new(runs), sessions: Arc::new(sessions), ledger: Arc::new(ledger) }
        }
        other => panic!("unknown backend {other}"),
    }
}

/// Charges once; once the charge has a response, reports its status.
struct PaymentModel;

#[async_trait]
impl Llm for PaymentModel {
    fn name(&self) -> &str {
        "payment-script"
    }

    async fn generate_content(&self, req: LlmRequest, _stream: bool) -> Result<LlmResponseStream> {
        let status = req.contents.iter().flat_map(|c| &c.parts).find_map(|part| match part {
            Part::FunctionResponse { function_response, .. }
                if function_response.name == "charge_card" =>
            {
                function_response.response.get("status").and_then(Value::as_str).map(str::to_string)
            }
            _ => None,
        });
        let content = match status {
            Some(status) => Content::new("model").with_text(format!("charge status: {status}")),
            None => Content {
                role: "model".to_string(),
                parts: vec![Part::FunctionCall {
                    name: "charge_card".to_string(),
                    args: json!({ "amountCents": 1999 }),
                    id: Some("call-charge-1".to_string()),
                    thought_signature: None,
                }],
            },
        };
        let response =
            LlmResponse { content: Some(content), turn_complete: true, ..Default::default() };
        Ok(Box::pin(futures::stream::iter([Ok(response)])))
    }
}

/// Appends one line to the charge log per charge, then hangs when told to.
struct ChargeCard {
    log: String,
    hang: bool,
}

#[async_trait]
impl Tool for ChargeCard {
    fn name(&self) -> &str {
        "charge_card"
    }

    fn description(&self) -> &str {
        "charges the customer's card"
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::NonIdempotent
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        let mut log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log)
            .expect("charge log opens");
        writeln!(log, "charged by {}", std::process::id()).expect("charge recorded");
        log.sync_all().expect("charge log synced");
        if self.hang {
            std::future::pending::<()>().await;
        }
        Ok(json!({ "status": "charged" }))
    }
}

/// The worker process body; a no-op unless the parent test set the worker variables.
#[tokio::test]
#[ignore = "worker process for the multi-process crash gates"]
async fn crash_worker() {
    let (Ok(backend), Ok(url), Ok(queue), Ok(run_id), Ok(worker_id), Ok(charges)) = (
        std::env::var(BACKEND_ENV),
        std::env::var(URL_ENV),
        std::env::var(QUEUE_ENV),
        std::env::var(RUN_ENV),
        std::env::var(WORKER_ENV),
        std::env::var(CHARGES_ENV),
    ) else {
        return;
    };
    let hang = std::env::var(HANG_ENV).is_ok_and(|value| value == "1");
    let stores = open(&backend, &url).await;
    let agent = LlmAgentBuilder::new("billing")
        .model(Arc::new(PaymentModel) as Arc<dyn Llm>)
        .tool(Arc::new(ChargeCard { log: charges, hang }))
        .build()
        .expect("agent builds");
    let runner = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(agent))
        .session_service(Arc::clone(&stores.sessions))
        .action_ledger(Arc::clone(&stores.ledger))
        .durable_run_store(Arc::clone(&stores.runs))
        .build()
        .expect("runner builds");
    let worker = DurableWorker::new(Arc::clone(&stores.runs))
        .with_agent("billing", Arc::new(runner))
        .with_config(
            DurableWorkerConfig::default()
                .with_worker_id(worker_id)
                .with_queue(queue)
                .with_lease_ttl(LEASE)
                .with_poll_interval(Duration::from_millis(100)),
        )
        .spawn();
    loop {
        let run = stores.runs.get(&run_id).await.expect("read run").expect("run exists");
        if run.state.is_terminal() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    worker.shutdown().await;
}

fn spawn_worker(
    backend: &str,
    url: &str,
    queue: &str,
    run_id: &str,
    worker: &str,
    hang: bool,
    charges: &str,
) -> Child {
    Command::new(std::env::current_exe().expect("test binary path"))
        .args(["crash_worker", "--exact", "--ignored", "--nocapture", "--test-threads=1"])
        .env(BACKEND_ENV, backend)
        .env(URL_ENV, url)
        .env(QUEUE_ENV, queue)
        .env(RUN_ENV, run_id)
        .env(WORKER_ENV, worker)
        .env(HANG_ENV, if hang { "1" } else { "0" })
        .env(CHARGES_ENV, charges)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("worker process starts")
}

fn charges(log: &str) -> usize {
    std::fs::read_to_string(log).map(|text| text.lines().count()).unwrap_or(0)
}

/// Enqueues a payment run, kills the process that took it mid-charge, and checks that a
/// second process completes it with exactly one charge.
async fn crash_gate(backend: &str, url: &str) {
    let dir = tempfile::tempdir().expect("temp dir");
    let log = dir.path().join("charges.log").display().to_string();
    let stores = open(backend, url).await;
    let queue = format!("crash/{}", uuid::Uuid::new_v4());
    let session_id = format!("order-{}", uuid::Uuid::new_v4());
    let run = stores
        .runs
        .enqueue(
            NewRun::new(&queue, APP, USER, "billing", json!("charge my card"))
                .with_session_id(&session_id),
        )
        .await
        .expect("enqueue")
        .record()
        .cloned()
        .expect("created");

    let mut first = spawn_worker(backend, url, &queue, &run.run_id, "process-a", true, &log);
    let started = Instant::now();
    while charges(&log) == 0 {
        assert!(started.elapsed() < Duration::from_secs(60), "process a never reached the charge");
        assert!(first.try_wait().expect("poll process a").is_none(), "process a exited early");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    first.kill().expect("SIGKILL process a");
    first.wait().expect("process a reaped");
    let stopped = stores.runs.get(&run.run_id).await.expect("read").expect("run");
    assert_eq!(stopped.state, RunState::Running, "the killed process wrote no outcome");
    assert!(matches!(stopped.checkpoint, Some(RunCheckpointRef::Session { .. })));

    let mut second = spawn_worker(backend, url, &queue, &run.run_id, "process-b", false, &log);
    let started = Instant::now();
    let status = loop {
        if let Some(status) = second.try_wait().expect("poll process b") {
            break status;
        }
        if started.elapsed() > Duration::from_secs(90) {
            second.kill().ok();
            panic!("process b did not complete the run");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(status.success(), "process b failed: {status:?}");

    let finished = stores.runs.get(&run.run_id).await.expect("read").expect("run");
    assert_eq!(finished.state, RunState::Completed, "{finished:?}");
    assert_eq!(finished.attempt, 2, "recovered at the next attempt");
    assert_eq!(finished.result.as_ref().unwrap()["text"], "charge status: outcome_unknown");
    assert_eq!(charges(&log), 1, "exactly one charge across both processes");

    let session = stores
        .sessions
        .get(GetRequest {
            app_name: APP.into(),
            user_id: USER.into(),
            session_id,
            num_recent_events: None,
            after: None,
        })
        .await
        .expect("session");
    let responses: Vec<Value> = session
        .events()
        .all()
        .iter()
        .filter_map(|event| event.llm_response.content.clone())
        .flat_map(|content| content.parts)
        .filter_map(|part| match part {
            Part::FunctionResponse { function_response, .. } => Some(function_response.response),
            _ => None,
        })
        .collect();
    assert_eq!(responses.len(), 1, "the stopped call has one response: {responses:?}");
    assert!(adk_core::is_outcome_unknown(&responses[0]));
}

#[tokio::test]
async fn sqlite_worker_process_killed_mid_charge_is_completed_by_another_process() {
    let dir = tempfile::tempdir().expect("temp dir");
    crash_gate("sqlite", &dir.path().display().to_string()).await;
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server at DATABASE_URL"]
async fn postgres_worker_process_killed_mid_charge_is_completed_by_another_process() {
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres@localhost:5432/postgres".to_string());
    crash_gate("postgres", &url).await;
}
