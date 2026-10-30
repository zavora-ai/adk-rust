//! The wake matcher (spec 3.2 §7.4, §9.2 K-I3, K-I21): events reach the router once per key
//! through their keyed `kernel.wake` run, a sleeper consumes a matching event and wakes once,
//! a trigger is the fallback and enqueues one service run per trigger, expiry acts, and
//! missed file changes are reconciled.

use crate::wake_support;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use adk_core::{DurableRunStore, Enqueued, NewRun, RunFilter, RunState, SubjectRef};
use adk_kernel::{
    EventBus, EventSubscriber, ExpiryAction, InMemoryProcessStore, KERNEL_WAKE_TARGET, KernelEvent,
    KernelWakeExecutor, PartitionId, ProcessKind, ProcessMeta, ProcessStore, QueueClass,
    RunFinishedWakeSource, Suspension, Trigger, Wake, WakeEvent, WakeOutcome, WakeRouter, WakeSet,
    kernel_wake_run, keys,
};
use adk_server::background::{DurableWorker, DurableWorkerConfig, WorkflowExecutor, WorkflowState};
use chrono::Utc;
use proptest::prelude::*;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use wake_support::{CountingRuns, sleeper, state, woken};

struct Fixture {
    counting: Arc<CountingRuns>,
    runs: Arc<dyn DurableRunStore>,
    processes: Arc<dyn ProcessStore>,
    bus: EventBus,
}

impl Fixture {
    fn new() -> Self {
        let counting = Arc::new(CountingRuns::default());
        Self {
            runs: counting.clone(),
            counting,
            processes: Arc::new(InMemoryProcessStore::new()),
            bus: EventBus::new(1024),
        }
    }

    fn router(&self) -> WakeRouter {
        WakeRouter::new(self.runs.clone(), self.processes.clone()).with_bus(self.bus.clone())
    }

    async fn sleep_on(&self, queue: &str, wake: WakeSet) -> String {
        let run = NewRun::new(queue, "adk-rust", "alice", "adk-rust", json!({"text": "go on"}));
        sleeper(&self.runs, &self.processes, run, wake).await
    }

    async fn wake_set(&self, pid: &str) -> Option<WakeSet> {
        self.processes.get(pid).await.unwrap().unwrap().wake
    }

    /// A maintenance worker that executes `kernel.wake` runs through `router`.
    fn maintenance(&self, router: &WakeRouter, id: &str) -> DurableWorker {
        DurableWorker::new(self.runs.clone())
            .with_workflows(Arc::new(KernelWakeExecutor::new(router.clone())))
            .with_config(
                DurableWorkerConfig::default().with_worker_id(id).with_queue("maintenance"),
            )
    }

    async fn trigger_runs(&self) -> Vec<adk_core::RunRecord> {
        self.runs.list(RunFilter::default().queue("background")).await.unwrap()
    }
}

/// Runs every claimable run of `worker`'s queue to the end of its attempt.
async fn drain(worker: &DurableWorker) {
    while worker.run_next().await.unwrap().is_some() {}
}

/// Every routed wake the bus published so far, as (key, outcome).
async fn routed(events: &mut EventSubscriber) -> Vec<(String, WakeOutcome)> {
    let mut routed = Vec::new();
    while let Ok(Some(record)) =
        tokio::time::timeout(Duration::from_millis(20), events.recv()).await
    {
        if let KernelEvent::Wake { key, outcome: Some(outcome), .. } = record.event {
            routed.push((key, outcome));
        }
    }
    routed
}

fn webhook(hook: &str, id: &str) -> WakeEvent {
    WakeEvent::new(format!("hook:{hook}"), keys::webhook(hook, id), json!({"id": id}))
}

fn template() -> NewRun {
    NewRun::new("background", "adk-rust", "alice", "adk-rust", json!({"text": "triage it"}))
}

