//! A definition's drivers on its tenant kernel, with real harnesses (spec 3.2 §7.7, §7.8,
//! §7.13, K-I13, K-I15): `HarnessResolver` builds one harness per partition; the driver the
//! definition's `work` partition mounts is proposed in the tenant's inbox and mounted when a
//! person approves it; work's harness then calls it directly, and the coordinator reaches it
//! only as a crossing its process parks on until a person approves. A second tenant hosting
//! the same definition sees none of it. On `origin/main` the driver is only recorded, so no
//! harness of either tenant is offered its tool.
//!
//! The driver is this test binary run as a stdio MCP server publishing one read tool.

#![cfg(all(feature = "managed", feature = "kernel"))]

use std::sync::Arc;
use std::time::Duration;

use adk_core::{
    ApprovalDecision, ApprovalPolicy, ApprovalRequest, ApprovalService, ApproverSelector,
    DurableRunStore, InMemoryApprovalStore, InMemoryRunStore, InboxQuery, Llm, Part, RunState,
    StoreApprovalService, SubjectRef,
};
use adk_harness::HarnessMode;
use adk_harness::packs::managed::{HarnessDef, HarnessResolver, ShellPolicy};
use adk_kernel::drivers::{DriverManifest, DriverSupervisor};
use adk_kernel::managed::{
    DRIVER_MOUNT_TOOL_NAME, KernelDef, ManagedKernels, PartitionDef, TenantKernel, WakeSetDef,
};
use adk_kernel::stores::InMemoryDriverStore;
use adk_kernel::{KernelConfig, Wake, WakeEvent};
use adk_managed::resolver::{ModelResolver, ResolverResult};
use adk_managed::types::{ManagedAgentDef, ModelRef};
use adk_managed::{InMemoryManagedStateStore, ManagedAgentRuntime, ManagedStores};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_server::background::DurableWorkerConfig;
use adk_session::InMemorySessionService;
use async_trait::async_trait;
use rmcp::model::*;
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServerHandler, ServiceExt};
use serde_json::json;

const SERVER_ENV: &str = "ADK_HARNESS_MANAGED_DRIVER_TEST_SERVER";
const OPERATOR: &str = "operator";

/// One read tool, `calendar_read`, answering `standup at nine`.
#[derive(Clone)]
struct Calendar;

impl ServerHandler for Calendar {
    fn get_info(&self) -> InitializeResult {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        _params: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let schema = Arc::new(json!({ "type": "object" }).as_object().unwrap().clone());
        Ok(ListToolsResult::with_all_items(vec![Tool::new(
            "calendar_read",
            "Reads today's calendar.",
            schema,
        )]))
    }

    async fn call_tool(
        &self,
        _params: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        Ok(CallToolResult::success(vec![ContentBlock::text("standup at nine")]).into())
    }
}

#[tokio::test]
#[ignore = "runs only as the stdio driver the tests start"]
async fn stdio_calendar_driver() {
    if std::env::var(SERVER_ENV).is_err() {
        return;
    }
    let running = Calendar.serve((tokio::io::stdin(), tokio::io::stdout())).await.unwrap();
    let _ = running.waiting().await;
}

/// The `calendar` driver, serving work, run under the OS sandbox where one exists.
fn calendar() -> DriverManifest {
    DriverManifest::from_toml(&format!(
        "id = \"calendar\"\ncommand = {:?}\n\
         args = [\"managed_partition_drivers::stdio_calendar_driver\", \"--exact\", \"--ignored\", \"--quiet\", \"--test-threads=1\"]\n\
         partitions = [\"work\"]\nenv = {{ {SERVER_ENV} = \"1\" }}\n[tools]\ncalendar_read = \"read\"\n",
        std::env::current_exe().unwrap().display().to_string(),
    ))
    .unwrap()
}

struct Models(Arc<ScriptedLlm>);

#[async_trait]
impl ModelResolver for Models {
    async fn resolve(&self, _model: &ModelRef) -> ResolverResult<Arc<dyn Llm>> {
        Ok(Arc::clone(&self.0) as Arc<dyn Llm>)
    }
}

