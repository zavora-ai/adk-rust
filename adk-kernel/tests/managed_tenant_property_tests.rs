//! No wake, process or approval crosses tenants (spec 3.2 §7.13, §10 M1, §11), under random
//! sequences of webhook deliveries, approvals and approvals attempted on another tenant's
//! request, over two or three tenant kernels that share one session service and one
//! definition name, each with its own stores. Each case runs real kernels: workers,
//! triggers, the tenants' approval services and the managed runtimes that host the
//! definitions.

#![cfg(feature = "managed")]

mod managed_support;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use adk_core::{ApprovalDecision, ApprovalState, InboxQuery, RunRecord, RunState, SubjectRef};
use adk_kernel::managed::{KernelDef, ManagedKernels, WakeOverlap, WakeSetDef};
use adk_kernel::{KERNEL_WAKE_TARGET, ProcessFilter, Wake, WakeEvent};
use adk_managed::ManagedAgentRuntime;
use adk_managed::types::{ManagedAgentDef, ModelRef};
use adk_session::{InMemorySessionService, ListRequest, SessionService};
use managed_support::*;
use proptest::prelude::*;
use serde_json::json;

const HOOKS: [&str; 3] = ["orders", "alerts", "deploys"];
const TARGET: &str = "ops";

#[derive(Debug, Clone)]
enum Op {
    /// Delivers hook `hook`'s message `id` to tenant `tenant`.
    Deliver {
        tenant: usize,
        hook: usize,
        id: u8,
    },
    /// Approves one of the tenant's pending requests.
    Approve {
        tenant: usize,
        pick: usize,
    },
    /// Decides one of `from`'s pending requests through tenant `to`'s approval service.
    CrossApprove {
        from: usize,
        to: usize,
        pick: usize,
    },
    Settle,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => (0..3usize, 0..HOOKS.len(), 0..4u8)
            .prop_map(|(tenant, hook, id)| Op::Deliver { tenant, hook, id }),
        2 => (0..3usize, any::<usize>()).prop_map(|(tenant, pick)| Op::Approve { tenant, pick }),
        2 => (0..3usize, 0..3usize, any::<usize>())
            .prop_map(|(from, to, pick)| Op::CrossApprove { from, to, pick }),
        1 => Just(Op::Settle),
    ]
}

/// One tenant of a case: which hooks its definition wakes on, and whether its tool is held
/// for approval.
#[derive(Debug, Clone)]
struct Spec {
    hooks: BTreeSet<usize>,
    confirm: bool,
}

fn spec() -> impl Strategy<Value = Spec> {
    (prop::collection::btree_set(0..HOOKS.len(), 0..=HOOKS.len()), any::<bool>())
        .prop_map(|(hooks, confirm)| Spec { hooks, confirm })
}

fn operator() -> InboxQuery {
    InboxQuery::for_approver(SubjectRef::human("ops"), [OPERATOR])
}

fn approval() -> ApprovalDecision {
    ApprovalDecision::approve(SubjectRef::human("ops")).with_groups([OPERATOR])
}

async fn pending(tenant: &Tenant) -> Vec<adk_core::ApprovalRequest> {
    tenant.service.inbox(operator()).await.unwrap().items
}

/// Waits until no tenant has a run `Running` and no run changed state for 150 ms.
async fn quiesce(tenants: &[Tenant]) {
    let snapshot = || async {
        let mut all = Vec::new();
        for tenant in tenants {
            for run in tenant.all_runs().await {
                all.push((run.run_id, format!("{:?}", run.state)));
            }
        }
        all.sort();
        all
    };
    let started = tokio::time::Instant::now();
    let mut last = snapshot().await;
    let mut stable_since = tokio::time::Instant::now();
    while started.elapsed() < Duration::from_secs(10) {
        tokio::time::sleep(Duration::from_millis(30)).await;
        let now = snapshot().await;
        let running = now.iter().any(|(_, state)| state == "Running");
        if now != last || running {
            last = now;
            stable_since = tokio::time::Instant::now();
        } else if stable_since.elapsed() >= Duration::from_millis(150) {
            return;
        }
    }
    panic!("the tenants did not settle: {last:?}");
}

