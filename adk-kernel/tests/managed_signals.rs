//! Signal wakes in a managed definition (spec 3.2 §7.13, spec 3.5 §7.10, #1276): a wake set
//! hears a signal kind in its own partition, the tenant's signal sink files the signal and
//! wakes the tenant's kernel, and the run it starts is unauthenticated with the definition's
//! own input. On `origin/main` a definition's signal wake is refused and a tenant has no
//! signal sink.
//!
//! The model is a scripted one that calls a counting `step` tool once and answers; the
//! kernels, workers, triggers, sinks and stores are real.

#![cfg(all(feature = "managed", feature = "charter"))]

use crate::managed_support;

use std::sync::Arc;
use std::time::Duration;

use adk_core::{Enqueued, RunRecord, RunState};
use adk_kernel::charter::{InMemorySignalStore, SignalOutcome, SignalStore};
use adk_kernel::managed::{KernelDef, ManagedKernels, PartitionDef, WakeSetDef};
use adk_kernel::{KERNEL_WAKE_TARGET, PartitionId, Wake, WakeEvent};
use adk_managed::ManagedAgentRuntime;
use adk_managed::types::{ManagedAgentDef, ModelRef, RuntimeError};
use adk_session::InMemorySessionService;
use awp_types::{Bounded, Signal, SignalKind};
use managed_support::*;
use serde_json::json;

const INPUT: &str = "Write the page the visitors asked for.";

fn definition(name: &str, kernel: KernelDef) -> ManagedAgentDef {
    kernel.apply_to(ManagedAgentDef::new(name, ModelRef::Shorthand("gemini-3.5-flash-lite".into())))
}

/// A wake set `gaps` hearing `content_gap` signals in its own partition.
fn on_gaps() -> WakeSetDef {
    WakeSetDef::new("gaps", [Wake::Signal { kind: "content_gap".into(), partition: None }], INPUT)
}

fn gap(topic: &str) -> Signal {
    Signal {
        id: uuid::Uuid::new_v4(),
        kind: SignalKind::ContentGap {
            topic: Bounded::new(topic, 80).unwrap(),
            question_count: 1,
            samples: vec![],
        },
        emitted_by: "concierge".into(),
        emitted_at: chrono::Utc::now(),
    }
}

fn signals() -> Arc<dyn SignalStore> {
    Arc::new(InMemorySignalStore::new())
}

/// The completed runs of `target`.
async fn completed(tenant: &Tenant, target: &str) -> Vec<RunRecord> {
    tenant
        .all_runs()
        .await
        .into_iter()
        .filter(|run| run.target == target && run.state == RunState::Completed)
        .collect()
}

