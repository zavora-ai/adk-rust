//! One process of a durability gate: a payment worker, a scheduler, or a kill-switch
//! replica. The gates in `tests/gates.rs` start it; it shares nothing with them but the
//! backend named by `ADK_GATE_BACKEND` and `ADK_GATE_LOCATION`, and files in a temporary
//! directory that carry test signals and observations.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use adk_agent::LlmAgentBuilder;
use adk_core::governance::GovernanceMirrorConfig;
use adk_core::governance_state::GovernanceScope;
use adk_core::{
    DurableRunStore, GOVERNANCE_FROZEN_CODE, GOVERNANCE_STALE_CODE, GovernanceControl, Llm,
};
use adk_durability_gates::{
    AfterCharge, Backend, ChargeCard, HeartbeatStall, LoopModel, PartitionedGovernance,
    PaymentModel, RecordingRunStore, Tick, append_line, now_ms,
};
use adk_runner::Runner;
use adk_server::background::{DurableScheduler, DurableWorker, DurableWorkerConfig};

fn var(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is set"))
}

fn millis(name: &str) -> Duration {
    Duration::from_millis(var(name).parse().unwrap_or_else(|_| panic!("{name} is milliseconds")))
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(tracing::Level::WARN)
        .init();
    match var("ADK_GATE_ROLE").as_str() {
        "payment-worker" => payment_worker().await,
        "scheduler" => scheduler().await,
        "replica" => replica().await,
        other => panic!("unknown role {other}"),
    }
}

