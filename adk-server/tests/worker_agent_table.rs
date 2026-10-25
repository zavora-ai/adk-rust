//! The durable worker's live target table (spec 3.3 §10 S4): a target inserted after the
//! worker started runs, a removed one is refused, an expected one that is not built yet is
//! handed back and runs once built, and `with_agent` keeps its behaviour.

#![cfg(feature = "background")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_agent::LlmAgentBuilder;
use adk_core::{DurableRunStore, InMemoryRunStore, Llm, NewRun, RunRecord, RunState};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_server::background::{
    AgentTable, DurableWorker, DurableWorkerConfig, ParkReason, WorkerHooks,
};
use adk_session::{InMemorySessionService, SessionService};
use async_trait::async_trait;
use serde_json::json;

const POLL: Duration = Duration::from_millis(20);

fn config(id: &str) -> DurableWorkerConfig {
    DurableWorkerConfig::default()
        .with_worker_id(id)
        .with_lease_ttl(Duration::from_millis(500))
        .with_poll_interval(POLL)
}

/// A runner whose agent `agent` answers every turn with `reply`.
fn runner(agent: &str, reply: &str) -> Arc<Runner> {
    let replies: Vec<ScriptedResponse> = (0..8).map(|_| ScriptedResponse::text(reply)).collect();
    let model = Arc::new(ScriptedLlm::new(format!("{agent}-script"), replies));
    let agent = LlmAgentBuilder::new(agent).model(model as Arc<dyn Llm>).build().unwrap();
    Arc::new(
        Runner::builder()
            .app_name("adk-rust")
            .agent(Arc::new(agent))
            .session_service(Arc::new(InMemorySessionService::new()) as Arc<dyn SessionService>)
            .build()
            .unwrap(),
    )
}

async fn enqueue(runs: &Arc<dyn DurableRunStore>, target: &str, session: &str) -> RunRecord {
    runs.enqueue(
        NewRun::new("default", "adk-rust", "alice", target, json!("hello"))
            .with_session_id(session),
    )
    .await
    .unwrap()
    .record()
    .cloned()
    .unwrap()
}

