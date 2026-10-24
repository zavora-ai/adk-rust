//! Cron watermarks for tenant kernels (spec 3.2 §7.4, §7.13): the ticks of a definition's
//! cron wake sets missed while no replica of the tenant ran fire once each, as each wake
//! set's `missed` policy picks them, from the cursor kept in the tenant's own
//! `CronWatermarkStore`. On `origin/main` a tenant kernel's cron source keeps no cursor, so
//! nothing missed while it was down ever fires, whatever the policy says.
//!
//! The schedule is yearly (00:00 UTC on 1 January), so no live tick falls inside a test; the
//! cursor is seeded three years back, which leaves three or four missed ticks.

#![cfg(feature = "managed")]

mod managed_support;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::time::Duration;

use adk_core::schedule::MissedFirePolicy;
use adk_core::{
    ApprovalPolicy, ApprovalService, ApproverSelector, DurableRunStore, InMemoryApprovalStore,
    InMemoryRunStore, RunFilter, RunRecord, RunState, StoreApprovalService,
};
use adk_kernel::managed::{KernelDef, ManagedKernels, TenantKernel, WakeOverlap, WakeSetDef};
use adk_kernel::stores::InMemoryCronWatermarkStore;
use adk_kernel::{CronWatermarkStore, InMemoryProcessStore, ProcessStore, Wake};
use adk_managed::types::{ManagedAgentDef, ModelRef};
use adk_managed::{AgentResolver, InMemoryManagedStateStore, ManagedAgentRuntime, ManagedStores};
use adk_session::{InMemorySessionService, SessionService};
use chrono::{DateTime, Datelike, TimeZone, Utc};
use managed_support::*;

/// 00:00:00 UTC on 1 January.
const YEARLY: &str = "0 0 0 1 1 *";
/// The tenant kernel's cron source of [`YEARLY`]: the expression with `_` for spaces.
const YEARLY_SOURCE: &str = "0_0_0_1_1_*";

/// Every 1 January 00:00 UTC in `(after, now]`, oldest first.
fn missed_since(after: DateTime<Utc>) -> Vec<DateTime<Utc>> {
    let now = Utc::now();
    (after.year()..=now.year())
        .filter_map(|year| Utc.with_ymd_and_hms(year, 1, 1, 0, 0, 0).single())
        .filter(|tick| *tick > after && *tick <= now)
        .collect()
}

fn three_years_ago() -> DateTime<Utc> {
    Utc::now() - chrono::Duration::days(3 * 365 + 10)
}

/// What every instance of one tenant shares: its stores, sessions and watermarks.
struct Deployment {
    tenant: String,
    runs: Arc<dyn DurableRunStore>,
    stores: ManagedStores,
    processes: Arc<dyn ProcessStore>,
    sessions: Arc<InMemorySessionService>,
    watermarks: Arc<dyn CronWatermarkStore>,
    resolver: Arc<dyn AgentResolver>,
}

impl Deployment {
    fn new(tenant: &str, watermarks: Arc<dyn CronWatermarkStore>) -> Self {
        let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
        let decisions = Arc::new(InMemoryApprovalStore::new());
        let service: Arc<dyn ApprovalService> = Arc::new(
            StoreApprovalService::new(decisions.clone())
                .with_run_store(Arc::clone(&runs))
                .with_router(
                    ApprovalPolicy::new([ApproverSelector::group(OPERATOR)]).allow_self_approval(),
                ),
        );
        let stores = ManagedStores::new(
            Arc::new(InMemoryManagedStateStore::new()),
            Arc::clone(&runs),
            decisions,
        )
        .with_approval_service(service);
        let resolver: Arc<dyn AgentResolver> = Arc::new(TestResolver {
            model: Arc::new(StepModel { requests: AtomicUsize::new(0) }),
            calls: Arc::new(AtomicUsize::new(0)),
            confirm: false,
        });
        Self {
            tenant: tenant.to_string(),
            runs,
            stores,
            processes: Arc::new(InMemoryProcessStore::new()),
            sessions: Arc::new(InMemorySessionService::new()),
            watermarks,
            resolver,
        }
    }

