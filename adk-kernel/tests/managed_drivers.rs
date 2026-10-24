//! A definition's drivers in the managed form (spec 3.2 §7.7, §7.8, §7.13, K-I13, K-I15): a
//! `KernelDef`'s drivers are proposed in the tenant's approval inbox, mounted on the tenant's
//! own supervisor only when a person approves, digest-pinned at their first listing, served
//! to the partition that mounts them, and invisible to every other tenant. On `origin/main` a
//! definition's driver manifests are only recorded: nothing is proposed or mounted.
//!
//! The driver is this test binary run as a stdio MCP server (`driver_support`).

#![cfg(feature = "managed")]

mod driver_support;
mod managed_support;

use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_agent::LlmAgentBuilder;
use adk_core::{
    Agent, ApprovalDecision, ApprovalPolicy, ApprovalRequest, ApprovalService, ApproverSelector,
    DurableRunStore, InMemoryApprovalStore, InMemoryRunStore, InboxQuery, Llm,
    StoreApprovalService, SubjectRef,
};
use adk_kernel::KernelConfig;
use adk_kernel::drivers::{DriverManifest, DriverSupervisor};
use adk_kernel::managed::{
    DRIVER_MOUNT_TOOL_NAME, KernelDef, ManagedKernels, PartitionAgentResolver, PartitionDef,
    PartitionHost, TenantKernel,
};
use adk_kernel::stores::InMemoryDriverStore;
use adk_managed::types::{ManagedAgentDef, ModelRef, RuntimeError};
use adk_managed::{
    AgentResolver, InMemoryManagedStateStore, ManagedAgentRuntime, ManagedStores, ModelResolver,
};
use adk_session::{InMemorySessionService, SessionService};
use driver_support::{RunContext, Workdir};
use managed_support::*;

#[tokio::test]
#[ignore = "runs only as the stdio driver the tests start"]
async fn stdio_driver() {
    driver_support::serve_stdio().await;
}

/// Builds each partition's agent on the step model, and keeps the host it was given.
#[derive(Default)]
struct PartitionAgents {
    hosts: Mutex<Vec<PartitionHost>>,
}

#[async_trait::async_trait]
impl PartitionAgentResolver for PartitionAgents {
    async fn resolve_partition(
        &self,
        def: &ManagedAgentDef,
        host: &PartitionHost,
        _models: &dyn ModelResolver,
    ) -> Result<Option<Arc<dyn Agent>>, RuntimeError> {
        self.hosts.lock().unwrap().push(host.clone());
        let model: Arc<dyn Llm> = Arc::new(StepModel { requests: AtomicUsize::new(0) });
        let agent = LlmAgentBuilder::new(format!("{}-{}", def.name, host.partition.id))
            .model(model)
            .build()
            .map_err(|error| RuntimeError::internal(error.to_string()))?;
        Ok(Some(Arc::new(agent)))
    }
}

/// One tenant with its own stores, approval service and driver supervisor.
struct Tenant {
    kernel: TenantKernel,
    service: Arc<dyn ApprovalService>,
    drivers: DriverSupervisor,
    partitions: Arc<PartitionAgents>,
}

