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
pub const SCRIPTED: &[(&str, &str)] = &[
    (
        "m1_one_kernel_per_tenant",
        "a definition's `adk.kernel` block is carried and ignored: the managed runtime runs sessions, and nothing wakes a tenant's agent",
    ),
    (
        "m2_a_tenants_drivers",
        "a definition's driver manifests are recorded and mounted nowhere: nothing asks a person, and no harness of any tenant is offered the driver",
    ),
    (
        "m3_missed_ticks_replay_once",
        "a tenant kernel's cron source keeps no cursor: a tick missed while it was down never fires, whatever the wake set's `missed` says",
    ),
];

/// The live scenarios of this module.
pub const LIVE: &[&str] =
    &["live_managed_tenant_approval", "live_managed_tenant_drivers", "live_tenant_missed_ticks"];

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
        "m2_a_tenants_drivers" => Some(tenant_drivers().await),
        "m3_missed_ticks_replay_once" => Some(missed_ticks().await),
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
        "live_managed_tenant_drivers" => Some(live_tenant_drivers(provider, model).await),
        "live_tenant_missed_ticks" => Some(live_missed_ticks(provider, model).await),
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

// ─── Drivers and cron watermarks in the managed form (spec 3.2 §7.4, §7.7, §7.8) ─────────

/// One tenant with its own stores, approval service, driver supervisor and watermarks, its
/// harnesses built per partition by `HarnessResolver`.
struct DriverTenant {
    name: String,
    runs: Arc<dyn DurableRunStore>,
    service: Arc<dyn ApprovalService>,
    sessions: Arc<InMemorySessionService>,
    drivers: adk_kernel::drivers::DriverSupervisor,
    kernel: TenantKernel,
    runtime: adk_managed::DefaultManagedAgentRuntime,
    _workspaces: tempfile::TempDir,
}

fn driver_tenant(
    name: &str,
    home: &std::path::Path,
    models: Arc<dyn ModelResolver>,
    watermarks: Arc<dyn adk_kernel::CronWatermarkStore>,
) -> Result<DriverTenant> {
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let decisions = Arc::new(InMemoryApprovalStore::new());
    let service: Arc<dyn ApprovalService> = Arc::new(
        StoreApprovalService::new(decisions.clone())
            .with_run_store(Arc::clone(&runs))
            .with_router(ApprovalPolicy::new([ApproverSelector::group(OPERATOR)])),
    );
    let stores = ManagedStores::new(
        Arc::new(adk_managed::InMemoryManagedStateStore::new()),
        Arc::clone(&runs),
        decisions,
    )
    .with_approval_service(Arc::clone(&service));
    let drivers = adk_kernel::drivers::DriverSupervisor::builder(Arc::new(
        adk_kernel::stores::InMemoryDriverStore::new(),
    ))
    .build();
    let workspaces = tempfile::tempdir()?;
    let resolver = Arc::new(HarnessResolver::new(workspaces.path()));
    let sessions = Arc::new(InMemorySessionService::new());
    let kernel = TenantKernel::builder(name, stores, sessions.clone())
        .config(
            KernelConfig::default().with_home(home).with_scheduler_tick(Duration::from_millis(250)),
        )
        .resolver(resolver.clone())
        .partition_resolver(resolver)
        .drivers(drivers.clone())
        .watermark_store(watermarks)
        .build()?;
    let runtime = kernel.runtime(models);
    Ok(DriverTenant {
        name: name.into(),
        runs,
        service,
        sessions,
        drivers,
        kernel,
        runtime,
        _workspaces: workspaces,
    })
}

/// The notes driver, serving work.
fn notes_for_work(dir: &std::path::Path) -> Result<adk_kernel::drivers::DriverManifest> {
    static INSTANCES: AtomicUsize = AtomicUsize::new(0);
    let instance =
        format!("managed-{}-{}", std::process::id(), INSTANCES.fetch_add(1, Ordering::SeqCst));
    let toml = crate::notes_driver::manifest(&std::env::current_exe()?, dir, &instance)
        .replace("sandbox = \"os\"\n", "sandbox = \"os\"\npartitions = [\"work\"]\n");
    Ok(adk_kernel::drivers::DriverManifest::from_toml(&toml)?)
}