/// Home, the coordinator, and work, which mounts `calendar`; a webhook wake in each.
fn definition() -> ManagedAgentDef {
    let harness: ManagedAgentDef =
        HarnessDef::new("assistant", ModelRef::Shorthand("scripted".into()))
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
        .with_partition(open("work").with_driver("calendar"))
        .with_coordinator("home")
        .with_driver(calendar())
        .with_wake_set(
            WakeSetDef::new("standup", [Wake::Webhook { hook: "standup".into() }], "Standup?")
                .with_partition("work"),
        )
        .with_wake_set(
            WakeSetDef::new("morning", [Wake::Webhook { hook: "morning".into() }], "Morning?")
                .with_partition("home"),
        )
        .apply_to(harness)
}

/// A tenant with its own stores, approval service and driver supervisor, its harnesses on
/// `model`.
struct Tenant {
    name: String,
    kernel: TenantKernel,
    service: Arc<dyn ApprovalService>,
    runs: Arc<dyn DurableRunStore>,
    drivers: DriverSupervisor,
    model: Arc<ScriptedLlm>,
}

impl Tenant {
    async fn new(name: &str, home: &std::path::Path, script: Vec<ScriptedResponse>) -> Self {
        let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
        let decisions = Arc::new(InMemoryApprovalStore::new());
        let service: Arc<dyn ApprovalService> = Arc::new(
            StoreApprovalService::new(decisions.clone())
                .with_run_store(Arc::clone(&runs))
                .with_router(ApprovalPolicy::new([ApproverSelector::group(OPERATOR)])),
        );
        let stores = ManagedStores::new(
            Arc::new(InMemoryManagedStateStore::new()),
            Arc::clone(&runs),
            decisions,
        )
        .with_approval_service(Arc::clone(&service));
        let drivers = DriverSupervisor::builder(Arc::new(InMemoryDriverStore::new()))
            .health_check_interval(Duration::from_millis(200))
            .build();
        let resolver = Arc::new(HarnessResolver::new(home.join("workspaces").join(name)));
        let kernel = TenantKernel::builder(name, stores, Arc::new(InMemorySessionService::new()))
            .config(
                KernelConfig::default()
                    .with_home(home)
                    .with_worker(
                        DurableWorkerConfig::default()
                            .with_worker_id(name)
                            .with_lease_ttl(Duration::from_millis(600))
                            .with_poll_interval(Duration::from_millis(20)),
                    )
                    .with_scheduler_tick(Duration::from_millis(50)),
            )
            .resolver(resolver.clone())
            .partition_resolver(resolver)
            .drivers(drivers.clone())
            .build()
            .unwrap();
        let model = Arc::new(ScriptedLlm::new("scripted", script));
        kernel.runtime(Arc::new(Models(Arc::clone(&model)))).create(definition()).await.unwrap();
        Self { name: name.into(), kernel, service, runs, drivers, model }
    }

    async fn pending(&self) -> Vec<ApprovalRequest> {
        let query = InboxQuery::for_approver(SubjectRef::human("ana"), [OPERATOR]);
        self.service.inbox(query).await.unwrap().items
    }

    async fn approve(&self, request: &ApprovalRequest) {
        let decision = ApprovalDecision::approve(SubjectRef::human("ana")).with_groups([OPERATOR]);
        self.service.decide(&request.id, request.version, decision).await.unwrap();
    }

    /// Delivers webhook `hook` and returns the process it started.
    async fn wake(&self, hook: &str) -> String {
        let kernel = self.kernel.kernel().await.unwrap();
        let delivery =
            WakeEvent::new(format!("hook:{hook}"), format!("hook:{hook}:evt-1"), json!({}));
        kernel.wake_router().submit(&delivery, &self.name).await.unwrap();
        let target = format!("assistant@{}", if hook == "standup" { "work" } else { "home" });
        eventually("the wake's process", || async {
            let runs = self.runs.list(Default::default()).await.unwrap();
            runs.into_iter().find(|run| run.target == target).map(|run| run.run_id)
        })
        .await
    }