impl Tenant {
    fn new(name: &str, home: &std::path::Path) -> Self {
        let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
        let decisions = Arc::new(InMemoryApprovalStore::new());
        let service: Arc<dyn ApprovalService> = Arc::new(
            StoreApprovalService::new(decisions.clone())
                .with_run_store(Arc::clone(&runs))
                .with_router(ApprovalPolicy::new([ApproverSelector::group(OPERATOR)])),
        );
        let stores =
            ManagedStores::new(Arc::new(InMemoryManagedStateStore::new()), runs, decisions)
                .with_approval_service(Arc::clone(&service));
        let drivers = DriverSupervisor::builder(Arc::new(InMemoryDriverStore::new()))
            .health_check_interval(Duration::from_millis(200))
            .build();
        let resolver: Arc<dyn AgentResolver> = Arc::new(TestResolver {
            model: Arc::new(StepModel { requests: AtomicUsize::new(0) }),
            calls: Arc::new(AtomicUsize::new(0)),
            confirm: false,
        });
        let partitions = Arc::new(PartitionAgents::default());
        let kernel = TenantKernel::builder(
            name,
            stores,
            Arc::new(InMemorySessionService::new()) as Arc<dyn SessionService>,
        )
        .config(fast_config(name).with_home(home))
        .resolver(resolver)
        .partition_resolver(Arc::clone(&partitions) as Arc<dyn PartitionAgentResolver>)
        .drivers(drivers.clone())
        .build()
        .unwrap();
        Self { kernel, service, drivers, partitions }
    }

    /// The driver mounts waiting in the tenant's inbox.
    async fn proposals(&self) -> Vec<ApprovalRequest> {
        let query = InboxQuery::for_approver(SubjectRef::human("ana"), [OPERATOR]);
        let page = self.service.inbox(query).await.unwrap();
        page.items.into_iter().filter(|item| item.tool_name == DRIVER_MOUNT_TOOL_NAME).collect()
    }

    async fn decide(&self, request: &ApprovalRequest, by: SubjectRef) {
        let decision = ApprovalDecision::approve(by).with_groups([OPERATOR]);
        self.service.decide(&request.id, request.version, decision).await.unwrap();
    }

    /// The tools the partition `partition`'s host offers from the tenant's drivers.
    async fn served(&self, partition: &str) -> Vec<String> {
        let host = self
            .partitions
            .hosts
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|host| host.partition.id.as_str() == partition)
            .cloned()
            .expect("the partition was hosted");
        let drivers = host.drivers.expect("the host carries the tenant's drivers");
        let toolset = drivers.toolset_for(&host.partition.id, &host.partition.drivers);
        let ctx = RunContext::new(None, "s1", "call-1");
        let mut names: Vec<String> =
            toolset.tools(ctx).await.unwrap().iter().map(|tool| tool.name().to_string()).collect();
        names.sort();
        names
    }
}

