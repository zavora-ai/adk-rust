//! Partitions in the managed form (spec 3.2 §7.8, §7.13): a definition's `partitions` and
//! `coordinator` run one agent per partition on its tenant's kernel — each in the tenant's
//! own partition table with the tenant as its tenant, as `<definition>@<partition>` — and a
//! wake set in a partition starts that partition's agent. On `origin/main` a definition's
//! partitions are recorded and its agent runs in the default partition.

#![cfg(feature = "managed")]

use crate::managed_support;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use adk_agent::LlmAgentBuilder;
use adk_core::{Agent, Llm, RunState};
use adk_kernel::managed::{
    KernelDef, ManagedKernels, PartitionAgentResolver, PartitionDef, PartitionHost, TenantKernel,
    WakeSetDef,
};
use adk_kernel::{
    InMemoryPartitionStore, PartitionId, PartitionMode, ProcessFilter, Wake, WakeEvent,
};
use adk_managed::resolver::ModelResolver;
use adk_managed::types::{ManagedAgentDef, ModelRef, RuntimeError};
use adk_managed::{ManagedAgentRuntime, ManagedStores};
use adk_session::InMemorySessionService;
use async_trait::async_trait;
use managed_support::*;
use serde_json::json;

/// Builds each partition's agent on the step model and records what it was asked for.
struct PartitionAgents {
    model: Arc<StepModel>,
    calls: Arc<std::sync::atomic::AtomicUsize>,
    seen: Mutex<Vec<(String, Option<String>, bool)>>,
}

#[async_trait]
impl PartitionAgentResolver for PartitionAgents {
    async fn resolve_partition(
        &self,
        def: &ManagedAgentDef,
        host: &PartitionHost,
        _models: &dyn ModelResolver,
    ) -> Result<Option<Arc<dyn Agent>>, RuntimeError> {
        self.seen.lock().unwrap().push((
            host.partition.id.to_string(),
            host.partition.tenant.clone(),
            host.coordinator,
        ));
        let agent = LlmAgentBuilder::new(format!("{}_{}", def.name, host.partition.id))
            .model(Arc::clone(&self.model) as Arc<dyn Llm>)
            .tool(Arc::new(Step { calls: Arc::clone(&self.calls) }))
            .build()
            .map_err(|error| RuntimeError::internal(error.to_string()))?;
        Ok(Some(Arc::new(agent)))
    }
}

fn partitioned() -> KernelDef {
    KernelDef::new()
        .with_partition(PartitionDef::new("home", "home-agents"))
        .with_partition(PartitionDef::new("work", "work-agents").with_mode(PartitionMode::Auto))
        .with_coordinator("home")
        .with_wake_set(
            WakeSetDef::new("standup", [Wake::Webhook { hook: "standup".into() }], "Standup.")
                .with_partition("work"),
        )
}

fn definition(kernel: &KernelDef) -> ManagedAgentDef {
    kernel.apply_to(ManagedAgentDef::new("digest", ModelRef::Shorthand("scripted".into())))
}

/// A tenant whose runtime builds partition agents with `agents`.
fn tenant_with(
    name: &str,
    agents: Arc<PartitionAgents>,
) -> (TenantKernel, adk_managed::DefaultManagedAgentRuntime) {
    let kernel = TenantKernel::builder(
        name,
        ManagedStores::in_memory(),
        Arc::new(InMemorySessionService::new()),
    )
    .config(fast_config(name))
    .resolver(Arc::new(TestResolver {
        model: Arc::clone(&agents.model),
        calls: Arc::clone(&agents.calls),
        confirm: false,
    }))
    .partition_resolver(agents)
    .build()
    .unwrap();
    let runtime = kernel.runtime(Arc::new(Models));
    (kernel, runtime)
}