#[tokio::test]
async fn a_matching_event_wakes_a_sleeper_once_and_a_duplicate_wakes_nothing() {
    let fx = Fixture::new();
    let mut events = fx.bus.subscribe();
    let pid = fx.sleep_on("s1", WakeSet::any_of([Wake::Webhook { hook: "github".into() }])).await;
    let router = fx.router();

    assert_eq!(router.route(webhook("gitlab", "1")).await.unwrap(), []);
    assert!(!woken(&fx.runs, &pid).await);

    let outcomes = router.route(webhook("github", "1")).await.unwrap();
    assert_eq!(outcomes, [WakeOutcome::Woke { pid: pid.clone() }]);
    assert!(woken(&fx.runs, &pid).await, "the sleeper is claimable at once");
    assert_eq!(fx.wake_set(&pid).await, None, "the wake consumed the set");

    assert_eq!(router.route(webhook("github", "1")).await.unwrap(), []);
    assert_eq!(fx.counting.wakes_of(&pid), 1);

    let published: Vec<KernelEvent> =
        [events.recv().await.unwrap(), events.recv().await.unwrap(), events.recv().await.unwrap()]
            .into_iter()
            .map(|record| record.event)
            .collect();
    let event = |pid: Option<&str>, key: &str, outcome: Option<WakeOutcome>| KernelEvent::Wake {
        pid: pid.map(str::to_string),
        source: key.rsplit_once(':').unwrap().0.to_string(),
        key: key.to_string(),
        outcome,
    };
    assert_eq!(
        published,
        [
            event(None, "hook:gitlab:1", None),
            event(Some(&pid), "hook:github:1", Some(WakeOutcome::Woke { pid: pid.clone() })),
            event(None, "hook:github:1", None),
        ]
    );
}

#[tokio::test]
async fn suspended_and_unparked_processes_are_not_woken() {
    let fx = Fixture::new();
    let wake = WakeSet::any_of([Wake::Queue { source: "orders".into() }]);
    let frozen = fx.sleep_on("s1", wake.clone()).await;
    let meta = fx.processes.get(&frozen).await.unwrap().unwrap();
    fx.processes.put(meta.with_suspension(Suspension::Freeze)).await.unwrap();
    let queued = fx
        .runs
        .enqueue(NewRun::new("s2", "adk-rust", "alice", "adk-rust", json!({})))
        .await
        .unwrap()
        .record()
        .cloned()
        .unwrap()
        .run_id;
    fx.processes
        .put(
            ProcessMeta::new(queued.clone(), QueueClass::Background, ProcessKind::Goal)
                .with_wake(wake.clone()),
        )
        .await
        .unwrap();

    let event = WakeEvent::new("orders", keys::queue("orders", "m-1"), json!({}));
    assert_eq!(fx.router().route(event).await.unwrap(), []);
    assert!(!woken(&fx.runs, &frozen).await, "a freeze-parked process wakes only on unfreeze");
    assert_eq!(fx.wake_set(&frozen).await, Some(wake.clone()));
    assert_eq!(state(&fx.runs, &queued).await, RunState::Queued);
    assert_eq!(fx.wake_set(&queued).await, Some(wake), "a process not yet asleep keeps its set");
}

#[tokio::test]
async fn a_trigger_spawns_one_keyed_service_run_when_no_sleeper_matched() {
    let fx = Fixture::new();
    let mut template = template().with_authenticated_user();
    template.max_attempts = 1;
    let trigger = Trigger::new("triage", Wake::Webhook { hook: "github".into() }, template)
        .with_partition("work");
    let router = fx.router().with_trigger(trigger);

    let outcomes = router.route(webhook("github", "7")).await.unwrap();
    let [WakeOutcome::Spawned { pid, trigger }] = outcomes.as_slice() else {
        panic!("expected one spawned process, got {outcomes:?}");
    };
    assert_eq!(trigger, "triage");
    let run = fx.runs.get(pid).await.unwrap().unwrap();
    assert_eq!(run.idempotency_key.as_deref(), Some("hook:github:7#triage"));
    assert_eq!(run.concurrency_key.as_deref(), Some("partition:work"));
    assert_eq!(
        (run.queue.as_str(), run.user_id.as_str(), run.max_attempts),
        ("background", "alice", 1)
    );
    assert!(!run.authenticated, "an event with no verified subject never runs authenticated");
    assert_eq!(run.principal.unwrap().on_behalf_of, Some(SubjectRef::service("alice")));
    assert_eq!(
        fx.processes.get(pid).await.unwrap().unwrap(),
        ProcessMeta::new(
            pid.clone(),
            QueueClass::Background,
            ProcessKind::Triggered { source: "triage".into() }
        )
        .with_partition("work")
    );

    let again = router.route(webhook("github", "7")).await.unwrap();
    assert_eq!(again, [WakeOutcome::Deduplicated { pid: pid.clone() }], "a retried route");
    assert_eq!(fx.trigger_runs().await.len(), 1, "a retried route enqueues nothing");
}