/// A definition whose `work` partition mounts `notes`.
fn definition(notes: DriverManifest) -> ManagedAgentDef {
    KernelDef::new()
        .with_partition(PartitionDef::new("work", "work").with_driver("notes"))
        .with_driver(notes)
        .apply_to(ManagedAgentDef::new(
            "assistant",
            ModelRef::Shorthand("gemini-3.5-flash-lite".into()),
        ))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_definitions_driver_mounts_on_its_tenant_only_when_a_person_approves_it() {
    let home = tempfile::tempdir().unwrap();
    let work = Workdir::new();
    let notes = work.manifest("notes", "partitions = [\"work\"]");
    let (a, b) = (Tenant::new("tenant-a", home.path()), Tenant::new("tenant-b", home.path()));
    for tenant in [&a, &b] {
        tenant.kernel.runtime(Arc::new(Models)).create(definition(notes.clone())).await.unwrap();
    }
    let kernels = ManagedKernels::new();
    kernels.insert(a.kernel.clone()).unwrap();
    kernels.insert(b.kernel.clone()).unwrap();
    kernels.start().await.unwrap();

    // Each tenant's inbox shows what a person approves; nothing is mounted before.
    let proposed = a.proposals().await;
    assert_eq!(proposed.len(), 1, "{proposed:?}");
    let shown = &proposed[0].args_redacted;
    assert_eq!(shown["id"], "notes");
    assert_eq!(shown["partitions"], serde_json::json!(["work"]));
    assert_eq!(shown["effectClasses"]["send_note"], "send_external");
    assert!(shown["commandLine"].as_str().unwrap().contains("stdio_driver"), "{shown}");
    assert_eq!(b.proposals().await.len(), 1, "tenant B is asked in its own inbox");
    assert!(a.drivers.mounted().is_empty() && b.drivers.mounted().is_empty());
    assert!(a.kernel.apply_driver_decisions().await.unwrap().is_empty(), "nobody decided yet");

    // A person in tenant A's operator group approves; the next tick mounts it.
    a.decide(&proposed[0], SubjectRef::human("ana")).await;
    let mounted = eventually("tenant A's driver to mount", || async {
        a.drivers.record("notes").filter(|record| record.approved_by == "ana")
    })
    .await;
    assert!(mounted.manifest.declaration_digests.is_empty(), "pinned at the first listing");

    // The partition that mounts it serves its tools, and pins their declarations.
    let served = a.served("work").await;
    assert!(served.contains(&"read_note".to_string()), "{served:?}");
    let pinned = a.drivers.record("notes").unwrap().manifest.declaration_digests;
    assert!(pinned.contains_key("read_note"), "{pinned:?}");
    let run_dir = std::fs::read_dir(home.path().join("tenants"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.file_name().unwrap().to_string_lossy().starts_with("tenant-a-"))
        .expect("tenant A's drivers run under a home of its own");
    assert!(run_dir.join("drivers").join("run").join("notes").exists(), "{run_dir:?}");

    // Tenant B decided nothing: its driver is not mounted, its partition serves nothing.
    assert!(b.drivers.mounted().is_empty());
    assert!(b.served("work").await.is_empty(), "tenant A's approval mounts nothing for B");
    assert_eq!(b.proposals().await.len(), 1, "B's proposal still waits");

    // A changed manifest is asked again; the approved one keeps serving meanwhile.
    let changed = work.manifest("notes", "partitions = [\"work\"]\nlifecycle = \"eager\"");
    a.kernel.runtime(Arc::new(Models)).create(definition(changed)).await.unwrap();
    let again = a.proposals().await;
    assert_eq!(again.len(), 1, "the changed manifest waits for its own approval");
    assert_ne!(again[0].id, proposed[0].id);
    assert!(
        a.drivers.record("notes").unwrap().manifest.declaration_digests.contains_key("read_note")
    );
    kernels.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_mount_approved_by_anyone_but_a_person_mounts_nothing() {
    let home = tempfile::tempdir().unwrap();
    let work = Workdir::new();
    let tenant = Tenant::new("tenant-a", home.path());
    let notes = work.manifest("notes", "partitions = [\"work\"]");
    tenant.kernel.runtime(Arc::new(Models)).create(definition(notes)).await.unwrap();
    let proposed = tenant.proposals().await;
    tenant.decide(&proposed[0], SubjectRef::service("deploy-bot")).await;
    assert!(tenant.kernel.apply_driver_decisions().await.unwrap().is_empty());
    assert!(tenant.drivers.mounted().is_empty(), "a service's approval mounts nothing");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tenants_never_share_a_driver_supervisor_or_store() {
    let home = tempfile::tempdir().unwrap();
    let a = Tenant::new("tenant-a", home.path());
    let kernels = ManagedKernels::new();
    kernels.insert(a.kernel.clone()).unwrap();
    let with = |drivers: DriverSupervisor, name: &str| {
        TenantKernel::builder(
            name,
            ManagedStores::in_memory(),
            Arc::new(InMemorySessionService::new()) as Arc<dyn SessionService>,
        )
        .config(KernelConfig::default().with_home(home.path()))
        .drivers(drivers)
        .build()
        .unwrap()
    };
    let error = kernels.insert(with(a.drivers.clone(), "tenant-b")).unwrap_err();
    assert!(error.to_string().contains("driver supervisor"), "{error}");
    let same_store = DriverSupervisor::builder(Arc::clone(a.drivers.store())).build();
    let error = kernels.insert(with(same_store, "tenant-c")).unwrap_err();
    assert!(error.to_string().contains("driver store"), "{error}");
}