/// Polls the run until it is in `state`, for at most five seconds.
async fn run_in_state(runs: &Arc<dyn DurableRunStore>, run_id: &str, state: RunState) -> RunRecord {
    let started = tokio::time::Instant::now();
    loop {
        let run = runs.get(run_id).await.unwrap().unwrap();
        if run.state == state {
            return run;
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "run {run_id} is {:?}, not {state:?}",
            run.state
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Records every park reason.
#[derive(Default)]
struct Parks(Mutex<Vec<ParkReason>>);

#[async_trait]
impl WorkerHooks for Parks {
    async fn on_park(&self, _run: &RunRecord, reason: ParkReason) {
        self.0.lock().unwrap().push(reason);
    }
}

/// A target inserted into the table after the claim loop started serves the next claim.
#[tokio::test]
async fn a_target_inserted_after_start_runs() {
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let table = AgentTable::new();
    let handle = DurableWorker::new(Arc::clone(&runs))
        .with_agent_table(table.clone())
        .with_config(config("w"))
        .spawn();
    tokio::time::sleep(POLL * 3).await;

    table.insert("finance", runner("finance", "invoices drafted"));
    let run = enqueue(&runs, "finance", "finance-1").await;
    let done = run_in_state(&runs, &run.run_id, RunState::Completed).await;
    assert_eq!(done.attempt, 1);
    handle.shutdown().await;
}

/// A target removed from the table, and no longer expected, fails as an unknown target does:
/// without retry.
#[tokio::test]
async fn a_removed_target_is_refused() {
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let table = AgentTable::new();
    table.insert("finance", runner("finance", "done"));
    table.expect("finance");
    let worker = DurableWorker::new(Arc::clone(&runs))
        .with_agent_table(table.clone())
        .with_config(config("w"));

    let first = enqueue(&runs, "finance", "finance-1").await;
    let finished = worker.run_next().await.unwrap().expect("the run is claimed");
    assert_eq!(
        (finished.run_id.as_str(), finished.state),
        (first.run_id.as_str(), RunState::Completed)
    );

    assert!(table.remove("finance").is_some());
    assert!(table.forget("finance"));
    assert!(!worker.has_target("finance"));
    let second = enqueue(&runs, "finance", "finance-2").await;
    let refused = worker.run_next().await.unwrap().expect("the run is claimed");
    assert_eq!(refused.run_id, second.run_id);
    assert_eq!(refused.state, RunState::Failed, "an unknown target is not retried");
    assert!(
        refused
            .error
            .as_deref()
            .is_some_and(|error| error.contains("no agent or workflow named 'finance'")),
        "{:?}",
        refused.error
    );
}

/// A claim of a target the table expects but has not built is handed back, claimable one
/// poll interval later, and runs once the runner is inserted, without another attempt.
#[tokio::test]
async fn an_expected_target_is_handed_back_and_runs_once_built() {
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let table = AgentTable::new();
    table.expect("finance");
    let parks = Arc::new(Parks::default());
    let worker = DurableWorker::new(Arc::clone(&runs))
        .with_agent_table(table.clone())
        .with_hooks(Arc::clone(&parks) as Arc<dyn WorkerHooks>)
        .with_config(config("w"));
    assert!(worker.has_target("finance"), "an expected target is accepted");

    let run = enqueue(&runs, "finance", "finance-1").await;
    let before = chrono::Utc::now();
    assert!(worker.run_next().await.unwrap().is_none(), "the run is handed back");
    let parked = runs.get(&run.run_id).await.unwrap().unwrap();
    assert_eq!(parked.state, RunState::Waiting);
    assert_eq!(parked.lease_owner, None);
    let not_before = parked.not_before.expect("claimable later");
    assert!(not_before >= before, "claimable no sooner than now");
    assert!(not_before <= chrono::Utc::now() + chrono::Duration::from_std(POLL).unwrap());
    assert_eq!(*parks.0.lock().unwrap(), [ParkReason::HandedBack]);

    // Still not built: handed back again, without failing.
    tokio::time::sleep(POLL * 2).await;
    assert!(worker.run_next().await.unwrap().is_none());
    assert_eq!(runs.get(&run.run_id).await.unwrap().unwrap().state, RunState::Waiting);

    table.insert("finance", runner("finance", "built now"));
    tokio::time::sleep(POLL * 2).await;
    let finished = worker.run_next().await.unwrap().expect("the run is claimed once built");
    assert_eq!(finished.state, RunState::Completed);
    assert_eq!(finished.attempt, 1, "a hand-back counts no attempt");
}

/// A runner swapped into the table serves the next claim.
#[tokio::test]
async fn a_swapped_runner_serves_the_next_claim() {
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let table = AgentTable::new();
    let first = runner("finance", "first");
    table.insert("finance", Arc::clone(&first));
    let worker = DurableWorker::new(Arc::clone(&runs))
        .with_agent_table(table.clone())
        .with_config(config("w"));
    enqueue(&runs, "finance", "finance-1").await;
    assert_eq!(worker.run_next().await.unwrap().unwrap().state, RunState::Completed);

    let second = runner("finance", "second");
    let replaced = table.insert("finance", Arc::clone(&second)).expect("replaced");
    assert!(Arc::ptr_eq(&replaced, &first));
    enqueue(&runs, "finance", "finance-2").await;
    assert_eq!(worker.run_next().await.unwrap().unwrap().state, RunState::Completed);
    assert!(Arc::ptr_eq(&table.get("finance").unwrap(), &second));
}

/// `with_agent` registers a target on the worker alone, as before: it wins over the table's
/// runner of the same name, a clone's registration does not reach the original, and a worker
/// without a table fails an unknown target as before.
#[tokio::test]
async fn with_agent_keeps_its_behaviour() {
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let worker = DurableWorker::new(Arc::clone(&runs))
        .with_agent("adk-rust", runner("adk-rust", "own"))
        .with_config(config("w"));
    let clone = worker.clone().with_agent("finance", runner("finance", "clone"));
    assert!(clone.has_target("finance"));
    assert!(!worker.has_target("finance"), "a clone's registration stays on the clone");
    assert!(worker.has_target("adk-rust"));

    let unknown = enqueue(&runs, "finance", "finance-1").await;
    let failed = worker.run_next().await.unwrap().expect("the run is claimed");
    assert_eq!((failed.run_id.as_str(), failed.state), (unknown.run_id.as_str(), RunState::Failed));

    // The worker's own registration is read before the table.
    let table = AgentTable::new();
    table.insert("adk-rust", runner("adk-rust", "table"));
    let worker = worker.with_agent_table(table.clone());
    enqueue(&runs, "adk-rust", "own-1").await;
    assert_eq!(worker.run_next().await.unwrap().unwrap().state, RunState::Completed);
    table.remove("adk-rust");
    assert!(worker.has_target("adk-rust"), "the worker's own target never leaves with the table's");
    enqueue(&runs, "adk-rust", "own-2").await;
    assert_eq!(worker.run_next().await.unwrap().unwrap().state, RunState::Completed);
}