/// Runs the payment agent until the run named by `ADK_GATE_RUN` is terminal.
async fn payment_worker() {
    let stores = Backend::from_process_env().open().await;
    let mut recording =
        RecordingRunStore::new(Arc::clone(&stores.runs), var("ADK_GATE_WRITES").into());
    if let (Ok(reached), Ok(resume)) =
        (std::env::var("ADK_GATE_STALL_REACHED"), std::env::var("ADK_GATE_STALL_RESUME"))
    {
        recording =
            recording.with_heartbeat_stall(HeartbeatStall::new(reached.into(), resume.into()));
    }
    let runs: Arc<dyn DurableRunStore> = Arc::new(recording);
    let after = match var("ADK_GATE_AFTER_CHARGE").as_str() {
        "return" => AfterCharge::Return,
        "hang" => AfterCharge::Hang,
        other => AfterCharge::WaitFor(
            other.strip_prefix("wait:").expect("return, hang, or wait:<path>").into(),
        ),
    };
    let model = PaymentModel { hold: std::env::var("ADK_GATE_HOLD").ok().map(PathBuf::from) };
    let agent = LlmAgentBuilder::new("billing")
        .model(Arc::new(model) as Arc<dyn Llm>)
        .tool(Arc::new(ChargeCard { log: var("ADK_GATE_CHARGES").into(), after }))
        .build()
        .expect("agent builds");
    let runner = Runner::builder()
        .app_name(adk_durability_gates::APP)
        .agent(Arc::new(agent))
        .session_service(Arc::clone(&stores.sessions))
        .action_ledger(Arc::clone(&stores.ledger))
        .durable_run_store(Arc::clone(&runs))
        .build()
        .expect("runner builds");
    let worker = DurableWorker::new(Arc::clone(&runs))
        .with_agent("billing", Arc::new(runner))
        .with_config(
            DurableWorkerConfig::default()
                .with_worker_id(var("ADK_GATE_WORKER"))
                .with_queue(var("ADK_GATE_QUEUE"))
                .with_lease_ttl(millis("ADK_GATE_LEASE_MS"))
                .with_poll_interval(Duration::from_millis(100)),
        )
        .spawn();
    let run_id = var("ADK_GATE_RUN");
    loop {
        let run = stores.runs.get(&run_id).await.expect("read run").expect("run exists");
        if run.state.is_terminal() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    worker.shutdown().await;
}

/// Ticks a scheduler over every due schedule until `ADK_GATE_STOP` exists, then writes
/// what it did to `ADK_GATE_REPORT`.
async fn scheduler() {
    let stores = Backend::from_process_env().open().await;
    let stop = PathBuf::from(var("ADK_GATE_STOP"));
    let tick = millis("ADK_GATE_TICK_MS");
    let scheduler = DurableScheduler::new(stores.schedules, stores.runs);
    let (mut ticks, mut enqueued, mut existing, mut lost) = (0, 0, 0, 0);
    while !stop.exists() {
        match scheduler.tick().await {
            Ok(done) => {
                ticks += 1;
                enqueued += done.enqueued;
                existing += done.existing;
                lost += done.lost_race;
            }
            Err(error) => tracing::warn!(error = %error, "scheduler tick failed"),
        }
        tokio::time::sleep(tick).await;
    }
    append_line(
        &PathBuf::from(var("ADK_GATE_REPORT")),
        &format!("ticks={ticks} enqueued={enqueued} existing={existing} lost={lost}"),
    );
}

/// One replica of a deployment: a mirrored kill switch, a worker on its own queue, and a
/// scheduler, all governed by the mirror. Runs until killed.
///
/// It reports each change of its kill-switch state (`ok`, `frozen`, `stale`) to
/// `status-{index}.log` and each model and tool call to `calls-{index}.log`.
async fn replica() {
    let stores = Backend::from_process_env().open().await;
    let index = var("ADK_GATE_INDEX");
    let dir = PathBuf::from(var("ADK_GATE_DIR"));
    let governance = Arc::new(PartitionedGovernance::new(
        Arc::clone(&stores.governance),
        dir.join(format!("partition-{index}")),
    ));
    let control = GovernanceControl::mirrored(
        governance,
        GovernanceMirrorConfig::default()
            .with_poll_interval(millis("ADK_GATE_POLL_MS"))
            .with_max_staleness(millis("ADK_GATE_STALENESS_MS"))
            .with_scopes([GovernanceScope::Tenant(var("ADK_GATE_TENANT"))]),
    );
    let _mirror = control.spawn_mirror().expect("mirrored control");

    let calls = dir.join(format!("calls-{index}.log"));
    let agent = LlmAgentBuilder::new("looper")
        .model(Arc::new(LoopModel { calls: calls.clone() }) as Arc<dyn Llm>)
        .tool(Arc::new(Tick { calls }))
        .max_iterations(u32::MAX)
        .build()
        .expect("agent builds");
    let runner = Runner::builder()
        .app_name(adk_durability_gates::APP)
        .agent(Arc::new(agent))
        .session_service(Arc::clone(&stores.sessions))
        .action_ledger(Arc::clone(&stores.ledger))
        .durable_run_store(Arc::clone(&stores.runs))
        .governance(control.clone())
        .build()
        .expect("runner builds");
    let _worker = DurableWorker::new(Arc::clone(&stores.runs))
        .with_agent("looper", Arc::new(runner))
        .with_governance(control.clone())
        .with_config(
            DurableWorkerConfig::default()
                .with_worker_id(format!("replica-{index}"))
                .with_queue(format!("{}/{index}", var("ADK_GATE_QUEUE")))
                .with_lease_ttl(Duration::from_secs(2))
                .with_poll_interval(Duration::from_millis(50)),
        )
        .spawn();
    let _scheduler = DurableScheduler::new(stores.schedules, stores.runs)
        .with_governance(control.clone())
        .with_tick_interval(Duration::from_millis(20))
        .spawn();

    let status = dir.join(format!("status-{index}.log"));
    let mut last = "";
    loop {
        let state = match control.check() {
            Ok(()) => "ok",
            Err(error) if error.code == GOVERNANCE_FROZEN_CODE => "frozen",
            Err(error) if error.code == GOVERNANCE_STALE_CODE => "stale",
            Err(error) => panic!("unexpected kill switch error: {error}"),
        };
        if state != last {
            append_line(&status, &format!("{state} {}", now_ms()));
            last = state;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}