/// Waits for `count` completed runs of `target`.
async fn completes(tenant: &Tenant, target: &str, count: usize) -> Vec<RunRecord> {
    eventually(&format!("{count} completed run(s) of {target}"), || async {
        let runs = completed(tenant, target).await;
        (runs.len() >= count).then_some(runs)
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_signal_starts_the_definition_unauthenticated_with_its_own_input() {
    let tenant =
        Tenant::with_signals("tenant-a", Arc::new(InMemorySessionService::new()), signals());
    tenant
        .runtime
        .create(definition("writer", KernelDef::new().with_wake_set(on_gaps())))
        .await
        .unwrap();
    tenant.kernel.start().await.unwrap();
    let kernel = tenant.kernel.kernel().await.unwrap();
    // The wake set's unset partition is its own, the default partition.
    let wakes: Vec<_> = kernel.wake_router().triggers().iter().map(|t| t.wake.clone()).collect();
    assert_eq!(
        wakes,
        [Wake::Signal { kind: "content_gap".into(), partition: Some(PartitionId::default()) }]
    );

    let sink = tenant.kernel.signal_sink(PartitionId::DEFAULT).await.unwrap();
    let SignalOutcome::Filed { item, count: 1 } = sink.emit(gap("parking")).await.unwrap() else {
        panic!("the first signal is filed");
    };
    let [run] = completes(&tenant, "writer", 1).await.try_into().unwrap();
    assert!(!run.authenticated, "a signal carries no principal (spec 3.1 I6)");
    assert_eq!(run.input, json!({ "text": INPUT }), "the definition's input, never the signal's");
    assert_eq!(run.app_name, "tenant-a");
    let key = run.idempotency_key.clone().unwrap();
    assert_eq!(key, format!("signal:default:{item}:1#writer/gaps"));

    // The same wake again is the run store's to deduplicate (K-I3).
    let repeat = WakeEvent::new(
        "signal",
        format!("signal:default:{item}:1"),
        json!({ "kind": "content_gap", "partition": "default", "item": item, "count": 1 }),
    );
    let again = kernel.wake_router().submit(&repeat, "tenant-a").await.unwrap();
    assert!(matches!(again, Enqueued::Existing(_)), "{again:?}");

    // A repeat of the signal is counted on its item and wakes the set again.
    let counted = sink.emit(gap("parking")).await.unwrap();
    assert_eq!(counted, SignalOutcome::Counted { item: item.clone(), count: 2 });
    let runs = completes(&tenant, "writer", 2).await;
    assert!(
        runs.iter().any(|run| run.idempotency_key.as_deref()
            == Some(format!("signal:default:{item}:2#writer/gaps").as_str())),
        "{runs:?}"
    );
    assert!(runs.iter().all(|run| !run.authenticated && run.input == json!({ "text": INPUT })));
    tenant.kernel.stop().await.unwrap();
    assert_eq!(tenant.tool_calls(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_definition_hears_only_the_signals_of_its_own_partition() {
    let tenant =
        Tenant::with_signals("tenant-a", Arc::new(InMemorySessionService::new()), signals());
    // `work` declares its partition and hears gaps there; `home` hears them in the default.
    let work = KernelDef::new()
        .with_partition(PartitionDef::new("work", "work"))
        .with_wake_set(on_gaps().with_partition("work"));
    tenant.runtime.create(definition("work", work)).await.unwrap();
    tenant
        .runtime
        .create(definition("home", KernelDef::new().with_wake_set(on_gaps())))
        .await
        .unwrap();
    tenant.kernel.start().await.unwrap();

    tenant.kernel.signal_sink("work").await.unwrap().emit(gap("parking")).await.unwrap();
    completes(&tenant, "work", 1).await;
    tenant.kernel.signal_sink(PartitionId::DEFAULT).await.unwrap().emit(gap("menu")).await.unwrap();
    completes(&tenant, "home", 1).await;
    // Both wakes were routed; each started only its own partition's definition.
    tokio::time::sleep(Duration::from_millis(300)).await;
    tenant.kernel.stop().await.unwrap();
    let runs = tenant.all_runs().await;
    let count = |target: &str| runs.iter().filter(|run| run.target == target).count();
    let signal_wakes = runs
        .iter()
        .filter(|run| run.target == KERNEL_WAKE_TARGET)
        .filter(|run| run.idempotency_key.as_deref().is_some_and(|key| key.starts_with("signal:")))
        .count();
    assert_eq!((count("work"), count("home"), signal_wakes), (1, 1, 2), "{runs:?}");

    // A partition the tenant's table does not hold has no sink.
    let refused = tenant.kernel.signal_sink("back-office").await.unwrap_err();
    assert!(refused.to_string().contains("back-office"), "{refused}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sink_taken_before_the_kernel_restarts_wakes_the_new_kernel() {
    let tenant =
        Tenant::with_signals("tenant-a", Arc::new(InMemorySessionService::new()), signals());
    tenant
        .runtime
        .create(definition("writer", KernelDef::new().with_wake_set(on_gaps())))
        .await
        .unwrap();
    tenant.kernel.start().await.unwrap();
    let sink = tenant.kernel.signal_sink(PartitionId::DEFAULT).await.unwrap();
    let before = tenant.kernel.kernel().await.unwrap();

    // Hosting another definition restarts the tenant's kernel.
    let orders = KernelDef::new().with_wake_set(WakeSetDef::new(
        "orders",
        [Wake::Webhook { hook: "orders".into() }],
        "Triage it.",
    ));
    tenant.runtime.create(definition("triage", orders)).await.unwrap();
    assert_eq!(before.health().state, adk_kernel::KernelState::Stopped);

    sink.emit(gap("parking")).await.unwrap();
    completes(&tenant, "writer", 1).await;
    tenant.kernel.stop().await.unwrap();
}

#[tokio::test]
async fn a_signal_wake_is_refused_on_a_tenant_without_a_signal_store() {
    let sessions = Arc::new(InMemorySessionService::new());
    let tenant = Tenant::new("tenant-a", sessions, false);
    let def = definition("writer", KernelDef::new().with_wake_set(on_gaps()));
    match tenant.runtime.create(def).await {
        Err(RuntimeError::InvalidRequest { message, .. }) => assert!(
            message.contains("'writer'")
                && message.contains("`gaps`")
                && message.contains("TenantKernelBuilder::signal_store"),
            "{message}"
        ),
        other => panic!("expected an invalid request, got {other:?}"),
    }
    assert!(tenant.kernel.definitions().await.is_empty());
    let refused = tenant.kernel.signal_sink(PartitionId::DEFAULT).await.unwrap_err();
    assert!(refused.to_string().contains("signal store"), "{refused}");
}

#[test]
fn tenants_keep_their_own_signal_stores() {
    let sessions = Arc::new(InMemorySessionService::new());
    let shared = signals();
    let kernels = ManagedKernels::new();
    kernels
        .insert(Tenant::with_signals("tenant-a", sessions.clone(), shared.clone()).kernel)
        .unwrap();
    let refused = kernels
        .insert(Tenant::with_signals("tenant-b", sessions.clone(), shared).kernel)
        .unwrap_err();
    assert!(refused.to_string().contains("signal store"), "{refused}");
    kernels.insert(Tenant::with_signals("tenant-c", sessions, signals()).kernel).unwrap();
}
