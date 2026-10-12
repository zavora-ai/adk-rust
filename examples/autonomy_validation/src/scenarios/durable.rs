//! `durable_crash_resume`: a durable worker killed mid-payment is recovered by a second
//! worker after its lease expires; the run completes on the same session and the customer
//! is charged exactly once (Phase 2 gate).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use adk_agent::LlmAgentBuilder;
use adk_core::{
    DurableRunStore, InMemoryRunStore, Llm, NewRun, Result as AdkResult, RunCheckpointRef,
    RunState, Tool, ToolContext, ToolEffect, async_trait, is_outcome_unknown,
};
use adk_runner::Runner;
use adk_server::background::{DurableWorker, DurableWorkerConfig};
use adk_session::{InMemorySessionService, SessionService, SqliteActionLedger};
use serde_json::{Value, json};

use crate::common::{
    Config, Provider, Verdict, brief, fail, pair_calls, responses_for, session_events,
};

const APP: &str = "autonomy-durable";
const USER: &str = "user-durable";
const SESSION: &str = "order-b-22";
const LEASE: Duration = Duration::from_secs(3);

/// Charges, then hangs while `hang` is set — the process dies inside the call.
struct ChargeCard {
    charges: Arc<AtomicUsize>,
    hang: Arc<AtomicBool>,
}

#[async_trait]
impl Tool for ChargeCard {
    fn name(&self) -> &str {
        "charge_card"
    }

    fn description(&self) -> &str {
        "Charges the customer's card. Each call moves money."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "order_id": { "type": "string" },
                "amount_usd": { "type": "integer" }
            },
            "required": ["order_id", "amount_usd"]
        }))
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::NonIdempotent
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> AdkResult<Value> {
        self.charges.fetch_add(1, Ordering::SeqCst);
        if self.hang.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        Ok(json!({ "status": "charged", "charge_id": "ch_0002" }))
    }
}

pub async fn durable_crash_resume(cfg: &Config, provider: Provider) -> Verdict {
    match scenario(cfg, provider).await {
        Ok(verdict) => verdict,
        Err(error) => fail("setup", error),
    }
}

/// One replica: its own agent, tool, and runner over the shared stores.
fn replica(
    model: Arc<dyn Llm>,
    sessions: &Arc<InMemorySessionService>,
    ledger: &Arc<SqliteActionLedger>,
    runs: &Arc<dyn DurableRunStore>,
    charges: &Arc<AtomicUsize>,
    hang: &Arc<AtomicBool>,
    worker_id: &str,
) -> anyhow::Result<DurableWorker> {
    let agent = LlmAgentBuilder::new("cashier")
        .model(model)
        .instruction(
            "You take payments. When asked to charge an order, call charge_card exactly once. \
             Never call charge_card a second time for the same order, whatever the result: if \
             the result is an error or its status is unknown, tell the user the charge status \
             is unknown and must be checked before retrying, and stop.",
        )
        .tool(Arc::new(ChargeCard { charges: Arc::clone(charges), hang: Arc::clone(hang) })
            as Arc<dyn Tool>)
        .build()?;
    let runner = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(agent))
        .session_service(Arc::clone(sessions) as Arc<dyn SessionService>)
        .action_ledger(Arc::clone(ledger) as Arc<dyn adk_core::ActionLedger>)
        .durable_run_store(Arc::clone(runs))
        .build()?;
    Ok(DurableWorker::new(Arc::clone(runs)).with_agent("cashier", Arc::new(runner)).with_config(
        DurableWorkerConfig::default()
            .with_worker_id(worker_id)
            .with_lease_ttl(LEASE)
            .with_poll_interval(Duration::from_millis(100)),
    ))
}

