//! A harness definition on a tenant kernel (spec 3.2 §7.13, §10 M1): a `HarnessDef` with a
//! `KernelDef` beside it, resolved by the tenant's `KernelResolver` over `HarnessResolver`.
//! A webhook wake starts a governed process of the harness; its held write parks on the
//! tenant's run store, the tenant's approval service decides it, and the write runs once
//! under the action ledger. The harness's follow-ups land on the tenant kernel's agenda. On
//! `origin/main` a definition's `adk.kernel` block is not read by anything.

#![cfg(all(feature = "managed", feature = "kernel"))]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use adk_core::{
    ActionLedger, ActionOutcome, ActionRecord, ApprovalDecision, ApprovalPolicy, ApprovalService,
    ApproverSelector, DurableRunStore, InMemoryActionLedger, InMemoryApprovalStore,
    InMemoryRunStore, InboxQuery, Llm, RunState, StoreApprovalService, SubjectRef,
};
use adk_harness::HarnessMode;
use adk_harness::packs::managed::{HarnessDef, HarnessResolver, ShellPolicy};
use adk_kernel::managed::{KernelDef, TenantKernel, WakeSetDef};
use adk_kernel::{AgendaFilter, AgendaHandle, KernelConfig, Wake, WakeEvent};
use adk_managed::resolver::{ModelResolver, ResolverResult};
use adk_managed::types::{ManagedAgentDef, ModelRef};
use adk_managed::{InMemoryManagedStateStore, ManagedAgentRuntime, ManagedStores};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_server::background::DurableWorkerConfig;
use adk_session::InMemorySessionService;
use async_trait::async_trait;
use serde_json::json;

const TENANT: &str = "tenant-a";
const OPERATOR: &str = "operator";

struct Models(Arc<ScriptedLlm>);

#[async_trait]
impl ModelResolver for Models {
    async fn resolve(&self, _model: &ModelRef) -> ResolverResult<Arc<dyn Llm>> {
        Ok(Arc::clone(&self.0) as Arc<dyn Llm>)
    }
}

/// An action ledger that counts the writes it records.
#[derive(Debug, Default)]
struct CountingLedger {
    inner: InMemoryActionLedger,
    writes: std::sync::Mutex<Vec<String>>,
    begun: AtomicUsize,
    completed: AtomicUsize,
}

#[async_trait]
impl ActionLedger for CountingLedger {
    async fn begin(&self, record: &ActionRecord) -> adk_core::Result<()> {
        if record.tool_name == "write_file" {
            self.begun.fetch_add(1, Ordering::SeqCst);
            self.writes.lock().unwrap().push(record.key.clone());
        }
        self.inner.begin(record).await
    }

    async fn complete(&self, key: &str, outcome: ActionOutcome) -> adk_core::Result<()> {
        if self.writes.lock().unwrap().iter().any(|write| write == key) {
            self.completed.fetch_add(1, Ordering::SeqCst);
        }
        self.inner.complete(key, outcome).await
    }

    async fn get(&self, key: &str) -> adk_core::Result<Option<ActionRecord>> {
        self.inner.get(key).await
    }
}