/// Home, the coordinator, and work, which mounts the notes driver; a webhook wake in each.
fn assistant(model: &str, notes: adk_kernel::drivers::DriverManifest) -> ManagedAgentDef {
    use adk_kernel::managed::PartitionDef;
    let harness: ManagedAgentDef = HarnessDef::new("assistant", ModelRef::Shorthand(model.into()))
        .with_mode(HarnessMode::Ask)
        .with_shell(ShellPolicy::none())
        .into();
    let open = |id: &str| {
        let mut partition = PartitionDef::new(id, format!("{id}-agents"));
        partition.cross_partition = "coordinator_with_approval".into();
        partition
    };
    KernelDef::new()
        .with_partition(open("home"))
        .with_partition(open("work").with_driver("notes"))
        .with_coordinator("home")
        .with_driver(notes)
        .with_wake_set(
            WakeSetDef::new(
                "standup",
                [Wake::Webhook { hook: "standup".into() }],
                "Call the read_note tool once and reply with the codeword it returns, nothing else.",
            )
            .with_partition("work"),
        )
        .with_wake_set(
            WakeSetDef::new(
                "morning",
                [Wake::Webhook { hook: "morning".into() }],
                "Call the work__read_note tool once and reply with the codeword it returns, nothing else.",
            )
            .with_partition("home"),
        )
        .apply_to(harness)
}

impl DriverTenant {
    async fn inbox(&self) -> Vec<adk_core::ApprovalRequest> {
        self.service.inbox(operator()).await.map(|page| page.items).unwrap_or_default()
    }

    async fn approve(&self, request: &adk_core::ApprovalRequest) -> Result<()> {
        let decision = ApprovalDecision::approve(SubjectRef::human("ops")).with_groups([OPERATOR]);
        self.service.decide(&request.id, request.version, decision).await?;
        Ok(())
    }

    /// Delivers webhook `hook` and returns the run it started for `target`.
    async fn wake(&self, hook: &str, target: &str, limit: Duration) -> Result<Option<String>> {
        let kernel = self.kernel.kernel().await.expect("the tenant kernel runs");
        let delivery = WakeEvent::new(format!("hook:{hook}"), format!("hook:{hook}:1"), json!({}));
        kernel.wake_router().submit(&delivery, &self.name).await?;
        Ok(within(limit, || async {
            let runs = self.runs.list(RunFilter::default()).await.ok()?;
            runs.into_iter().find(|run| run.target == target).map(|run| run.run_id)
        })
        .await)
    }

    async fn reaches(&self, pid: &str, state: RunState, limit: Duration) -> bool {
        within(limit, || async {
            self.runs.get(pid).await.ok()?.filter(|run| run.state == state).map(|_| ())
        })
        .await
        .is_some()
    }

    /// The text every session of the tenant's wake user holds: the model's replies and the
    /// tools' answers.
    async fn transcript(&self) -> String {
        use adk_session::{GetRequest, ListRequest, SessionService as _};
        let list = ListRequest {
            app_name: self.name.clone(),
            user_id: adk_kernel::managed::DEFAULT_WAKE_USER.into(),
            limit: None,
            offset: None,
        };
        let mut text = String::new();
        for session in self.sessions.list(list).await.unwrap_or_default() {
            let request = GetRequest {
                app_name: self.name.clone(),
                user_id: adk_kernel::managed::DEFAULT_WAKE_USER.into(),
                session_id: session.id().to_string(),
                num_recent_events: None,
                after: None,
            };
            let Ok(session) = self.sessions.get(request).await else { continue };
            for event in session.events().all() {
                let Some(content) = &event.llm_response.content else { continue };
                for part in &content.parts {
                    match part {
                        adk_core::Part::Text { text: said } => text.push_str(said),
                        adk_core::Part::FunctionResponse { function_response, .. } => {
                            text.push_str(&function_response.response.to_string())
                        }
                        _ => {}
                    }
                    text.push('\n');
                }
            }
        }
        text
    }
}