/// `(hook, id)` of a trigger run's key `hook:<hook>:<id>#ops/<hook>`, or of a `kernel.wake`
/// run's key `hook:<hook>:<id>`.
fn delivery_of(run: &RunRecord) -> (String, String) {
    let key = run.idempotency_key.clone().unwrap_or_default();
    let event = key.split('#').next().unwrap_or_default();
    let mut parts = event.splitn(3, ':');
    assert_eq!(parts.next(), Some("hook"), "{key}");
    (parts.next().unwrap_or_default().to_string(), parts.next().unwrap_or_default().to_string())
}

async fn check(specs: Vec<Spec>, ops: Vec<Op>) {
    let sessions = Arc::new(InMemorySessionService::new());
    let tenants: Vec<Tenant> = specs
        .iter()
        .enumerate()
        .map(|(i, spec)| Tenant::new(&format!("tenant-{i}"), sessions.clone(), spec.confirm))
        .collect();
    let kernels = ManagedKernels::new();
    for (tenant, spec) in tenants.iter().zip(&specs) {
        let mut kernel = KernelDef::new();
        for hook in &spec.hooks {
            let hook = HOOKS[*hook];
            kernel = kernel.with_wake_set(
                WakeSetDef::new(hook, [Wake::Webhook { hook: hook.into() }], "Handle it.")
                    .with_overlap(WakeOverlap::Queue),
            );
        }
        let def = kernel.apply_to(ManagedAgentDef::new(
            TARGET,
            ModelRef::Shorthand("gemini-3.5-flash-lite".into()),
        ));
        tenant.runtime.create(def).await.unwrap();
        kernels.insert(tenant.kernel.clone()).unwrap();
    }
    kernels.start().await.unwrap();

    let mut delivered: BTreeSet<(usize, String, String)> = BTreeSet::new();
    for op in ops {
        match op {
            Op::Deliver { tenant, hook, id } => {
                let tenant = tenant % tenants.len();
                let hook = HOOKS[hook];
                let event = WakeEvent::new(
                    format!("hook:{hook}"),
                    format!("hook:{hook}:{id}"),
                    json!({ "id": id }),
                );
                let kernel = tenants[tenant].kernel.kernel().await.unwrap();
                kernel.wake_router().submit(&event, &tenants[tenant].name).await.unwrap();
                delivered.insert((tenant, hook.to_string(), id.to_string()));
            }
            Op::Approve { tenant, pick } => {
                let tenant = &tenants[tenant % tenants.len()];
                let open = pending(tenant).await;
                if let Some(request) = open.get(pick % open.len().max(1)) {
                    tenant.service.decide(&request.id, request.version, approval()).await.unwrap();
                }
            }
            Op::CrossApprove { from, to, pick } => {
                let (from, to) = (from % tenants.len(), to % tenants.len());
                if from == to {
                    continue;
                }
                let open = pending(&tenants[from]).await;
                if let Some(request) = open.get(pick % open.len().max(1)) {
                    let crossed =
                        tenants[to].service.decide(&request.id, request.version, approval()).await;
                    assert!(
                        crossed.is_err(),
                        "tenant {to}'s approval service decided tenant {from}'s request"
                    );
                    let still = tenants[from].service.get(&request.id).await.unwrap().unwrap();
                    assert_eq!(still.state, ApprovalState::Pending, "the request stayed pending");
                }
            }
            Op::Settle => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    }
    quiesce(&tenants).await;

    for (i, (tenant, spec)) in tenants.iter().zip(&specs).enumerate() {
        let runs = tenant.all_runs().await;
        let ids: BTreeSet<&str> = runs.iter().map(|run| run.run_id.as_str()).collect();
        let mut completed = 0;
        for run in &runs {
            assert_eq!(run.app_name, tenant.name, "a run of another tenant: {run:?}");
            let key = run.idempotency_key.as_deref().unwrap_or_default();
            if let Some(finished) = key.strip_prefix("run:") {
                // The kernel's run-finished wake of one of this tenant's own processes.
                assert_eq!(run.target, KERNEL_WAKE_TARGET);
                let pid = finished.split(':').next().unwrap_or_default();
                assert!(
                    ids.contains(pid),
                    "{}: a run-finished wake of another tenant",
                    tenant.name
                );
                continue;
            }
            let (hook, id) = delivery_of(run);
            assert!(
                delivered.contains(&(i, hook.clone(), id.clone())),
                "{} ran {hook}:{id}, delivered only to another tenant",
                tenant.name
            );
            if run.target == KERNEL_WAKE_TARGET {
                continue;
            }
            assert_eq!(run.target, TARGET);
            let hook_index = HOOKS.iter().position(|name| *name == hook).unwrap();
            assert!(spec.hooks.contains(&hook_index), "{} has no wake on {hook}", tenant.name);
            if run.state == RunState::Completed {
                completed += 1;
            }
        }
        // Every tool call ran in a process of this tenant: one per completed trigger run.
        assert_eq!(tenant.tool_calls(), completed, "{}: tool calls", tenant.name);
        for meta in tenant.kernel.process_store().list(&ProcessFilter::default()).await.unwrap() {
            assert!(
                ids.contains(meta.pid.as_str()),
                "{}: a process of another tenant",
                tenant.name
            );
        }
        let mut query = operator();
        query.states = ApprovalState::ALL.to_vec();
        for request in tenant.service.inbox(query).await.unwrap().items {
            assert_eq!(request.scope.app_name, tenant.name, "an approval of another tenant");
            let run = request.run_id.expect("a held process's request names its run");
            assert!(
                ids.contains(run.as_str()),
                "{}: an approval for another tenant's run",
                tenant.name
            );
        }
        let own = sessions
            .list(ListRequest {
                app_name: tenant.name.clone(),
                user_id: "kernel".into(),
                limit: None,
                offset: None,
            })
            .await
            .unwrap();
        for session in own {
            let run = session.id().strip_prefix("run-").expect("a process's session");
            assert!(ids.contains(run), "{}: a session of another tenant's run", tenant.name);
        }
    }
    kernels.stop().await.unwrap();
}

fn run(specs: Vec<Spec>, ops: Vec<Op>) {
    let runtime =
        tokio::runtime::Builder::new_multi_thread().worker_threads(4).enable_all().build().unwrap();
    runtime.block_on(check(specs, ops));
}

proptest! {
    // `PROPTEST_CASES` raises the count for a soak run.
    #![proptest_config(ProptestConfig {
        cases: std::env::var("PROPTEST_CASES")
            .ok()
            .and_then(|cases| cases.parse().ok())
            .unwrap_or(100),
        ..ProptestConfig::default()
    })]

    #[test]
    fn no_wake_process_or_approval_crosses_tenants(
        specs in prop::collection::vec(spec(), 2..=3),
        ops in prop::collection::vec(op(), 1..10),
    ) {
        run(specs, ops);
    }
}

/// Every tenant wakes on the same hook and holds its call; one tenant's request is decided
/// through another tenant's service, then through its own.
#[test]
fn the_same_hook_on_every_tenant_stays_with_the_tenant_it_was_delivered_to() {
    let every = Spec { hooks: BTreeSet::from([0]), confirm: true };
    run(
        vec![every.clone(), every.clone(), every],
        vec![
            Op::Deliver { tenant: 0, hook: 0, id: 1 },
            Op::Deliver { tenant: 1, hook: 0, id: 1 },
            Op::Settle,
            Op::Settle,
            Op::CrossApprove { from: 0, to: 1, pick: 0 },
            Op::Approve { tenant: 0, pick: 0 },
        ],
    );
}