#[tokio::test]
async fn a_verified_principal_runs_as_an_authenticated_user_on_its_session() {
    let fx = Fixture::new();
    let trigger = Trigger::new(
        "triage",
        Wake::Webhook { hook: "github".into() },
        template().with_session_id("issues"),
    );
    let router = fx.router().with_trigger(trigger);
    let outcomes = router.route(webhook("github", "8").with_principal("github-app")).await.unwrap();
    let run = fx.runs.get(outcomes[0].pid()).await.unwrap().unwrap();
    assert!(run.authenticated);
    assert_eq!(run.user_id, "github-app");
    assert_eq!(run.concurrency_key.as_deref(), Some("issues"));
    assert_eq!(run.principal.unwrap().on_behalf_of, Some(SubjectRef::service("github-app")));
}

#[tokio::test]
async fn a_sleeper_consumes_the_event_and_a_trigger_is_the_fallback() {
    let fx = Fixture::new();
    let wake = Wake::Webhook { hook: "github".into() };
    let pid = fx.sleep_on("s1", WakeSet::any_of([wake.clone()])).await;
    let router = fx.router().with_trigger(Trigger::new("triage", wake, template()));

    assert_eq!(router.route(webhook("github", "1")).await.unwrap(), [WakeOutcome::Woke { pid }]);
    assert!(fx.trigger_runs().await.is_empty(), "the sleeper consumed the event");
    let next = router.route(webhook("github", "2")).await.unwrap();
    assert!(matches!(next.as_slice(), [WakeOutcome::Spawned { .. }]), "{next:?}");
}

#[tokio::test]
async fn two_triggers_on_one_queue_each_run_for_one_event() {
    let fx = Fixture::new();
    let wake = || Wake::Webhook { hook: "github".into() };
    let router = fx
        .router()
        .with_trigger(Trigger::new("triage", wake(), template()))
        .with_trigger(Trigger::new("label", wake(), template()));

    let outcomes = router.route(webhook("github", "1")).await.unwrap();
    let triggers: Vec<&str> = outcomes
        .iter()
        .map(|outcome| match outcome {
            WakeOutcome::Spawned { trigger, .. } => trigger.as_str(),
            other => panic!("expected spawns, got {other:?}"),
        })
        .collect();
    assert_eq!(triggers, ["triage", "label"]);
    let mut keys: Vec<String> =
        fx.trigger_runs().await.into_iter().filter_map(|run| run.idempotency_key).collect();
    keys.sort();
    assert_eq!(keys, ["hook:github:1#label", "hook:github:1#triage"]);
}

#[tokio::test]
async fn a_submitted_key_is_routed_once_so_its_repeat_cannot_start_a_trigger() {
    let fx = Fixture::new();
    let wake = Wake::Webhook { hook: "github".into() };
    let sleeping = fx.sleep_on("s1", WakeSet::any_of([wake.clone()])).await;
    let router = fx.router().with_trigger(Trigger::new("triage", wake, template()));
    let worker = fx.maintenance(&router, "m");
    let mut events = fx.bus.subscribe();

    let first = router.submit(&webhook("github", "1"), "adk-rust").await.unwrap();
    let Enqueued::Created(wake_run) = first else { panic!("a new key enqueues: {first:?}") };
    assert_eq!(wake_run.target, KERNEL_WAKE_TARGET);
    assert_eq!(wake_run.idempotency_key.as_deref(), Some("hook:github:1"));
    drain(&worker).await;
    assert!(woken(&fx.runs, &sleeping).await);

    let repeat = router.submit(&webhook("github", "1"), "adk-rust").await.unwrap();
    assert!(matches!(repeat, Enqueued::Existing(_)), "{repeat:?}");
    drain(&worker).await;
    assert!(fx.trigger_runs().await.is_empty(), "the repeat never reached the router");

    router.submit(&webhook("github", "2"), "adk-rust").await.unwrap();
    drain(&worker).await;
    let spawned = fx.trigger_runs().await;
    assert_eq!(spawned.len(), 1);
    assert_eq!(
        routed(&mut events).await,
        [
            ("hook:github:1".to_string(), WakeOutcome::Woke { pid: sleeping.clone() }),
            (
                "hook:github:2".to_string(),
                WakeOutcome::Spawned { pid: spawned[0].run_id.clone(), trigger: "triage".into() }
            ),
        ]
    );
    assert_eq!(fx.counting.wakes_of(&sleeping), 1);
}