/// The drivers scenario on `model`: tenant A's person approves the mount in A's inbox; work's
/// harness calls the driver directly; the coordinator's call is a crossing its process parks
/// on until approved; tenant B, hosting the same definition, is offered nothing.
async fn drivers_scenario(
    checks: &mut Checks,
    a_models: Arc<dyn ModelResolver>,
    b_models: Option<Arc<dyn ModelResolver>>,
    model: &str,
    limit: Duration,
) -> Result<()> {
    let home = tempfile::tempdir()?;
    let notes_dir = home.path().join("notes");
    std::fs::create_dir_all(&notes_dir)?;
    let watermarks = || Arc::new(adk_kernel::stores::InMemoryCronWatermarkStore::new()) as _;
    let a = driver_tenant("tenant-a", home.path(), a_models, watermarks())?;
    a.runtime.create(assistant(model, notes_for_work(&notes_dir)?)).await?;
    let b = match b_models {
        Some(models) => {
            let b = driver_tenant("tenant-b", home.path(), models, watermarks())?;
            b.runtime.create(assistant(model, notes_for_work(&notes_dir)?)).await?;
            Some(b)
        }
        None => None,
    };
    let kernels = ManagedKernels::new();
    kernels.insert(a.kernel.clone())?;
    if let Some(b) = &b {
        kernels.insert(b.kernel.clone())?;
    }
    kernels.start().await?;

    let proposal = a
        .inbox()
        .await
        .into_iter()
        .find(|item| item.tool_name == adk_kernel::managed::DRIVER_MOUNT_TOOL_NAME);
    checks.check(
        "the driver's mount waits in the tenant's inbox, showing its command, partitions and classes",
        proposal.as_ref().is_some_and(|item| {
            item.args_redacted["partitions"] == json!(["work"])
                && item.args_redacted["effectClasses"]["send_note"] == "send_external"
        }),
    );
    checks.check("nothing is mounted before a person approves", a.drivers.mounted().is_empty());
    let Some(proposal) = proposal else { return Ok(()) };
    a.approve(&proposal).await?;
    let mounted = within(limit, || async {
        a.drivers.record("notes").filter(|record| record.approved_by == "ops")
    })
    .await;
    checks.check("the person's approval mounts it on the tenant's supervisor", mounted.is_some());

    let work = a.wake("standup", "assistant@work", limit).await?;
    let completed = match &work {
        Some(pid) => a.reaches(pid, RunState::Completed, limit).await,
        None => false,
    };
    checks.check("work's harness calls the driver directly and completes", completed);
    checks
        .check("and answers from it", a.transcript().await.contains(crate::notes_driver::CODEWORD));

    let coordinator = a.wake("morning", "assistant@home", limit).await?;
    let pid = coordinator.unwrap_or_default();
    checks.check(
        "the coordinator's call is a crossing its process parks on",
        a.reaches(&pid, RunState::Waiting, limit).await,
    );
    let crossing = within(limit, || async {
        a.inbox().await.into_iter().find(|item| item.tool_name == "work__read_note")
    })
    .await;
    checks.check("one crossing waits in the inbox", crossing.is_some());
    if let Some(crossing) = crossing {
        a.approve(&crossing).await?;
    }
    checks.check(
        "the approved crossing completes the process",
        a.reaches(&pid, RunState::Completed, limit).await,
    );
    let ledger = a.kernel.partitions().crossings(&Default::default()).await?;
    checks.check(
        "the ledger shows the driver crossing held, then approved",
        ledger
            .iter()
            .filter(|record| record.resource == adk_kernel::CrossingResource::Driver)
            .map(|record| record.outcome)
            .collect::<Vec<_>>()
            == [adk_kernel::CrossingOutcome::Held, adk_kernel::CrossingOutcome::Approved],
    );

    if let Some(b) = &b {
        let run = b.wake("standup", "assistant@work", limit).await?;
        let done = match &run {
            Some(pid) => b.reaches(pid, RunState::Completed, limit).await,
            None => false,
        };
        checks.check(
            "tenant B's harness, on the same definition, runs without the driver",
            done && b.drivers.mounted().is_empty()
                && !b.transcript().await.contains(crate::notes_driver::CODEWORD),
        );
        checks.check(
            "tenant B's mount still waits for its own person",
            b.inbox()
                .await
                .iter()
                .any(|item| item.tool_name == adk_kernel::managed::DRIVER_MOUNT_TOOL_NAME),
        );
    }
    kernels.stop().await?;
    Ok(())
}

