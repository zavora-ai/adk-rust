//! Each wake source keys its events as the spec 3.2 §7.4 key table says, over the real
//! `adk-agent` triggers, `StoreApprovalService`, `DurableWorker` hooks and `QueueIngress`.

use crate::wake_support;

use std::sync::Arc;
use std::time::Duration;

use adk_agent::ambient::CronTrigger;
use adk_core::queue_source::InMemoryQueueSource;
use adk_core::{
    ApprovalDecision, ApprovalPolicy, ApprovalRequest, ApprovalScope, ApprovalService,
    ApproverSelector, DurableRunStore, InMemoryApprovalStore, InMemoryRunStore, NewRun, RunFilter,
    RunState, StoreApprovalService, SubjectRef, ToolConfirmationRequest,
};
use adk_kernel::{
    ApprovalWakeSource, CronWakeSource, FileWakeSource, KERNEL_WAKE_TARGET, QueueClass,
    RunFinishedWakeSource, WakeEvent, WakeSource, keys, queue_wake_ingress,
};
use adk_server::background::{DurableWorker, DurableWorkerConfig, WorkerHooks, WorkflowRegistry};
use chrono::{DateTime, Utc};
use futures::StreamExt;
use futures::stream::BoxStream;
use serde_json::json;
use wake_support::eventually;

async fn next_within(
    events: &mut BoxStream<'static, WakeEvent>,
    limit: Duration,
) -> Option<WakeEvent> {
    tokio::time::timeout(limit, events.next()).await.ok().flatten()
}

#[tokio::test]
async fn cron_ticks_are_keyed_by_their_scheduled_time() {
    let source = CronWakeSource::new("every-second", CronTrigger::new("* * * * * *").unwrap());
    let mut events = source.subscribe().await.unwrap();
    let tick = next_within(&mut events, Duration::from_secs(3)).await.expect("a tick within 3 s");
    let scheduled_for: DateTime<Utc> =
        tick.payload["scheduled_for"].as_str().unwrap().parse().unwrap();
    assert_eq!(tick.source, "every-second");
    assert_eq!(tick.key, keys::cron("every-second", scheduled_for));
    assert_eq!(tick.payload["expression"], "* * * * * *");
    assert_eq!(tick.principal, None);
    let next = next_within(&mut events, Duration::from_secs(3)).await.expect("a second tick");
    assert_ne!(next.key, tick.key, "each tick has its own key");
}

#[tokio::test]
async fn a_file_save_wakes_once_keyed_by_its_modification_time() {
    let dir = tempfile::tempdir().unwrap();
    let source = FileWakeSource::new("notes", dir.path(), "*.md")
        .unwrap()
        .with_debounce(Duration::from_millis(200));
    let mut events = source.subscribe().await.unwrap();
    // Let the watcher register before the first change.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let path = dir.path().join("notes.md");
    let saved = tokio::time::Instant::now();
    std::fs::write(&path, "first").unwrap();
    std::fs::write(dir.path().join("ignored.txt"), "not a match").unwrap();
    std::fs::write(&path, "first, then more").unwrap();
    let change = next_within(&mut events, Duration::from_secs(2)).await.expect("a wake within 2 s");
    assert!(saved.elapsed() < Duration::from_secs(2), "woke {:?} after the save", saved.elapsed());

    let root = std::fs::canonicalize(dir.path()).unwrap();
    let canonical = std::fs::canonicalize(&path).unwrap();
    let modified = DateTime::<Utc>::from(std::fs::metadata(&path).unwrap().modified().unwrap());
    assert_eq!(change.source, "notes");
    assert_eq!(change.key, keys::file(&root, &canonical, modified));
    assert_eq!(change.payload["path"], canonical.display().to_string());
    assert_eq!(change.payload["removed"], false);
    assert!(
        next_within(&mut events, Duration::from_millis(700)).await.is_none(),
        "the notifications of one save wake once"
    );

    std::fs::write(&path, "first").unwrap();
    let again = next_within(&mut events, Duration::from_secs(2)).await.expect("a later save wakes");
    assert_ne!(again.key, change.key, "a later save back to earlier contents has a new key");
}