fn agents() -> Arc<PartitionAgents> {
    Arc::new(PartitionAgents {
        model: Arc::new(StepModel { requests: Default::default() }),
        calls: Arc::new(Default::default()),
        seen: Mutex::new(Vec::new()),
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn each_partition_of_a_definition_runs_its_own_agent_on_the_tenants_table() {
    let agents = agents();
    let (tenant, runtime) = tenant_with("tenant-a", Arc::clone(&agents));
    runtime.create(definition(&partitioned())).await.unwrap();
    let mut seen = agents.seen.lock().unwrap().clone();
    seen.sort();
    assert_eq!(
        seen,
        [
            ("home".to_string(), Some("tenant-a".to_string()), true),
            ("work".to_string(), Some("tenant-a".to_string()), false),
        ]
    );
    let table = tenant.partitions().list().await.unwrap();
    assert_eq!(
        table.iter().map(|partition| partition.id.as_str()).collect::<Vec<_>>(),
        ["home", "work"]
    );
    assert!(table.iter().all(|partition| partition.tenant.as_deref() == Some("tenant-a")));
    assert_eq!(table[1].default_mode, PartitionMode::Auto);

    tenant.start().await.unwrap();
    let kernel = tenant.kernel().await.unwrap();
    let work = PartitionId::new("work");
    assert_eq!(kernel.partition_of_target("digest@work"), Some(&work));
    assert_eq!(kernel.partition_of_target("digest@home"), Some(&PartitionId::new("home")));
    assert_eq!(
        kernel.partition_of_target("digest"),
        Some(&PartitionId::new("home")),
        "the coordinator's agent serves the definition's own target"
    );

    // A wake in work starts work's agent, in work.
    let delivery = WakeEvent::new("hook:standup", "hook:standup:evt-1", json!({}));
    kernel.wake_router().submit(&delivery, "tenant-a").await.unwrap();
    let process = eventually("the standup to complete", || async {
        kernel
            .processes()
            .ps(ProcessFilter::default())
            .await
            .unwrap()
            .into_iter()
            .find(|process| process.run.state == RunState::Completed)
    })
    .await;
    assert_eq!((process.run.target.as_str(), &process.meta.partition), ("digest@work", &work));
    tenant.stop().await.unwrap();
}

#[tokio::test]
async fn host_partitions_needs_one_agent_per_declared_partition_and_owns_its_partitions() {
    let agents = agents();
    let (tenant, _runtime) = tenant_with("tenant-a", Arc::clone(&agents));
    let agent = |name: &str| -> Arc<dyn Agent> {
        Arc::new(
            LlmAgentBuilder::new(name)
                .model(Arc::clone(&agents.model) as Arc<dyn Llm>)
                .build()
                .unwrap(),
        )
    };
    let only_home = BTreeMap::from([(PartitionId::new("home"), agent("home"))]);
    let error = tenant.host_partitions(&definition(&partitioned()), only_home).await.unwrap_err();
    assert!(error.to_string().contains("one agent per declared partition"), "{error}");
    let both = || {
        BTreeMap::from([
            (PartitionId::new("home"), agent("home")),
            (PartitionId::new("work"), agent("work")),
        ])
    };
    assert!(tenant.host_partitions(&definition(&partitioned()), both()).await.unwrap());

    // Another definition declaring work on the same tenant is refused: one table.
    let other = partitioned()
        .apply_to(ManagedAgentDef::new("planner", ModelRef::Shorthand("scripted".into())));
    let error = tenant.host_partitions(&other, both()).await.unwrap_err();
    assert!(error.to_string().contains("declares partition `home`"), "{error}");
}

/// Cross-tenant (spec 3.2 §7.13): each tenant's partition table is its own. Tenant A's
/// partitions carry tenant A and are not in tenant B's table, and a tenant whose partition
/// store another tenant holds is refused.
#[tokio::test]
async fn tenants_keep_their_own_partition_tables() {
    let (a, a_runtime) = tenant_with("tenant-a", agents());
    let (b, _) = tenant_with("tenant-b", agents());
    a_runtime.create(definition(&partitioned())).await.unwrap();
    assert_eq!(a.partitions().list().await.unwrap().len(), 2);
    assert!(b.partitions().list().await.unwrap().is_empty(), "tenant B sees none of A's");

    let kernels = ManagedKernels::new();
    let shared = Arc::new(InMemoryPartitionStore::new());
    let tenant = |name: &str| {
        TenantKernel::builder(
            name,
            ManagedStores::in_memory(),
            Arc::new(InMemorySessionService::new()),
        )
        .partition_store(shared.clone())
        .build()
        .unwrap()
    };
    kernels.insert(tenant("tenant-c")).unwrap();
    let error = kernels.insert(tenant("tenant-d")).unwrap_err();
    assert!(error.to_string().contains("partition store"), "{error}");
}