#[tokio::test]
async fn a_run_finished_trigger_records_the_finished_run_as_parent() {
    let fx = Fixture::new();
    let router = fx.router().with_trigger(Trigger::new(
        "follow-up",
        Wake::RunFinished { pid: "run-1".into() },
        template(),
    ));
    let finished = WakeEvent::new(
        RunFinishedWakeSource::NAME,
        keys::run_finished("run-1", RunState::Completed),
        json!({"pid": "run-1", "state": "completed"}),
    );
    let outcomes = router.route(finished).await.unwrap();
    let meta = fx.processes.get(outcomes[0].pid()).await.unwrap().unwrap();
    assert_eq!(meta.parent.as_deref(), Some("run-1"));
    assert_eq!(meta.partition, PartitionId::default());
}

#[tokio::test]
async fn expired_wake_sets_cancel_or_run_and_passed_times_wake() {
    let fx = Fixture::new();
    let now = Utc::now();
    let past = now - chrono::Duration::seconds(1);
    let future = now + chrono::Duration::hours(1);
    let webhook_wake = || Wake::Webhook { hook: "github".into() };
    let cancel = fx
        .sleep_on("s1", WakeSet::any_of([webhook_wake()]).with_expiry(past, ExpiryAction::Cancel))
        .await;
    let run = fx
        .sleep_on("s2", WakeSet::any_of([webhook_wake()]).with_expiry(past, ExpiryAction::Run))
        .await;
    let later = fx
        .sleep_on("s3", WakeSet::any_of([webhook_wake()]).with_expiry(future, ExpiryAction::Cancel))
        .await;
    let due = fx.sleep_on("s4", WakeSet::any_of([Wake::At(past), Wake::Manual])).await;
    let not_due = fx.sleep_on("s5", WakeSet::any_of([Wake::At(future)])).await;
    let mut events = fx.bus.subscribe();

    let router = fx.router();
    assert_eq!(
        router.route(webhook("github", "1")).await.unwrap(),
        [WakeOutcome::Woke { pid: later.clone() }],
        "an expired set is not woken by a matching event"
    );
    let mut outcomes = router.sweep_expired(now).await.unwrap();
    outcomes.sort_by(|a, b| a.pid().cmp(b.pid()));
    let mut expected = vec![
        WakeOutcome::Expired { pid: cancel.clone(), action: ExpiryAction::Cancel },
        WakeOutcome::Expired { pid: run.clone(), action: ExpiryAction::Run },
        WakeOutcome::Woke { pid: due.clone() },
    ];
    expected.sort_by(|a, b| a.pid().cmp(b.pid()));
    assert_eq!(outcomes, expected);
    assert_eq!(state(&fx.runs, &cancel).await, RunState::Cancelled);
    assert!(woken(&fx.runs, &run).await && woken(&fx.runs, &due).await);
    assert!(!woken(&fx.runs, &not_due).await);
    for pid in [&cancel, &run, &due] {
        assert_eq!(fx.wake_set(pid).await, None);
    }
    assert!(router.sweep_expired(now).await.unwrap().is_empty(), "a sweep acts once");

    let mut published = Vec::new();
    while let Ok(Some(record)) =
        tokio::time::timeout(Duration::from_millis(50), events.recv()).await
    {
        published.push(record.event);
    }
    assert!(published.contains(&KernelEvent::Process {
        pid: cancel.clone(),
        from: RunState::Waiting,
        to: RunState::Cancelled,
        reason: Some("wake set expired".into()),
    }));
    assert!(published.iter().any(|event| matches!(
        event,
        KernelEvent::Wake { source, outcome: Some(WakeOutcome::Expired { action: ExpiryAction::Run, .. }), .. } if source == "expiry"
    )));
}