/// M2 scripted: two tenants, one definition; see [`drivers_scenario`].
async fn tenant_drivers() -> Result<Checks> {
    let mut checks = Checks::default();
    let reply = |text: &str| ScriptedResponse::text(text);
    let a = Arc::new(ScriptedLlm::new(
        "scripted",
        vec![
            ScriptedResponse::function_call("read_note", json!({})),
            reply(crate::notes_driver::CODEWORD),
            ScriptedResponse::function_call("work__read_note", json!({})),
            reply(crate::notes_driver::CODEWORD),
        ],
    ));
    let b = Arc::new(ScriptedLlm::new("scripted", vec![reply("no notes here")]));
    drivers_scenario(
        &mut checks,
        Arc::new(Scripted(a)),
        Some(Arc::new(Scripted(b.clone()))),
        "scripted",
        Duration::from_secs(20),
    )
    .await?;
    let offered: Vec<String> = b
        .requests()
        .first()
        .map(|request| request.tools.keys().cloned().collect())
        .unwrap_or_default();
    checks.check(
        "tenant B's model was offered no notes tool",
        !offered.iter().any(|name| name.contains("note")),
    );
    Ok(checks)
}

/// Live drivers: one tenant on `model`; see [`drivers_scenario`].
async fn live_tenant_drivers(provider: &str, model: &str) -> Result<Checks> {
    let mut checks = Checks::default();
    let models: Arc<dyn ModelResolver> = Arc::new(DefaultModelResolver::new());
    if models.resolve(&ModelRef::Shorthand(model.into())).await.is_err() {
        bail!("{provider}'s model {model} cannot be built: is its key set?");
    }
    drivers_scenario(&mut checks, models, None, model, Duration::from_secs(180)).await?;
    Ok(checks)
}

/// The yearly schedule: 00:00 UTC on 1 January, so no live tick falls inside the scenario.
const YEARLY: &str = "0 0 0 1 1 *";

/// A definition whose wake sets on [`YEARLY`] have the given policies, each queueing.
fn yearly(model: &str, sets: &[(&str, adk_core::schedule::MissedFirePolicy)]) -> ManagedAgentDef {
    let harness: ManagedAgentDef = HarnessDef::new("digest", ModelRef::Shorthand(model.into()))
        .with_mode(HarnessMode::Ask)
        .with_shell(ShellPolicy::none())
        .into();
    let mut kernel = KernelDef::new();
    for (name, missed) in sets {
        kernel = kernel.with_wake_set(
            WakeSetDef::new(
                *name,
                [Wake::Cron { expr: YEARLY.into(), missed: *missed }],
                "The yearly summary is due. Reply with the single word: done",
            )
            .with_overlap(adk_kernel::managed::WakeOverlap::Queue),
        );
    }
    kernel.apply_to(harness)
}

/// The yearly ticks each wake set fired, by set.
async fn fired(runs: &Arc<dyn DurableRunStore>) -> std::collections::BTreeMap<String, usize> {
    let mut fired = std::collections::BTreeMap::new();
    for run in runs.list(RunFilter::default()).await.unwrap_or_default() {
        if let Some(set) = run
            .idempotency_key
            .as_deref()
            .and_then(|key| key.split_once("#digest/"))
            .map(|(_, set)| set.to_string())
        {
            *fired.entry(set).or_default() += 1;
        }
    }
    fired
}

