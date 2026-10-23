//! The managed form of the kernel (spec 3.2 §7.13, §10 M1): a `KernelDef` travels in a
//! `ManagedAgentDef` through the managed state store, the managed runtime resolves it through
//! a tenant's `KernelResolver`, and each tenant's kernel runs over that tenant's
//! `ManagedStores` only. On `origin/main` none of this exists: `adk-kernel` has no `managed`
//! module and a definition's metadata is not read by any kernel.
//!
//! The model is a scripted one that calls a counting `step` tool once and answers; the
//! kernels, workers, triggers, approval services and stores are real.

#![cfg(feature = "managed")]

mod managed_support;

use std::sync::Arc;
use std::time::Duration;

use adk_core::{
    ApprovalDecision, ApprovalState, InboxQuery, RunCheckpointRef, RunState, SubjectRef,
};
use adk_kernel::managed::{KernelDef, ManagedKernels, WakeSetDef};
use adk_kernel::{
    AgendaItem, AgendaOrigin, ConsolidationTick, KERNEL_WAKE_TARGET, PartitionMode,
    ProactiveApproval, ProactivityPolicy, ProcessFilter, ProposalOutcome, Wake, WakeEvent,
};
use adk_managed::types::{ManagedAgentDef, ModelRef, RuntimeError};
use adk_managed::{FileManagedStateStore, ManagedAgentRuntime, ManagedOwner};
use adk_session::{InMemorySessionService, ListRequest, SessionService};
use managed_support::*;
use serde_json::json;

fn digest(kernel: KernelDef) -> ManagedAgentDef {
    kernel.apply_to(
        ManagedAgentDef::new("digest", ModelRef::Shorthand("gemini-3.5-flash-lite".into()))
            .with_system("Keep it short."),
    )
}

fn on_hook(hook: &str) -> KernelDef {
    KernelDef::new().with_wake_set(WakeSetDef::new(
        hook,
        [Wake::Webhook { hook: hook.into() }],
        "Triage it.",
    ))
}