    /// One instance — a replica, or the tenant after a restart — over the shared stores,
    /// hosting `def`.
    async fn instance(&self, worker: &str, def: &ManagedAgentDef) -> TenantKernel {
        let kernel = TenantKernel::builder(
            self.tenant.clone(),
            self.stores.clone(),
            Arc::clone(&self.sessions) as Arc<dyn SessionService>,
        )
        .config(fast_config(worker))
        .resolver(Arc::clone(&self.resolver))
        .process_store(Arc::clone(&self.processes))
        .watermark_store(Arc::clone(&self.watermarks))
        .build()
        .unwrap();
        kernel.runtime(Arc::new(Models)).create(def.clone()).await.unwrap();
        kernel
    }

    /// The completed runs of the definition's agent, by wake set, each with its tick.
    async fn fired(&self) -> BTreeMap<String, Vec<DateTime<Utc>>> {
        let mut fired: BTreeMap<String, Vec<DateTime<Utc>>> = BTreeMap::new();
        for run in self.runs.list(RunFilter::default()).await.unwrap() {
            if let Some((set, tick)) = wake_set_tick(&run) {
                fired.entry(set).or_default().push(tick);
            }
        }
        for ticks in fired.values_mut() {
            ticks.sort();
        }
        fired
    }

    /// Whether every run in the store has finished.
    async fn settled(&self) -> bool {
        self.runs.list(RunFilter::default()).await.unwrap().iter().all(|run| {
            matches!(run.state, RunState::Completed | RunState::Failed | RunState::Cancelled)
        })
    }
}

/// The wake set and the tick of a trigger run, keyed `cron:<source>:<tick>#digest/<set>`.
fn wake_set_tick(run: &RunRecord) -> Option<(String, DateTime<Utc>)> {
    let key = run.idempotency_key.as_deref()?.strip_prefix("cron:")?;
    let (event, trigger) = key.split_once('#')?;
    let set = trigger.strip_prefix("digest/")?;
    // The tick is everything after the source name and its colon.
    let tick = event.strip_prefix(&format!("{YEARLY_SOURCE}:"))?;
    let tick = DateTime::parse_from_rfc3339(tick).ok()?.with_timezone(&Utc);
    Some((set.to_string(), tick))
}

/// A definition whose wake sets on [`YEARLY`] have the given policies, each queueing its
/// processes, so a catch-up's processes run one after another.
fn digest(sets: &[(&str, MissedFirePolicy)]) -> ManagedAgentDef {
    let mut kernel = KernelDef::new();
    for (name, missed) in sets {
        kernel = kernel.with_wake_set(
            WakeSetDef::new(
                *name,
                [Wake::Cron { expr: YEARLY.into(), missed: *missed }],
                "Summarize the year.",
            )
            .with_overlap(WakeOverlap::Queue),
        );
    }
    kernel.apply_to(ManagedAgentDef::new(
        "digest",
        ModelRef::Shorthand("gemini-3.5-flash-lite".into()),
    ))
}