/// The missed-ticks scenario on `models`: a tenant whose cursor is three years old starts;
/// each wake set fires what its policy picks, once, and a restart fires nothing again.
async fn missed_ticks_scenario(
    checks: &mut Checks,
    models: Arc<dyn ModelResolver>,
    model: &str,
    sets: &[(&str, adk_core::schedule::MissedFirePolicy)],
    expected: &[(&str, usize)],
    limit: Duration,
) -> Result<()> {
    use adk_kernel::CronWatermarkStore as _;
    let home = tempfile::tempdir()?;
    let watermarks = Arc::new(adk_kernel::stores::InMemoryCronWatermarkStore::new());
    let tenant = driver_tenant("tenant-a", home.path(), models, watermarks.clone())?;
    let seeded = chrono::Utc::now() - chrono::Duration::days(3 * 365 + 10);
    watermarks.advance("0_0_0_1_1_*", seeded).await?;
    tenant.runtime.create(yearly(model, sets)).await?;
    tenant.kernel.start().await?;
    let total: usize = expected.iter().map(|(_, count)| count).sum();
    let settled = within(limit, || async {
        let fired = fired(&tenant.runs).await;
        let runs = tenant.runs.list(RunFilter::default()).await.ok()?;
        let done = runs.iter().all(|run| run.state.is_terminal());
        (fired.values().sum::<usize>() >= total && done).then_some(fired)
    })
    .await;
    let fired_now = fired(&tenant.runs).await;
    println!("    fired after the outage: {fired_now:?}");
    if settled.is_none() {
        // Why a process has not finished: its state and last error, and what waits on a person.
        for run in tenant.runs.list(RunFilter::default()).await? {
            if !run.state.is_terminal() {
                let (target, state, attempt, error) =
                    (run.target, run.state, run.attempt, run.error);
                println!("    unfinished: {target} {state:?}, attempt {attempt}, error {error:?}");
            }
        }
        for request in tenant.inbox().await {
            println!("    waiting on a person: {} ({})", request.tool_name, request.summary);
        }
    }
    checks.check("the missed ticks fired and their processes finished", settled.is_some());
    for (set, count) in expected {
        checks.check(
            &format!("wake set `{set}` fired {count} missed tick(s)"),
            fired_now.get(*set).copied().unwrap_or(0) == *count,
        );
    }
    let completed = tenant
        .runs
        .list(RunFilter::default())
        .await?
        .iter()
        .filter(|run| run.target == "digest")
        .all(|run| run.state == RunState::Completed);
    checks.check("every replayed tick's process completed", completed);

    tenant.kernel.stop().await?;
    tenant.kernel.start().await?;
    tokio::time::sleep(Duration::from_secs(2)).await;
    tenant.kernel.stop().await?;
    checks.check("a restart replays nothing again", fired(&tenant.runs).await == fired_now);
    Ok(())
}

/// M3 scripted: `fireOnce`, `catchUp(2)` and `skip` on one yearly expression.
async fn missed_ticks() -> Result<Checks> {
    use adk_core::schedule::MissedFirePolicy;
    let mut checks = Checks::default();
    let replies: Vec<ScriptedResponse> = (0..6).map(|_| ScriptedResponse::text("done")).collect();
    missed_ticks_scenario(
        &mut checks,
        Arc::new(Scripted(Arc::new(ScriptedLlm::new("scripted", replies)))),
        "scripted",
        &[
            ("once", MissedFirePolicy::FireOnce),
            ("two", MissedFirePolicy::CatchUp(2)),
            ("skip", MissedFirePolicy::Skip),
        ],
        &[("once", 1), ("two", 2), ("skip", 0)],
        Duration::from_secs(20),
    )
    .await?;
    Ok(checks)
}

/// Live missed ticks: one `fireOnce` wake set on `model`.
async fn live_missed_ticks(provider: &str, model: &str) -> Result<Checks> {
    use adk_core::schedule::MissedFirePolicy;
    let mut checks = Checks::default();
    let models: Arc<dyn ModelResolver> = Arc::new(DefaultModelResolver::new());
    if models.resolve(&ModelRef::Shorthand(model.into())).await.is_err() {
        bail!("{provider}'s model {model} cannot be built: is its key set?");
    }
    missed_ticks_scenario(
        &mut checks,
        models,
        model,
        &[("once", MissedFirePolicy::FireOnce)],
        &[("once", 1)],
        Duration::from_secs(180),
    )
    .await?;
    Ok(checks)
}