#[tokio::test]
async fn reconcile_wakes_sleepers_whose_files_changed_while_the_kernel_was_down() {
    let fx = Fixture::new();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("old.md"), "written before the process slept").unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    let file_wake = |glob: &str| {
        WakeSet::any_of([Wake::FileChange {
            root: dir.path().to_path_buf(),
            glob: glob.into(),
            debounce: Duration::from_millis(500),
        }])
    };
    let notes = fx.sleep_on("s1", file_wake("*.md")).await;
    let rust = fx.sleep_on("s2", file_wake("*.rs")).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    let stale = fx.sleep_on("s3", file_wake("old.md")).await;
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    let changed = dir.path().join("sub").join("notes.md");
    std::fs::write(&changed, "changed while down").unwrap();
    let mut events = fx.bus.subscribe();

    let outcomes = fx.router().reconcile().await.unwrap();
    assert_eq!(outcomes, [WakeOutcome::Woke { pid: notes.clone() }]);
    assert!(woken(&fx.runs, &notes).await);
    assert!(!woken(&fx.runs, &rust).await && !woken(&fx.runs, &stale).await);

    let root = std::fs::canonicalize(dir.path()).unwrap();
    let path = std::fs::canonicalize(&changed).unwrap();
    let modified =
        chrono::DateTime::<Utc>::from(std::fs::metadata(&path).unwrap().modified().unwrap());
    let record = events.recv().await.unwrap();
    assert_eq!(
        record.event,
        KernelEvent::Wake {
            pid: Some(notes.clone()),
            source: "reconcile".into(),
            key: keys::file(&root, &path, modified),
            outcome: Some(WakeOutcome::Woke { pid: notes }),
        }
    );
}