    async fn in_state(&self, pid: &str, state: RunState) {
        eventually(&format!("{pid} to be {state:?}"), || async {
            self.runs.get(pid).await.ok().flatten().filter(|run| run.state == state).map(|_| ())
        })
        .await;
    }

    /// The text of every function response `tool` got, across the model's requests.
    fn answers(&self, tool: &str) -> Vec<String> {
        self.model
            .requests()
            .iter()
            .flat_map(|request| request.contents.iter())
            .flat_map(|content| content.parts.iter())
            .filter_map(|part| match part {
                Part::FunctionResponse { function_response, .. }
                    if function_response.name == tool =>
                {
                    Some(function_response.response.to_string())
                }
                _ => None,
            })
            .collect()
    }
}

async fn eventually<T, F, Fut>(what: &str, mut probe: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let started = tokio::time::Instant::now();
    while started.elapsed() <= Duration::from_secs(15) {
        if let Some(value) = probe().await {
            return value;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {what}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_tenants_approved_driver_serves_its_partition_and_the_coordinator_only_by_crossing() {
    let home = tempfile::tempdir().unwrap();
    let a = Tenant::new(
        "tenant-a",
        home.path(),
        vec![
            ScriptedResponse::function_call("calendar_read", json!({})),
            ScriptedResponse::text("Standup is at nine."),
            ScriptedResponse::function_call("work__calendar_read", json!({})),
            ScriptedResponse::text("Work says standup is at nine."),
        ],
    )
    .await;
    let b = Tenant::new("tenant-b", home.path(), vec![ScriptedResponse::text("Nothing.")]).await;
    let kernels = ManagedKernels::new();
    kernels.insert(a.kernel.clone()).unwrap();
    kernels.insert(b.kernel.clone()).unwrap();
    kernels.start().await.unwrap();

    // A person approves tenant A's mount in A's inbox; B decides nothing.
    let proposal = a
        .pending()
        .await
        .into_iter()
        .find(|item| item.tool_name == DRIVER_MOUNT_TOOL_NAME)
        .expect("the mount is proposed in tenant A's inbox");
    a.approve(&proposal).await;
    eventually("tenant A's driver to mount", || async {
        a.drivers.record("calendar").filter(|record| record.approved_by == "ana")
    })
    .await;

    // Work's harness calls it directly.
    let work = a.wake("standup").await;
    a.in_state(&work, RunState::Completed).await;
    let direct = a.answers("calendar_read");
    assert!(direct.iter().any(|answer| answer.contains("standup at nine")), "{direct:?}");

    // The coordinator's call is a crossing: its process parks until a person approves it.
    let coordinator = a.wake("morning").await;
    a.in_state(&coordinator, RunState::Waiting).await;
    let crossing = eventually("the crossing in tenant A's inbox", || async {
        a.pending().await.into_iter().find(|item| item.tool_name == "work__calendar_read")
    })
    .await;
    a.approve(&crossing).await;
    a.in_state(&coordinator, RunState::Completed).await;
    let crossed = a.answers("work__calendar_read");
    assert!(crossed.iter().any(|answer| answer.contains("standup at nine")), "{crossed:?}");
    let ledger = a.kernel.partitions().crossings(&Default::default()).await.unwrap();
    assert!(
        ledger.iter().any(|record| record.to.as_str() == "work"
            && record.resource == adk_kernel::CrossingResource::Driver),
        "{ledger:?}"
    );

    // Tenant B's work harness was offered no driver tool: its mount waits for its own person.
    let b_work = b.wake("standup").await;
    b.in_state(&b_work, RunState::Completed).await;
    let offered: Vec<String> = b.model.requests()[0].tools.keys().cloned().collect();
    assert!(!offered.iter().any(|name| name.contains("calendar")), "{offered:?}");
    assert!(b.drivers.mounted().is_empty());
    assert!(b.pending().await.iter().any(|item| item.tool_name == DRIVER_MOUNT_TOOL_NAME));
    kernels.stop().await.unwrap();
}