async fn eventually<T, F, Fut>(what: &str, mut probe: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let started = tokio::time::Instant::now();
    while started.elapsed() <= Duration::from_secs(10) {
        if let Some(value) = probe().await {
            return value;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {what}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_harness_definition_on_a_tenant_kernel_runs_its_held_write_once_after_approval() {
    let workspaces = tempfile::tempdir().unwrap();
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let decisions = Arc::new(InMemoryApprovalStore::new());
    let service: Arc<dyn ApprovalService> = Arc::new(
        StoreApprovalService::new(decisions.clone()).with_run_store(Arc::clone(&runs)).with_router(
            ApprovalPolicy::new([ApproverSelector::group(OPERATOR)]).allow_self_approval(),
        ),
    );
    let stores = ManagedStores::new(
        Arc::new(InMemoryManagedStateStore::new()),
        Arc::clone(&runs),
        decisions,
    )
    .with_approval_service(Arc::clone(&service));
    let model = Arc::new(ScriptedLlm::new(
        "scripted",
        vec![
            ScriptedResponse::function_call(
                "propose_followup",
                json!({ "title": "Re-check the order", "rationale": "the supplier answers tomorrow" }),
            ),
            ScriptedResponse::function_call(
                "write_file",
                json!({ "path": "order.txt", "content": "triaged" }),
            ),
            ScriptedResponse::text("triaged"),
        ],
    ));
    // The operator's resolver: harnesses with the tenant's agenda; the definition's mode.
    let agenda = AgendaHandle::new();
    let resolver = HarnessResolver::new(workspaces.path()).configure({
        let agenda = agenda.clone();
        move |builder, _def| builder.agenda_handle(agenda.clone())
    });
    let ledger = Arc::new(CountingLedger::default());
    let tenant = TenantKernel::builder(TENANT, stores, Arc::new(InMemorySessionService::new()))
        .config(
            KernelConfig::default().with_worker(
                DurableWorkerConfig::default()
                    .with_worker_id("tenant-a")
                    .with_lease_ttl(Duration::from_millis(600))
                    .with_poll_interval(Duration::from_millis(20)),
            ),
        )
        .resolver(Arc::new(resolver))
        .action_ledger(Arc::clone(&ledger) as Arc<dyn ActionLedger>)
        .agenda_handle(agenda)
        .build()
        .unwrap();
    let runtime = tenant.runtime(Arc::new(Models(Arc::clone(&model))));

    let harness: ManagedAgentDef =
        HarnessDef::new("triage", ModelRef::Shorthand("scripted".into()))
            .with_mode(HarnessMode::Ask)
            .with_shell(ShellPolicy::none())
            .into();
    let def = KernelDef::new()
        .with_wake_set(WakeSetDef::new(
            "orders",
            [Wake::Webhook { hook: "orders".into() }],
            "Triage the new order into order.txt.",
        ))
        .apply_to(harness);
    runtime.create(def).await.unwrap();
    tenant.start().await.unwrap();

    let kernel = tenant.kernel().await.unwrap();
    let delivery = WakeEvent::new("hook:orders", "hook:orders:evt-1", json!({ "order": 7 }));
    kernel.wake_router().submit(&delivery, TENANT).await.unwrap();
    let operator = || InboxQuery::for_approver(SubjectRef::human("ops"), [OPERATOR]);
    let request = eventually("the held write in the tenant's inbox", || async {
        service
            .inbox(operator())
            .await
            .unwrap()
            .items
            .into_iter()
            .find(|request| request.tool_name == "write_file")
    })
    .await;
    let pid = request.run_id.clone().expect("the request names the parked process");
    eventually("the process parked", || async {
        runs.get(&pid).await.unwrap().filter(|run| run.state == RunState::Waiting)
    })
    .await;
    let order = workspaces.path().join("triage").join("order.txt");
    assert!(!order.exists(), "held, not written");

    // The harness's follow-up is on the tenant kernel's agenda.
    let items = tenant.agenda_handle().get().unwrap().list(&AgendaFilter::default()).await.unwrap();
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0].title, "Re-check the order");

    service
        .decide(
            &request.id,
            request.version,
            ApprovalDecision::approve(SubjectRef::human("ops")).with_groups([OPERATOR]),
        )
        .await
        .unwrap();
    eventually("the process to complete", || async {
        runs.get(&pid).await.unwrap().filter(|run| run.state == RunState::Completed)
    })
    .await;
    assert_eq!(std::fs::read_to_string(&order).unwrap(), "triaged");
    assert_eq!(
        (ledger.begun.load(Ordering::SeqCst), ledger.completed.load(Ordering::SeqCst)),
        (1, 1),
        "the write ran once under the action ledger"
    );

    // A restart of the tenant's kernel runs nothing again.
    tenant.stop().await.unwrap();
    tenant.start().await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    tenant.stop().await.unwrap();
    assert_eq!(ledger.begun.load(Ordering::SeqCst), 1);
    assert_eq!(model.remaining(), 0);
}
