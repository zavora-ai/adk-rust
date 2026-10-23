//! The managed form (spec 3.2 §7.13, §10 M1): a harness definition with a `KernelDef`
//! beside it, hosted on its tenant's kernel by the managed runtime. Scripted on
//! `ScriptedLlm` with two tenants, and live with one tenant on a real model; both approve a
//! held call through the tenant's approval service, the managed approval path.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use adk_core::{
    ActionLedger, ActionOutcome, ActionRecord, ApprovalDecision, ApprovalPolicy, ApprovalService,
    ApproverSelector, DurableRunStore, InMemoryActionLedger, InMemoryApprovalStore,
    InMemoryRunStore, InboxQuery, Llm, RunFilter, RunState, StoreApprovalService, SubjectRef,
};
use adk_harness::HarnessMode;
use adk_harness::packs::managed::{HarnessDef, HarnessResolver, ShellPolicy};
use adk_kernel::managed::{KernelDef, ManagedKernels, TenantKernel, WakeSetDef};
use adk_kernel::{KernelConfig, Wake, WakeEvent};
use adk_managed::resolver::{DefaultModelResolver, ModelResolver, ResolverResult};
use adk_managed::types::{ManagedAgentDef, ModelRef};
use adk_managed::{
    FileManagedStateStore, ManagedAgentRuntime, ManagedOwner, ManagedStateStore, ManagedStores,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_session::InMemorySessionService;
use anyhow::{Result, bail};
use async_trait::async_trait;
use serde_json::json;

use crate::Checks;

/// The scripted scenarios of this module, and what `main` does instead.
pub const SCRIPTED: &[(&str, &str)] = &[(
    "m1_one_kernel_per_tenant",
    "a definition's `adk.kernel` block is carried and ignored: the managed runtime runs sessions, and nothing wakes a tenant's agent",
)];

/// The live scenarios of this module.
pub const LIVE: &[&str] = &["live_managed_tenant_approval"];

const OPERATOR: &str = "operator";

/// An action ledger that counts the writes it records.
#[derive(Debug, Default)]
struct WriteLedger {
    inner: InMemoryActionLedger,
    writes: AtomicUsize,
}

#[async_trait]
impl ActionLedger for WriteLedger {
    async fn begin(&self, record: &ActionRecord) -> adk_core::Result<()> {
        if record.tool_name == "write_file" {
            self.writes.fetch_add(1, Ordering::SeqCst);
        }
        self.inner.begin(record).await
    }

    async fn complete(&self, key: &str, outcome: ActionOutcome) -> adk_core::Result<()> {
        self.inner.complete(key, outcome).await
    }

    async fn get(&self, key: &str) -> adk_core::Result<Option<ActionRecord>> {
        self.inner.get(key).await
    }
}

/// Resolves every model reference to one scripted model.
struct Scripted(Arc<ScriptedLlm>);

#[async_trait]
impl ModelResolver for Scripted {
    async fn resolve(&self, _model: &ModelRef) -> ResolverResult<Arc<dyn Llm>> {
        Ok(Arc::clone(&self.0) as Arc<dyn Llm>)
    }
}

/// One tenant: its own stores and approval service, its kernel, its managed runtime.
struct Tenant {
    name: String,
    runs: Arc<dyn DurableRunStore>,
    service: Arc<dyn ApprovalService>,
    kernel: TenantKernel,
    runtime: adk_managed::DefaultManagedAgentRuntime,
    ledger: Arc<WriteLedger>,
    workspaces: tempfile::TempDir,
}

fn tenant(
    name: &str,
    state: Arc<dyn ManagedStateStore>,
    models: Arc<dyn ModelResolver>,
) -> Result<Tenant> {
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let decisions = Arc::new(InMemoryApprovalStore::new());
    let service: Arc<dyn ApprovalService> = Arc::new(
        StoreApprovalService::new(decisions.clone()).with_run_store(Arc::clone(&runs)).with_router(
            ApprovalPolicy::new([ApproverSelector::group(OPERATOR)]).allow_self_approval(),
        ),
    );
    let stores = ManagedStores::new(state, Arc::clone(&runs), decisions)
        .with_approval_service(Arc::clone(&service));
    let workspaces = tempfile::tempdir()?;
    let ledger = Arc::new(WriteLedger::default());
    let kernel = TenantKernel::builder(name, stores, Arc::new(InMemorySessionService::new()))
        .config(KernelConfig::default().with_scheduler_tick(Duration::from_millis(250)))
        .resolver(Arc::new(HarnessResolver::new(workspaces.path())))
        .action_ledger(Arc::clone(&ledger) as Arc<dyn ActionLedger>)
        .build()?;
    let runtime = kernel.runtime(models);
    Ok(Tenant { name: name.into(), runs, service, kernel, runtime, ledger, workspaces })
}

/// A harness in Ask mode with no shell that wakes on the `orders` webhook.
fn triage(model: &str, system: Option<&str>) -> ManagedAgentDef {
    let mut harness = HarnessDef::new("triage", ModelRef::Shorthand(model.into()))
        .with_mode(HarnessMode::Ask)
        .with_shell(ShellPolicy::none());
    if let Some(system) = system {
        harness = harness.with_system(system);
    }
    KernelDef::new()
        .with_wake_set(WakeSetDef::new(
            "orders",
            [Wake::Webhook { hook: "orders".into() }],
            "A new order arrived: order 7, two chairs. Write a one-line summary of it to \
             order.txt in the workspace with the write_file tool.",
        ))
        .apply_to(harness.into())
}

fn operator() -> InboxQuery {
    InboxQuery::for_approver(SubjectRef::human("ops"), [OPERATOR])
}

/// Waits until `probe` answers, polling every 100 ms, for at most `limit`.
async fn within<T, F, Fut>(limit: Duration, mut probe: F) -> Option<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let deadline = tokio::time::Instant::now() + limit;
    while tokio::time::Instant::now() < deadline {
        if let Some(value) = probe().await {
            return Some(value);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    None
}

/// Delivers order 7 to `tenant`, waits for the held write in its inbox, approves it through
/// the tenant's approval service, and checks the write ran once.
async fn approve_the_held_write(
    tenant: &Tenant,
    limit: Duration,
    checks: &mut Checks,
) -> Result<()> {
    let kernel = tenant.kernel.kernel().await.expect("the tenant kernel runs");
    let delivery = WakeEvent::new("hook:orders", "hook:orders:order-7", json!({ "order": 7 }));
    kernel.wake_router().submit(&delivery, &tenant.name).await?;
    let held = within(limit, || async {
        tenant
            .service
            .inbox(operator())
            .await
            .ok()?
            .items
            .into_iter()
            .find(|r| r.tool_name == "write_file")
    })
    .await;
    checks.check(
        "the webhook started a process whose write was held in the tenant's inbox",
        held.is_some(),
    );
    let Some(request) = held else { return Ok(()) };
    let pid = request.run_id.clone().unwrap_or_default();
    let parked = tenant.runs.get(&pid).await?.is_some_and(|run| run.state == RunState::Waiting);
    checks.check("the process parked on the tenant's run store", parked);
    let order = tenant.workspaces.path().join("triage").join("order.txt");
    checks.check("nothing was written before the approval", !order.exists());
    tenant
        .service
        .decide(
            &request.id,
            request.version,
            ApprovalDecision::approve(SubjectRef::human("ops")).with_groups([OPERATOR]),
        )
        .await?;
    let completed = within(limit, || async {
        tenant.runs.get(&pid).await.ok()?.filter(|run| run.state == RunState::Completed)
    })
    .await;
    checks.check("the approval woke the process and it completed", completed.is_some());
    let written = std::fs::read_to_string(&order).unwrap_or_default();
    println!("    order.txt: {}", adk_harness::escape_control(written.trim()));
    checks.check("order.txt was written", !written.trim().is_empty());
    checks.check(
        "the write ran once under the action ledger",
        tenant.ledger.writes.load(Ordering::SeqCst) == 1,
    );
    Ok(())
}

pub async fn run_scripted(name: &str) -> Option<Result<Checks>> {
    match name {
        "m1_one_kernel_per_tenant" => Some(one_kernel_per_tenant().await),
        _ => None,
    }
}

/// Two tenants, one definition name: the block survives the state store, a delivery to
/// tenant A runs on A's stores only, and A's held write runs once after A's approval.
async fn one_kernel_per_tenant() -> Result<Checks> {
    let mut checks = Checks::default();
    let state_dir = tempfile::tempdir()?;
    let model = Arc::new(ScriptedLlm::new(
        "scripted",
        vec![
            ScriptedResponse::function_call(
                "write_file",
                json!({ "path": "order.txt", "content": "order 7: two chairs" }),
            ),
            ScriptedResponse::text("summarized"),
        ],
    ));
    let a = tenant(
        "tenant-a",
        Arc::new(FileManagedStateStore::new(state_dir.path())),
        Arc::new(Scripted(model.clone())),
    )?;
    let idle = Arc::new(ScriptedLlm::new("scripted", Vec::<ScriptedResponse>::new()));
    let b = tenant(
        "tenant-b",
        Arc::new(adk_managed::InMemoryManagedStateStore::new()),
        Arc::new(Scripted(idle.clone())),
    )?;
    let def = triage("scripted", None);
    let agent = a.runtime.create(def.clone()).await?;
    b.runtime.create(def.clone()).await?;
    let session =
        a.runtime.start_session(&agent, &ManagedOwner::new("tenant-a", "ana")?, None).await?;
    let stored = FileManagedStateStore::new(state_dir.path()).load(&session.0).await?;
    let kept = stored.and_then(|state| state.record).is_some_and(|record| {
        serde_json::to_value(&record.agent).ok() == serde_json::to_value(&def).ok()
    });
    checks.check(
        "the definition with its kernel block round-tripped through the state store unchanged",
        kept,
    );

    let kernels = ManagedKernels::new();
    kernels.insert(a.kernel.clone())?;
    kernels.insert(b.kernel.clone())?;
    let shared = TenantKernel::builder(
        "tenant-c",
        a.kernel.stores().clone(),
        Arc::new(InMemorySessionService::new()),
    )
    .build()?;
    checks.check("a tenant on another tenant's stores is refused", kernels.insert(shared).is_err());
    kernels.start().await?;
    approve_the_held_write(&a, Duration::from_secs(10), &mut checks).await?;
    let b_runs = b.runs.list(RunFilter::default()).await?;
    checks.check(
        "tenant B's stores hold no run and its model made no call",
        b_runs.is_empty() && idle.requests().is_empty(),
    );
    let a_runs = a.runs.list(RunFilter::default()).await?;
    checks.check(
        "every run on A's stores is A's",
        a_runs.iter().all(|run| run.app_name == "tenant-a"),
    );
    kernels.stop().await?;
    Ok(checks)
}

pub async fn run_live(name: &str, provider: &str, model: &str) -> Option<Result<Checks>> {
    match name {
        "live_managed_tenant_approval" => Some(live_tenant(provider, model).await),
        _ => None,
    }
}

/// One tenant on a real model: the webhook wakes the harness, its write is held, the tenant's
/// approval service approves it, and it runs once.
async fn live_tenant(provider: &str, model: &str) -> Result<Checks> {
    let mut checks = Checks::default();
    let models: Arc<dyn ModelResolver> = Arc::new(DefaultModelResolver::new());
    if models.resolve(&ModelRef::Shorthand(model.into())).await.is_err() {
        bail!("{provider}'s model {model} cannot be built: is its key set?");
    }
    let tenant =
        tenant("tenant-a", Arc::new(adk_managed::InMemoryManagedStateStore::new()), models)?;
    tenant
        .runtime
        .create(triage(
            model,
            Some("You triage orders. Use write_file for the summary; keep it to one line."),
        ))
        .await?;
    tenant.kernel.start().await?;
    approve_the_held_write(&tenant, Duration::from_secs(120), &mut checks).await?;
    tenant.kernel.stop().await?;
    Ok(checks)
}