#[tokio::test]
async fn the_kernel_wake_executor_routes_the_event_a_wake_run_carries() {
    let fx = Fixture::new();
    let pid = fx.sleep_on("s1", WakeSet::any_of([Wake::Queue { source: "orders".into() }])).await;
    let executor = KernelWakeExecutor::new(fx.router());
    let event = WakeEvent::new("orders", keys::queue("orders", "m-1"), json!({"sku": 7}));
    let run = kernel_wake_run(&event, "adk-rust").unwrap();
    let input: WorkflowState = run.input.as_object().unwrap().clone().into_iter().collect();

    let result = executor
        .execute(KERNEL_WAKE_TARGET, input.clone(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(result, json!({"outcomes": [{"woke": {"pid": pid}}]}));
    assert!(woken(&fx.runs, &pid).await);

    let retried = executor.execute(KERNEL_WAKE_TARGET, input, CancellationToken::new()).await;
    assert_eq!(retried.unwrap(), json!({"outcomes": []}), "a retried wake run wakes nothing");
    let wrong = executor.execute("other", WorkflowState::new(), CancellationToken::new()).await;
    assert!(wrong.unwrap_err().contains("kernel.wake"));
    let garbage: WorkflowState = [("text".to_string(), Value::from("hi"))].into_iter().collect();
    let garbage = executor.execute(KERNEL_WAKE_TARGET, garbage, CancellationToken::new()).await;
    assert!(garbage.unwrap_err().contains("wake event"));
}

/// Sleepers and triggers by hook index, and deliveries as (hook, id, verified).
type Scenario = (Vec<usize>, Vec<usize>, Vec<(usize, u8, bool)>);

fn scenario() -> impl Strategy<Value = Scenario> {
    (
        proptest::collection::vec(0..3usize, 0..4),
        proptest::collection::vec(0..3usize, 0..3),
        proptest::collection::vec((0..3usize, 0..4u8, any::<bool>()), 1..16),
    )
}

const HOOKS: [&str; 3] = ["a", "b", "c"];

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// K-I3: deliveries repeat, interleave and race on two tasks, each submitting through
    /// the keyed `kernel.wake` run and draining its own maintenance worker. Each key is
    /// routed once; it wakes sleepers or, when none matched, starts one run per matching
    /// trigger, never both; each sleeper wakes once; no run key repeats.
    #[test]
    fn a_wake_never_produces_two_runs_for_one_key((sleepers, triggers, deliveries) in scenario()) {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let fx = Fixture::new();
            let mut sleeping = Vec::new();
            for (index, hook) in sleepers.iter().enumerate() {
                let wake = WakeSet::any_of([Wake::Webhook { hook: HOOKS[*hook].into() }]);
                sleeping.push((fx.sleep_on(&format!("sleep-{index}"), wake).await, *hook));
            }
            let router = triggers.iter().enumerate().fold(fx.router(), |router, (index, hook)| {
                router.with_trigger(Trigger::new(
                    format!("t{index}"),
                    Wake::Webhook { hook: HOOKS[*hook].into() },
                    template(),
                ))
            });
            let mut events = fx.bus.subscribe();
            let deliveries_as_events: Vec<WakeEvent> = deliveries
                .iter()
                .map(|(hook, id, verified)| {
                    let event = webhook(HOOKS[*hook], &id.to_string());
                    if *verified { event.with_principal("sender") } else { event }
                })
                .collect();
            let deliver = |worker: DurableWorker, router: WakeRouter, events: Vec<WakeEvent>| async move {
                for event in events {
                    router.submit(&event, "adk-rust").await.unwrap();
                    drain(&worker).await;
                }
                drain(&worker).await;
            };
            let (left, right) = tokio::join!(
                tokio::spawn(deliver(fx.maintenance(&router, "m1"), router.clone(), deliveries_as_events.clone())),
                tokio::spawn(deliver(
                    fx.maintenance(&router, "m2"),
                    router.clone(),
                    deliveries_as_events.iter().rev().cloned().collect(),
                )),
            );
            left.unwrap();
            right.unwrap();
            drain(&fx.maintenance(&router, "m3")).await;

            let distinct: HashSet<String> = deliveries_as_events.iter().map(|event| event.key.clone()).collect();
            let wake_runs = fx.runs.list(RunFilter::default().queue("maintenance")).await.unwrap();
            let mut wake_keys: Vec<String> =
                wake_runs.iter().filter_map(|run| run.idempotency_key.clone()).collect();
            wake_keys.sort();
            let mut expected_keys: Vec<String> = distinct.iter().cloned().collect();
            expected_keys.sort();
            assert_eq!(wake_keys, expected_keys, "one kernel.wake run per distinct key");
            assert!(wake_runs.iter().all(|run| run.state == RunState::Completed));

            let mut by_key: HashMap<String, Vec<WakeOutcome>> = HashMap::new();
            for (key, outcome) in routed(&mut events).await {
                by_key.entry(key).or_default().push(outcome);
            }
            for (key, outcomes) in &by_key {
                let woke = outcomes.iter().any(|outcome| matches!(outcome, WakeOutcome::Woke { .. }));
                let spawned = outcomes.iter().any(|outcome| matches!(outcome, WakeOutcome::Spawned { .. }));
                assert!(!(woke && spawned), "key {key} both woke a sleeper and started a trigger run");
            }

            let trigger_runs = fx.trigger_runs().await;
            let run_keys: Vec<String> = trigger_runs.iter().filter_map(|run| run.idempotency_key.clone()).collect();
            let unique: HashSet<&String> = run_keys.iter().collect();
            assert_eq!(unique.len(), run_keys.len(), "a run key repeated: {run_keys:?}");

            for (pid, hook) in &sleeping {
                let delivered = deliveries.iter().any(|(delivered, _, _)| delivered == hook);
                assert_eq!(fx.counting.wakes_of(pid), usize::from(delivered), "sleeper on hook {hook}");
            }
            for key in &distinct {
                let woke = by_key.get(key).is_some_and(|outcomes| {
                    outcomes.iter().any(|outcome| matches!(outcome, WakeOutcome::Woke { .. }))
                });
                let hook = key.split(':').nth(1).unwrap();
                for (index, trigger_hook) in triggers.iter().enumerate() {
                    if HOOKS[*trigger_hook] != hook {
                        continue;
                    }
                    let run_key = keys::trigger(key, &format!("t{index}"));
                    let count = run_keys.iter().filter(|run_key_seen| **run_key_seen == run_key).count();
                    assert_eq!(count, usize::from(!woke), "trigger run {run_key}");
                }
            }
        });
    }
}
