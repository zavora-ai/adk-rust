//! Wakes end to end on real `DurableWorker`s and a scripted model (spec 3.2 §7.4, §11, K3):
//! a sleeping agent costs nothing until a file save wakes it, through its keyed
//! `kernel.wake` run, within two seconds; a webhook delivery wakes its sleeper, the next one
//! falls back to the trigger, and a redelivery wakes nothing.

use crate::wake_support;

use std::sync::Arc;
use std::time::Duration;

use adk_agent::LlmAgentBuilder;
use adk_core::{DurableRunStore, Llm, NewRun, RunFilter, RunState};
use adk_kernel::{
    EventBus, FileWakeSource, InMemoryProcessStore, KernelEvent, KernelWakeExecutor, ProcessStore,
    Wake, WakeOutcome, WakeRouter, WakeSet, WakeSource,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_server::background::{DurableWorker, DurableWorkerConfig, WorkerHandle};
use adk_session::{InMemorySessionService, SessionService};
use futures::StreamExt;
use serde_json::json;
use wake_support::{CountingRuns, eventually, sleeper};

const POLL: Duration = Duration::from_millis(20);

fn agent_worker(runs: &Arc<dyn DurableRunStore>, model: Arc<ScriptedLlm>) -> WorkerHandle {
    let agent = LlmAgentBuilder::new("assistant")
        .model(model as Arc<dyn Llm>)
        .instruction("Answer briefly.")
        .build()
        .unwrap();
    let runner = Runner::builder()
        .app_name("adk-rust")
        .agent(Arc::new(agent))
        .session_service(Arc::new(InMemorySessionService::new()) as Arc<dyn SessionService>)
        .build()
        .unwrap();
    DurableWorker::new(runs.clone())
        .with_agent("adk-rust", Arc::new(runner))
        .with_config(
            DurableWorkerConfig::default()
                .with_worker_id("background")
                .with_queue("background")
                .with_poll_interval(POLL),
        )
        .spawn()
}

/// The maintenance worker that routes `kernel.wake` runs.
fn maintenance_worker(runs: &Arc<dyn DurableRunStore>, router: &WakeRouter) -> WorkerHandle {
    DurableWorker::new(runs.clone())
        .with_workflows(Arc::new(KernelWakeExecutor::new(router.clone())))
        .with_config(
            DurableWorkerConfig::default()
                .with_worker_id("maintenance")
                .with_queue("maintenance")
                .with_poll_interval(POLL),
        )
        .spawn()
}

async fn completed(runs: &Arc<dyn DurableRunStore>, run_id: &str) {
    eventually(&format!("run {run_id} to complete"), Duration::from_secs(5), || {
        let runs = runs.clone();
        let id = run_id.to_string();
        async move { runs.get(&id).await.unwrap().filter(|run| run.state == RunState::Completed) }
    })
    .await;
}

#[tokio::test]
async fn a_file_save_wakes_a_sleeping_agent_turn_within_two_seconds() {
    let runs: Arc<dyn DurableRunStore> = Arc::new(CountingRuns::default());
    let processes: Arc<dyn ProcessStore> = Arc::new(InMemoryProcessStore::new());
    let bus = EventBus::new(64);
    let router = WakeRouter::new(runs.clone(), processes.clone()).with_bus(bus.clone());
    let dir = tempfile::tempdir().unwrap();
    let wake = WakeSet::any_of([Wake::FileChange {
        root: dir.path().to_path_buf(),
        glob: "*.md".into(),
        debounce: Duration::from_millis(200),
    }]);
    let pid = sleeper(
        &runs,
        &processes,
        NewRun::new(
            "background",
            "adk-rust",
            "alice",
            "adk-rust",
            json!({"text": "read notes.md"}),
        )
        .with_session_id("notes"),
        wake,
    )
    .await;

    let source = FileWakeSource::new("notes", dir.path(), "*.md")
        .unwrap()
        .with_debounce(Duration::from_millis(200));
    let mut changes = source.subscribe().await.unwrap();
    let routing = {
        let router = router.clone();
        tokio::spawn(async move {
            while let Some(change) = changes.next().await {
                router.submit(&change, "adk-rust").await.unwrap();
            }
        })
    };
    let maintenance = maintenance_worker(&runs, &router);
    let model = Arc::new(ScriptedLlm::new("notes", [ScriptedResponse::text("Read it.")]));
    let worker = agent_worker(&runs, model.clone());
    let mut events = bus.subscribe();

    tokio::time::sleep(Duration::from_millis(500)).await;
    let asleep = runs.get(&pid).await.unwrap().unwrap();
    assert_eq!(
        (asleep.state, asleep.lease_owner),
        (RunState::Waiting, None),
        "asleep holds no lease"
    );
    assert!(model.requests().is_empty(), "a sleeping process makes no model call (K-I2)");

    let saved = tokio::time::Instant::now();
    std::fs::write(dir.path().join("notes.md"), "ship the kernel").unwrap();
    let woke = tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .expect("a wake within 2 s of the save")
        .unwrap();
    assert!(matches!(
        &woke.event,
        KernelEvent::Wake { outcome: Some(WakeOutcome::Woke { pid: woken }), .. } if *woken == pid
    ));
    let latency = saved.elapsed();
    completed(&runs, &pid).await;
    assert_eq!(model.requests().len(), 1, "one save, one turn");
    assert!(latency < Duration::from_secs(2), "woke {latency:?} after the save");

    worker.shutdown().await;
    maintenance.shutdown().await;
    routing.abort();
}

#[cfg(feature = "webhooks")]
#[tokio::test]
async fn a_webhook_wakes_its_sleeper_then_its_trigger_and_a_redelivery_wakes_nothing() {
    use adk_core::webhook::{WebhookSecret, WebhookSigner, WebhookVerifier};
    use adk_kernel::{Trigger, keys, webhook_wake_hook};
    use adk_server::webhooks::{WebhookIngress, webhook_ingress_router};

    let counting = Arc::new(CountingRuns::default());
    let runs: Arc<dyn DurableRunStore> = counting.clone();
    let processes: Arc<dyn ProcessStore> = Arc::new(InMemoryProcessStore::new());
    let bus = EventBus::new(64);
    let mut events = bus.subscribe();
    let template =
        NewRun::new("background", "adk-rust", "alice", "adk-rust", json!({"text": "triage"}))
            .with_session_id("triage");
    let router = WakeRouter::new(runs.clone(), processes.clone())
        .with_bus(bus.clone())
        .with_trigger(Trigger::new("triage", Wake::Webhook { hook: "github".into() }, template));
    let sleeping = sleeper(
        &runs,
        &processes,
        NewRun::new("background", "adk-rust", "alice", "adk-rust", json!({"text": "go on"}))
            .with_session_id("waiting"),
        WakeSet::any_of([Wake::Webhook { hook: "github".into() }]),
    )
    .await;

    let secret = WebhookSecret::from_bytes(b"a shared signing key of 32 bytes".to_vec()).unwrap();
    let ingress = WebhookIngress::new(runs.clone()).with_hook(webhook_wake_hook(
        "github",
        WebhookVerifier::new(secret.clone()),
        "adk-rust",
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, webhook_ingress_router(ingress)).await.unwrap();
    });
    let maintenance = maintenance_worker(&runs, &router);
    let model = Arc::new(ScriptedLlm::new(
        "triage",
        [ScriptedResponse::text("Resumed."), ScriptedResponse::text("Triaged.")],
    ));
    let background = agent_worker(&runs, model.clone());

    let signer = WebhookSigner::new(secret);
    let client = reqwest::Client::new();
    let deliver = |id: &'static str| {
        let body = serde_json::to_vec(&json!({"action": "opened", "delivery": id})).unwrap();
        let signed = signer.sign_now(id, &body);
        let mut request = client.post(format!("http://{address}/hooks/github")).body(body);
        for (name, value) in signed.header_pairs() {
            request = request.header(name, value);
        }
        async move {
            let response = request.send().await.unwrap();
            let status = response.status().as_u16();
            (status, response.json::<serde_json::Value>().await.unwrap())
        }
    };

    let (status, first) = deliver("msg_1").await;
    assert_eq!((status, first["duplicate"].clone()), (202, json!(false)));
    completed(&runs, &sleeping).await;
    let (status, _) = deliver("msg_2").await;
    assert_eq!(status, 202);
    let spawned = eventually("the trigger's run", Duration::from_secs(5), || {
        let runs = runs.clone();
        let sleeping = sleeping.clone();
        async move {
            let runs = runs.list(RunFilter::default().queue("background")).await.unwrap();
            runs.into_iter().find(|run| run.run_id != sleeping)
        }
    })
    .await;
    assert_eq!(spawned.idempotency_key, Some(keys::trigger("hook:github:msg_2", "triage")));
    completed(&runs, &spawned.run_id).await;

    for id in ["msg_1", "msg_2"] {
        let (status, again) = deliver(id).await;
        assert_eq!((status, again["duplicate"].clone()), (200, json!(true)), "{id}");
    }
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(runs.list(RunFilter::default().queue("maintenance")).await.unwrap().len(), 2);
    assert_eq!(runs.list(RunFilter::default().queue("background")).await.unwrap().len(), 2);
    assert_eq!(counting.wakes_of(&sleeping), 1);
    assert_eq!(model.requests().len(), 2, "one turn for the sleeper, one for the trigger");
    let mut outcomes = Vec::new();
    while let Ok(Some(record)) =
        tokio::time::timeout(Duration::from_millis(50), events.recv()).await
    {
        if let KernelEvent::Wake { key, outcome: Some(outcome), .. } = record.event {
            outcomes.push((key, outcome));
        }
    }
    assert_eq!(
        outcomes,
        [
            ("hook:github:msg_1".to_string(), WakeOutcome::Woke { pid: sleeping }),
            (
                "hook:github:msg_2".to_string(),
                WakeOutcome::Spawned { pid: spawned.run_id, trigger: "triage".into() }
            ),
        ]
    );

    maintenance.shutdown().await;
    background.shutdown().await;
    server.abort();
}