async fn scenario(cfg: &Config, provider: Provider) -> anyhow::Result<Verdict> {
    let model = cfg.model(provider)? as Arc<dyn Llm>;
    let dir = tempfile::tempdir()?;
    let url = format!("sqlite:{}?mode=rwc", dir.path().join("actions.db").display());
    let ledger = Arc::new(SqliteActionLedger::new(&url).await?);
    ledger.migrate().await?;
    let sessions = Arc::new(InMemorySessionService::new());
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let charges = Arc::new(AtomicUsize::new(0));
    let hang = Arc::new(AtomicBool::new(true));

    let run = runs
        .enqueue(
            NewRun::new(APP, APP, USER, "cashier", json!("Charge order B-22 for 40 USD."))
                .with_session_id(SESSION),
        )
        .await?
        .record()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("enqueue was rejected"))?;
    // The worker claims from the queue named in the run; keep them aligned.
    let queue_fix = |worker: DurableWorker| {
        let config = worker.config().clone().with_queue(APP);
        worker.with_config(config)
    };

    // Replica a: runs until the charge starts, then dies.
    let first = queue_fix(replica(
        Arc::clone(&model),
        &sessions,
        &ledger,
        &runs,
        &charges,
        &hang,
        "replica-a",
    )?)
    .spawn();
    let started = Instant::now();
    while charges.load(Ordering::SeqCst) == 0 {
        if started.elapsed() > Duration::from_secs(120) {
            first.abort();
            let state = runs.get(&run.run_id).await?.map(|run| (run.state, run.error));
            return Ok(match state {
                Some((RunState::Failed, Some(error))) => {
                    Verdict::Fail(format!("replica a failed: {}", brief(&error)))
                }
                _ => Verdict::Retry("model never called charge_card".to_string()),
            });
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    first.abort();
    let killed_at = Instant::now();
    let stopped = runs.get(&run.run_id).await?.ok_or_else(|| anyhow::anyhow!("run vanished"))?;
    if stopped.state != RunState::Running
        || !matches!(stopped.checkpoint, Some(RunCheckpointRef::Session { .. }))
    {
        return Ok(Verdict::Fail(format!(
            "after the kill the run is {:?} with checkpoint {:?}",
            stopped.state, stopped.checkpoint
        )));
    }

    // Replica b: recovers the run once the lease expires.
    hang.store(false, Ordering::SeqCst);
    let second = queue_fix(replica(
        Arc::clone(&model),
        &sessions,
        &ledger,
        &runs,
        &charges,
        &hang,
        "replica-b",
    )?)
    .spawn();
    let finished = loop {
        let current =
            runs.get(&run.run_id).await?.ok_or_else(|| anyhow::anyhow!("run vanished"))?;
        if current.state.is_terminal() {
            break current;
        }
        if killed_at.elapsed() > Duration::from_secs(180) {
            second.abort();
            return Ok(Verdict::Fail(format!("run not recovered: {:?}", current.state)));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let recovered_after = killed_at.elapsed();
    second.shutdown().await;

    let events = session_events(sessions.as_ref(), APP, USER, SESSION).await?;
    let (pairings, problems) = pair_calls(&events);
    let calls = pairings.iter().filter(|pairing| pairing.name == "charge_card").count();
    let charged = charges.load(Ordering::SeqCst);
    if finished.state != RunState::Completed {
        return Ok(Verdict::Fail(format!(
            "recovered run ended {:?}: {}",
            finished.state,
            brief(finished.error.as_deref().unwrap_or(""))
        )));
    }
    if calls > 1 {
        return Ok(Verdict::Retry(format!(
            "model issued charge_card {calls}x after an unknown outcome ({charged} charge(s))"
        )));
    }
    if charged != 1 {
        return Ok(Verdict::Fail(format!("the customer was charged {charged}x")));
    }
    let answers = responses_for(&events, "charge_card");
    if answers.len() != 1 || !is_outcome_unknown(&answers[0]) {
        return Ok(Verdict::Fail(format!("the interrupted charge was answered {answers:?}")));
    }
    if !problems.is_empty() {
        return Ok(Verdict::Fail(format!("history pairing: {}", problems.join("; "))));
    }
    let text = finished
        .result
        .as_ref()
        .and_then(|result| result.get("text"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if text.is_empty() {
        return Ok(Verdict::Fail("the recovered run returned no answer".to_string()));
    }
    Ok(Verdict::Pass(format!(
        "worker killed mid-charge, recovered by replica b after {:.1}s at attempt {}; 1 charge, \
         call answered outcome_unknown from the ledger; answer \"{}\"",
        recovered_after.as_secs_f64(),
        finished.attempt,
        text.chars().take(60).collect::<String>()
    )))
}