#[tokio::test]
async fn a_resolved_approval_wakes_once_keyed_by_its_request_id() {
    let source = Arc::new(ApprovalWakeSource::new());
    let service = StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))
        .with_notifier(source.clone());
    let mut events = source.subscribe().await.unwrap();
    let call = ToolConfirmationRequest {
        tool_name: "transfer".into(),
        function_call_id: Some("call-1".into()),
        args: json!({ "amount": 900 }),
        origin: None,
        acting: None,
    };
    let policy = ApprovalPolicy::new([ApproverSelector::group("finance")]).with_quorum(2);
    let request = service
        .submit(ApprovalRequest::new("req-1", ApprovalScope::new("app", "u", "s"), &call, policy))
        .await
        .unwrap();
    let approve =
        |who: &str| ApprovalDecision::approve(SubjectRef::human(who)).with_groups(["finance"]);
    let first = service.decide(&request.id, request.version, approve("ana")).await.unwrap();
    assert!(
        next_within(&mut events, Duration::from_millis(100)).await.is_none(),
        "a decision short of the quorum is no wake"
    );
    service.decide(&first.id, first.version, approve("bo")).await.unwrap();

    let decided = next_within(&mut events, Duration::from_secs(1)).await.expect("the resolution");
    assert_eq!(decided.source, ApprovalWakeSource::NAME);
    assert_eq!(decided.key, keys::approval("req-1"));
    assert_eq!(decided.payload["fingerprint"], call.fingerprint());
    assert_eq!(decided.payload["state"], "approved");
    assert_eq!(decided.principal, None);
    assert!(next_within(&mut events, Duration::from_millis(100)).await.is_none());
}

#[tokio::test]
async fn a_finished_run_wakes_once_and_a_requeued_retry_does_not() {
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let source = Arc::new(RunFinishedWakeSource::new());
    let mut events = source.subscribe().await.unwrap();
    let workflows = WorkflowRegistry::new()
        .register("ok", |_, _| async { Ok(json!({"done": true})) })
        .register("flaky", |_, _| async { Err("not yet".to_string()) });
    let worker = DurableWorker::new(runs.clone())
        .with_workflows(Arc::new(workflows))
        .with_hooks(source.clone() as Arc<dyn WorkerHooks>)
        .with_config(
            DurableWorkerConfig::default()
                .with_queue("jobs")
                .with_poll_interval(Duration::from_millis(20)),
        );
    let ok = runs
        .enqueue(NewRun::new("jobs", "app", "u", "ok", json!({})))
        .await
        .unwrap()
        .record()
        .cloned()
        .unwrap();
    let flaky = runs
        .enqueue(NewRun::new("jobs", "app", "u", "flaky", json!({})).with_max_attempts(2))
        .await
        .unwrap()
        .record()
        .cloned()
        .unwrap();
    let handle = worker.spawn();

    let mut keys_seen = Vec::new();
    for _ in 0..2 {
        let event = next_within(&mut events, Duration::from_secs(5)).await.expect("a finished run");
        assert_eq!(event.source, RunFinishedWakeSource::NAME);
        keys_seen.push(event.key);
    }
    keys_seen.sort();
    let mut expected = vec![
        keys::run_finished(&ok.run_id, RunState::Completed),
        keys::run_finished(&flaky.run_id, RunState::Failed),
    ];
    expected.sort();
    assert_eq!(keys_seen, expected, "the requeued first attempt of the flaky run is no wake");
    assert!(next_within(&mut events, Duration::from_millis(200)).await.is_none());
    handle.shutdown().await;
}

#[tokio::test]
async fn queue_messages_become_kernel_wake_runs_keyed_by_message_id() {
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let queue = Arc::new(InMemoryQueueSource::new("orders"));
    let ingress = queue_wake_ingress(queue.clone(), runs.clone(), "adk-rust")
        .with_pull_wait(Duration::from_millis(10));
    queue.publish("m-1", br#"{"sku": 7}"#.to_vec());
    let report = ingress.poll_once().await.unwrap();
    assert_eq!(report.created.len(), 1);

    let run = runs.get(&report.created[0]).await.unwrap().unwrap();
    assert_eq!(run.target, KERNEL_WAKE_TARGET);
    assert_eq!(run.queue, QueueClass::MAINTENANCE);
    assert_eq!(run.idempotency_key.as_deref(), Some(keys::queue("orders", "m-1").as_str()));
    let event: WakeEvent = serde_json::from_value(run.input).unwrap();
    assert_eq!((event.source.as_str(), event.key.as_str()), ("orders", "queue:orders:m-1"));
    assert_eq!(event.payload, json!({"sku": 7}));

    queue.publish("m-1", br#"{"sku": 7}"#.to_vec());
    let redelivered = ingress.poll_once().await.unwrap();
    assert_eq!((redelivered.created.len(), redelivered.existing), (0, 1));
    let all = eventually("the store to settle", Duration::from_secs(1), || {
        let runs = runs.clone();
        async move { runs.list(RunFilter::default()).await.ok() }
    })
    .await;
    assert_eq!(all.len(), 1, "a redelivered message enqueues nothing");
}