/// Waits until `expected` runs have fired and every run has finished.
async fn fired_and_settled(
    deployment: &Deployment,
    expected: usize,
) -> BTreeMap<String, Vec<DateTime<Utc>>> {
    eventually("the missed ticks to fire and their runs to finish", || async {
        let fired = deployment.fired().await;
        let count: usize = fired.values().map(Vec::len).sum();
        (count >= expected && deployment.settled().await).then_some(fired)
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn missed_ticks_fire_once_each_as_each_wake_set_picks_them() {
    let deployment = Deployment::new("tenant-a", Arc::new(InMemoryCronWatermarkStore::new()));
    let seeded = three_years_ago();
    deployment.watermarks.advance(YEARLY_SOURCE, seeded).await.unwrap();
    let missed = missed_since(seeded);
    assert!(missed.len() >= 3, "{missed:?}");
    let def = digest(&[
        ("skip", MissedFirePolicy::Skip),
        ("once", MissedFirePolicy::FireOnce),
        ("two", MissedFirePolicy::CatchUp(2)),
    ]);

    let first = deployment.instance("replica-1", &def).await;
    first.start().await.unwrap();
    let fired = fired_and_settled(&deployment, 3).await;
    let latest = *missed.last().unwrap();
    assert_eq!(fired.get("once"), Some(&vec![latest]), "fireOnce: the latest missed tick");
    assert_eq!(
        fired.get("two"),
        Some(&missed[missed.len() - 2..].to_vec()),
        "catchUp(2): the latest two, oldest first"
    );
    assert_eq!(fired.get("skip"), None, "skip: no missed tick is on time");
    let cursor = deployment.watermarks.read(YEARLY_SOURCE).await.unwrap().unwrap();
    assert!(cursor >= latest, "the cursor moved past the replayed ticks: {cursor}");

    // The tenant restarts: the cursor is past the gap, so nothing replays a second time.
    first.stop().await.unwrap();
    let restarted = deployment.instance("replica-2", &def).await;
    restarted.start().await.unwrap();
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    restarted.stop().await.unwrap();
    assert_eq!(deployment.fired().await, fired, "a restart replays nothing again");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_replicas_starting_together_fire_each_missed_tick_once() {
    let deployment = Deployment::new("tenant-a", Arc::new(InMemoryCronWatermarkStore::new()));
    let seeded = three_years_ago();
    deployment.watermarks.advance(YEARLY_SOURCE, seeded).await.unwrap();
    let missed = missed_since(seeded);
    let def = digest(&[("all", MissedFirePolicy::CatchUp(10))]);

    let (a, b) = (
        deployment.instance("replica-a", &def).await,
        deployment.instance("replica-b", &def).await,
    );
    let (started_a, started_b) = tokio::join!(a.start(), b.start());
    started_a.unwrap();
    started_b.unwrap();
    let fired = fired_and_settled(&deployment, missed.len()).await;
    // Both replicas planned the same gap; the second delivery of each key started nothing.
    tokio::time::sleep(Duration::from_millis(1_000)).await;
    let (_, _) = tokio::join!(a.stop(), b.stop());
    assert_eq!(fired.get("all"), Some(&missed), "every missed tick, once");
    assert_eq!(deployment.fired().await, fired);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn each_tenant_keeps_its_own_watermarks() {
    // Tenant A was down three years; tenant B's definition is new. One definition, two
    // tenants, each with its own watermark store.
    let a = Deployment::new("tenant-a", Arc::new(InMemoryCronWatermarkStore::new()));
    let b = Deployment::new("tenant-b", Arc::new(InMemoryCronWatermarkStore::new()));
    a.watermarks.advance(YEARLY_SOURCE, three_years_ago()).await.unwrap();
    let def = digest(&[("once", MissedFirePolicy::FireOnce)]);
    let kernels = ManagedKernels::new();
    kernels.insert(a.instance("a", &def).await).unwrap();
    kernels.insert(b.instance("b", &def).await).unwrap();
    kernels.start().await.unwrap();

    fired_and_settled(&a, 1).await;
    tokio::time::sleep(Duration::from_millis(1_000)).await;
    kernels.stop().await.unwrap();
    assert_eq!(a.fired().await.get("once").map(Vec::len), Some(1));
    assert!(b.fired().await.is_empty(), "a new source replays nothing from before it existed");
    let b_cursor = b.watermarks.read(YEARLY_SOURCE).await.unwrap();
    assert!(
        b_cursor.is_some_and(|cursor| cursor > Utc::now() - chrono::Duration::minutes(1)),
        "tenant B's cursor starts at its first watch: {b_cursor:?}"
    );

    // A tenant on another tenant's watermark store is refused.
    let shared = TenantKernel::builder(
        "tenant-c",
        ManagedStores::in_memory(),
        Arc::new(InMemorySessionService::new()) as Arc<dyn SessionService>,
    )
    .watermark_store(Arc::clone(&a.watermarks))
    .build()
    .unwrap();
    let error = kernels.insert(shared).unwrap_err();
    assert!(error.to_string().contains("cron watermark store"), "{error}");
}