async fn sessions_of(sessions: &InMemorySessionService, app: &str, user: &str) -> usize {
    sessions
        .list(ListRequest { app_name: app.into(), user_id: user.into(), limit: None, offset: None })
        .await
        .unwrap()
        .len()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_kernel_def_round_trips_through_the_managed_state_store_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = Arc::new(InMemorySessionService::new());
    let state = || Arc::new(FileManagedStateStore::new(dir.path())) as _;
    let tenant = Tenant::with_state("tenant-a", sessions.clone(), false, state());
    let kernel = KernelDef::new()
        .with_wake_set(WakeSetDef::new("morning", [WakeSetDef::cron("0 0 9 * * *")], "Summarize."))
        .with_wake_set(WakeSetDef::new(
            "orders",
            [Wake::Webhook { hook: "orders".into() }],
            "Triage.",
        ));
    let def = digest(kernel.clone());
    let agent = tenant.runtime.create(def.clone()).await.unwrap();
    let owner = ManagedOwner::new("tenant-a", "ana").unwrap();
    let session = tenant.runtime.start_session(&agent, &owner, None).await.unwrap();

    // The session record the state store wrote holds the definition unchanged.
    let stored = tenant.kernel.stores().state.load(&session.0).await.unwrap().unwrap();
    let record = stored.record.expect("the session is recorded with its definition");
    assert_eq!(serde_json::to_value(&record.agent).unwrap(), serde_json::to_value(&def).unwrap());
    assert_eq!(KernelDef::from_agent_def(&record.agent).unwrap(), Some(kernel.clone()));

    // A new instance over the same stores rebuilds the session from that record and hosts
    // its definition on its own tenant kernel.
    let restarted = Tenant::with_state("tenant-a", sessions, false, state());
    assert!(restarted.kernel.definitions().await.is_empty());
    let rebuilt = adk_managed::DefaultManagedAgentRuntime::new(
        Arc::new(Models),
        Arc::new(InMemorySessionService::new()),
    )
    .with_stores(tenant.kernel.stores().clone())
    .with_agent_resolver(restarted.kernel.resolver());
    rebuilt.status(&session).await.unwrap();
    let hosted = restarted.kernel.definitions().await;
    assert_eq!(hosted.get("digest"), Some(&kernel));
    rebuilt.archive(&session).await.unwrap();
    tenant.runtime.archive(&session).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cron_wake_for_tenant_a_starts_a_run_on_tenant_as_stores_only() {
    // One session service and one definition name for both tenants: only the stores differ.
    let sessions = Arc::new(InMemorySessionService::new());
    let a = Tenant::new("tenant-a", sessions.clone(), false);
    let b = Tenant::new("tenant-b", sessions.clone(), false);
    let every_second = KernelDef::new().with_wake_set(WakeSetDef::new(
        "tick",
        [WakeSetDef::cron("* * * * * *")],
        "Do the step.",
    ));
    a.runtime.create(digest(every_second)).await.unwrap();
    b.runtime.create(digest(on_hook("orders"))).await.unwrap();
    let kernels = ManagedKernels::new();
    kernels.insert(a.kernel.clone()).unwrap();
    kernels.insert(b.kernel.clone()).unwrap();
    kernels.start().await.unwrap();

    let first = eventually("tenant A's cron run to complete", || async {
        a.all_runs()
            .await
            .into_iter()
            .find(|run| run.target == "digest" && run.state == RunState::Completed)
    })
    .await;
    // Another tick goes by on both kernels.
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    kernels.stop().await.unwrap();

    assert!(
        first.idempotency_key.as_deref().is_some_and(|key| key.starts_with("cron:")),
        "{:?}",
        first.idempotency_key
    );
    assert!(first.idempotency_key.as_deref().unwrap().ends_with("#digest/tick"));
    let runs = a.all_runs().await;
    assert!(runs.iter().all(|run| run.app_name == "tenant-a"), "{runs:?}");
    assert!(runs.iter().any(|run| run.target == KERNEL_WAKE_TARGET));
    assert!(a.tool_calls() >= 1);
    let processes = a.kernel.process_store().list(&ProcessFilter::default()).await.unwrap();
    assert!(processes.iter().any(|meta| meta.pid == first.run_id));
    assert!(sessions_of(&sessions, "tenant-a", "kernel").await >= 1);

    // Nothing reached tenant B: no run, no process, no model call, no session.
    assert!(b.all_runs().await.is_empty(), "{:?}", b.all_runs().await);
    assert!(b.kernel.process_store().list(&ProcessFilter::default()).await.unwrap().is_empty());
    assert_eq!((b.model_calls(), b.tool_calls()), (0, 0));
    assert_eq!(sessions_of(&sessions, "tenant-b", "kernel").await, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_held_call_runs_once_under_the_action_ledger_after_the_managed_approval() {
    let sessions = Arc::new(InMemorySessionService::new());
    let tenant = Tenant::new("tenant-a", sessions, true);
    tenant.runtime.create(digest(on_hook("orders"))).await.unwrap();
    tenant.kernel.start().await.unwrap();
    let kernel = tenant.kernel.kernel().await.unwrap();

    let delivery = WakeEvent::new("hook:orders", "hook:orders:evt-1", json!({ "order": 7 }));
    kernel.wake_router().submit(&delivery, "tenant-a").await.unwrap();
    let operator = || InboxQuery::for_approver(SubjectRef::human("ops"), [OPERATOR]);
    let request = eventually("the held call in the tenant's inbox", || async {
        tenant.service.inbox(operator()).await.unwrap().items.into_iter().next()
    })
    .await;
    let pid = request.run_id.clone().expect("the request names the parked process");
    let parked = eventually("the process parked on its held call", || async {
        tenant.runs.get(&pid).await.unwrap().filter(|run| run.state == RunState::Waiting)
    })
    .await;
    assert!(matches!(parked.checkpoint, Some(RunCheckpointRef::AwaitingApproval { .. })));
    assert_eq!(parked.app_name, "tenant-a");
    assert_eq!(tenant.tool_calls(), 0, "held, not run");

    // The same delivery again starts nothing (K-I3).
    kernel.wake_router().submit(&delivery, "tenant-a").await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let digests: Vec<_> =
        tenant.all_runs().await.into_iter().filter(|run| run.target == "digest").collect();
    assert_eq!(digests.len(), 1, "{digests:?}");

    // The managed approval path: the tenant's approval service, whose run store wakes the
    // parked process.
    let decided = tenant
        .service
        .decide(
            &request.id,
            request.version,
            ApprovalDecision::approve(SubjectRef::human("ops")).with_groups([OPERATOR]),
        )
        .await
        .unwrap();
    assert_eq!(decided.state, ApprovalState::Approved);
    eventually("the process to complete", || async {
        tenant.runs.get(&pid).await.unwrap().filter(|run| run.state == RunState::Completed)
    })
    .await;
    assert_eq!(tenant.tool_calls(), 1);
    let ledger = &tenant.ledger;
    assert_eq!(
        (
            ledger.begun.load(std::sync::atomic::Ordering::SeqCst),
            ledger.completed.load(std::sync::atomic::Ordering::SeqCst)
        ),
        (1, 1),
        "the call ran once under the action ledger"
    );

    // A restart of the tenant's kernel runs nothing again.
    tenant.kernel.stop().await.unwrap();
    tenant.kernel.start().await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    tenant.kernel.stop().await.unwrap();
    assert_eq!(tenant.tool_calls(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_definition_hosted_after_start_restarts_the_kernel_with_its_wake_sets() {
    let sessions = Arc::new(InMemorySessionService::new());
    let tenant = Tenant::new("tenant-a", sessions, false);
    tenant.kernel.start().await.unwrap();
    let before = tenant.kernel.kernel().await.unwrap();
    assert!(before.wake_router().triggers().is_empty());

    tenant.runtime.create(digest(on_hook("orders"))).await.unwrap();
    let after = tenant.kernel.kernel().await.unwrap();
    let names: Vec<_> = after.wake_router().triggers().iter().map(|t| t.name.clone()).collect();
    assert_eq!(names, ["digest/orders"]);
    assert_eq!(before.health().state, adk_kernel::KernelState::Stopped);

    // The same definition again changes nothing.
    tenant.runtime.create(digest(on_hook("orders"))).await.unwrap();
    let same = tenant.kernel.kernel().await.unwrap();
    assert_eq!(same.health().state, adk_kernel::KernelState::Running);
    assert_eq!(after.health().state, adk_kernel::KernelState::Running);

    let delivery = WakeEvent::new("hook:orders", "hook:orders:evt-2", json!({}));
    same.wake_router().submit(&delivery, "tenant-a").await.unwrap();
    eventually("the triggered run to complete", || async {
        tenant
            .all_runs()
            .await
            .into_iter()
            .find(|run| run.target == "digest" && run.state == RunState::Completed)
    })
    .await;
    assert_eq!(tenant.tool_calls(), 1);
    tenant.kernel.stop().await.unwrap();
}

#[tokio::test]
async fn the_runtime_refuses_a_kernel_block_it_cannot_host() {
    let sessions = Arc::new(InMemorySessionService::new());
    let tenant = Tenant::new("tenant-a", sessions.clone(), false);

    // An invalid block: refused at create, naming the key.
    let mut bad = digest(KernelDef::new());
    bad.metadata.get_or_insert_with(Default::default).insert(
        "adk.kernel".into(),
        r#"{"wakeSets":[{"name":"x","anyOf":[],"input":"y"}]}"#.into(),
    );
    match tenant.runtime.create(bad).await {
        Err(RuntimeError::InvalidRequest { message, .. }) => {
            assert!(
                message.contains("adk.kernel") && message.contains("at least one wake"),
                "{message}"
            )
        }
        other => panic!("expected an invalid request, got {other:?}"),
    }

    // A block on a definition no resolver builds: the kernel hosts only the operator's agents.
    let bare = adk_kernel::managed::TenantKernel::builder(
        "tenant-b",
        adk_managed::ManagedStores::in_memory(),
        sessions,
    )
    .build()
    .unwrap();
    let runtime = bare.runtime(Arc::new(Models));
    match runtime.create(digest(on_hook("orders"))).await {
        Err(RuntimeError::InvalidRequest { message, .. }) => {
            assert!(message.contains("adk.harness"), "{message}")
        }
        other => panic!("expected an invalid request, got {other:?}"),
    }
    // A definition without a block is the runtime's, as before.
    runtime
        .create(ManagedAgentDef::new("plain", ModelRef::Shorthand("gemini-3.5-flash-lite".into())))
        .await
        .unwrap();
    assert!(bare.definitions().await.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_tenant_kernel_runs_its_definitions_policy_and_the_tick_only_when_one_turns_it_on() {
    let sessions = Arc::new(InMemorySessionService::new());
    // The operator's policy gives the limits, never the tick (spec 3.2 §13 Q1).
    let operator = ProactivityPolicy::default()
        .with_max_proposed_open(4)
        .with_consolidation(ConsolidationTick::default());
    let tenant = Tenant::with_config(
        "tenant-a",
        sessions,
        fast_config("tenant-a").with_proactivity(operator),
    );
    let defaults = tenant.kernel.proactivity().await;
    assert_eq!((defaults.max_proposed_open, defaults.consolidation), (4, None));
    tenant.kernel.start().await.unwrap();
    let agenda = tenant.kernel.agenda_handle().get().expect("bound to the running kernel");
    assert_eq!(agenda.policy().consolidation, None);

    // A definition's policy is the tenant kernel's, the tick included.
    let policy = ProactivityPolicy::default()
        .with_approval(ProactiveApproval::AlwaysAsk)
        .with_consolidation(ConsolidationTick::default().with_min_new_facts(10));
    let with_policy = |name: &str, policy: ProactivityPolicy| {
        KernelDef::new()
            .with_proactivity(policy)
            .apply_to(ManagedAgentDef::new(name, ModelRef::Shorthand("claude-sonnet-5-5".into())))
    };
    tenant.runtime.create(with_policy("assistant", policy.clone())).await.unwrap();
    assert_eq!(tenant.kernel.proactivity().await, policy);
    let kernel = tenant.kernel.kernel().await.unwrap();
    assert_eq!(kernel.config().proactivity, policy);
    let rebound = tenant.kernel.agenda_handle().get().expect("rebound to the new kernel");
    assert_eq!(rebound.policy(), &policy);

    // One kernel, one policy: a second definition may repeat it or leave it out.
    tenant.runtime.create(with_policy("helper", policy.clone())).await.unwrap();
    tenant.runtime.create(digest(on_hook("orders"))).await.unwrap();
    let other = ProactivityPolicy::default().with_approval(ProactiveApproval::Never);
    match tenant.runtime.create(with_policy("rogue", other)).await {
        Err(RuntimeError::InvalidRequest { message, .. }) => {
            assert!(message.contains("one policy"), "{message}")
        }
        other => panic!("expected an invalid request, got {other:?}"),
    }
    assert_eq!(tenant.kernel.proactivity().await, policy);
    tenant.kernel.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_consolidation_item_waits_for_a_person_even_in_an_auto_partition() {
    let sessions = Arc::new(InMemorySessionService::new());
    let tenant = Tenant::new("tenant-a", sessions, false);
    let policy = ProactivityPolicy::default().with_consolidation(ConsolidationTick::default());
    tenant
        .runtime
        .create(KernelDef::new().with_proactivity(policy).apply_to(ManagedAgentDef::new(
            "assistant",
            ModelRef::Shorthand("claude-sonnet-5-5".into()),
        )))
        .await
        .unwrap();
    tenant.kernel.start().await.unwrap();
    let agenda = tenant.kernel.agenda_handle().get().unwrap();
    let item = |id: &str, origin: AgendaOrigin| {
        AgendaItem::new(
            id,
            SubjectRef::human("ana"),
            "Tidy the notes",
            "weekly",
            origin,
            chrono::Utc::now(),
        )
    };

    // K-I23: the tick's proposal goes to the tenant's inbox in Auto too.
    let consolidation = agenda
        .propose(item("from-tick", AgendaOrigin::Consolidation), PartitionMode::Auto)
        .await
        .unwrap();
    assert!(matches!(consolidation, ProposalOutcome::AwaitingApproval { .. }), "{consolidation:?}");
    let inbox = tenant
        .service
        .inbox(InboxQuery::for_approver(SubjectRef::human("ops"), [OPERATOR]))
        .await
        .unwrap();
    assert!(inbox.items.iter().any(|request| request.tool_name == adk_kernel::AGENDA_TOOL_NAME));

    // An agent's proposal follows the mode: Auto schedules it.
    let agent = agenda
        .propose(
            item("from-agent", AgendaOrigin::Agent { pid: "run-1".into(), confidence: 0.9 }),
            PartitionMode::Auto,
        )
        .await
        .unwrap();
    assert!(matches!(agent, ProposalOutcome::Scheduled { .. }), "{agent:?}");
    tenant.kernel.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_tenant_kernel_opens_no_control_plane_of_its_own() {
    // Every tenant's default home is the same directory, so its socket would collide.
    let sessions = Arc::new(InMemorySessionService::new());
    let config = adk_kernel::KernelConfig::default()
        .with_control(adk_kernel::control::ControlPlaneConfig::default());
    let a = Tenant::with_config("tenant-a", sessions.clone(), config.clone());
    let b = Tenant::with_config("tenant-b", sessions, config);
    a.kernel.start().await.unwrap();
    b.kernel.start().await.unwrap();
    for tenant in [&a, &b] {
        let kernel = tenant.kernel.kernel().await.unwrap();
        assert_eq!(kernel.config().control.socket, None);
        assert_eq!(kernel.config().control.http, None);
        assert_eq!(kernel.config().app_name, tenant.name);
    }
    a.kernel.stop().await.unwrap();
    b.kernel.stop().await.unwrap();
}
