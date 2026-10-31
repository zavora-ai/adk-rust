//! `Kernel::inventory` (spec 3.2 §7.1, §7.4; #1278): what a kernel runs, with a warning for
//! each thing it would silently not do — a trigger whose target nothing serves, a signal
//! trigger in a partition the kernel files no signals in, a store held in memory, a kernel with
//! nothing to run — logged at start and served at `GET /api/kernel/inventory`.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use adk_core::schedule::MissedFirePolicy;
use adk_core::{InMemoryRunStore, NewRun, RunFilter, RunState};
use adk_kernel::control::api::IssueRequest;
use adk_kernel::control::{ControlClient, ControlPlane, ControlPlaneConfig};
use adk_kernel::inventory::{InventoryWarning, KernelInventory};
use adk_kernel::pairing::{DeviceScope, InMemoryDeviceStore, Pairing};
use adk_kernel::stores::KernelStores;
use adk_kernel::{Kernel, KernelBuilder, KernelConfig, Trigger, Wake};
use adk_server::background::{DurableWorkerConfig, WorkflowExecutor, WorkflowState};
use async_trait::async_trait;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

/// Serves the workflow targets it names, each run answering at once.
struct Targets(Vec<&'static str>);

#[async_trait]
impl WorkflowExecutor for Targets {
    fn has_workflow(&self, workflow_id: &str) -> bool {
        self.0.contains(&workflow_id)
    }

    async fn execute(
        &self,
        _workflow_id: &str,
        _input: WorkflowState,
        _cancel_token: CancellationToken,
    ) -> Result<Value, String> {
        Ok(json!({ "done": true }))
    }
}

fn run(target: &str) -> NewRun {
    NewRun::new("background", "ops", "kernel", target, json!({ "text": "go" }))
}

fn quiet() -> KernelConfig {
    KernelConfig::default()
        .with_app_name("ops")
        .with_control(ControlPlaneConfig::default().without_socket())
        .with_worker(DurableWorkerConfig::default().with_poll_interval(Duration::from_millis(20)))
        .with_scheduler_tick(Duration::from_millis(50))
}

fn kernel() -> KernelBuilder {
    Kernel::builder().config(quiet()).stores(&KernelStores::in_memory())
}

fn codes(inventory: &KernelInventory) -> Vec<(&str, Option<&str>)> {
    inventory
        .warnings
        .iter()
        .map(|warning| (warning.code.as_str(), warning.subject.as_deref()))
        .collect()
}

#[test]
fn a_kernel_with_nothing_to_run_and_defaulted_stores_says_so() {
    let kernel = Kernel::builder()
        .config(quiet())
        .run_store(Arc::new(InMemoryRunStore::new()))
        .build()
        .unwrap();
    let inventory = kernel.inventory();
    let store = InventoryWarning::STORE_IN_MEMORY;
    // No control plane is served, so the settings store goes unread and unwarned.
    assert_eq!(
        codes(&inventory),
        [
            (store, Some("process_store")),
            (store, Some("agenda_store")),
            (store, Some("partitions")),
            (store, Some("watermark_store")),
            (store, Some("agent_store")),
            (store, Some("governance")),
            (InventoryWarning::KERNEL_IDLE, None),
        ]
    );
    assert!(inventory.warnings[0].message.contains("KernelBuilder::process_store"));
    assert_eq!(inventory.app_name, "ops");
    assert!(inventory.stores.iter().any(|used| used.store == "control_store" && used.in_memory));
}

#[test]
fn an_in_memory_bundle_is_one_warning_naming_its_stores() {
    let kernel = kernel().workflows(Arc::new(Targets(vec!["digest"]))).build().unwrap();
    let inventory = kernel.inventory();
    assert_eq!(codes(&inventory), [(InventoryWarning::STORE_IN_MEMORY, Some("memory"))]);
    let message = &inventory.warnings[0].message;
    assert!(message.contains("run_store") && message.contains("process_store"), "{message}");
}

#[test]
fn a_trigger_whose_target_nothing_serves_is_warned() {
    let kernel = kernel()
        .workflows(Arc::new(Targets(vec!["digest"])))
        .trigger(Trigger::new(
            "morning",
            Wake::Cron { expr: "0 0 9 * * *".into(), missed: MissedFirePolicy::Skip },
            run("digest"),
        ))
        .trigger(Trigger::new("orders", Wake::Webhook { hook: "orders".into() }, run("triage")))
        .build()
        .unwrap();
    let inventory = kernel.inventory();
    let served: Vec<(&str, bool)> =
        inventory.triggers.iter().map(|trigger| (trigger.name.as_str(), trigger.served)).collect();
    assert_eq!(served, [("morning", true), ("orders", false)]);
    let unserved = inventory
        .warnings
        .iter()
        .find(|warning| warning.code == InventoryWarning::TRIGGER_UNSERVED)
        .expect("the orders trigger is warned");
    assert_eq!(unserved.subject.as_deref(), Some("orders"));
    assert!(unserved.message.contains("`triage`"), "{}", unserved.message);
    assert!(inventory.wake_sources.iter().any(|source| source.starts_with("trigger-cron:")));
}

#[test]
fn a_hosts_warning_joins_the_inventory() {
    let kernel = kernel()
        .workflows(Arc::new(Targets(vec![])))
        .inventory_warning(
            InventoryWarning::new("site.job_unscheduled", "the digest is not scheduled")
                .with_subject("site.digest"),
        )
        .build()
        .unwrap();
    assert!(codes(&kernel.inventory()).contains(&("site.job_unscheduled", Some("site.digest"))));
}

#[cfg(feature = "charter")]
mod signals {
    use adk_kernel::charter::{InMemorySignalStore, SignalOutcome};
    use awp_types::{Bounded, Signal, SignalKind};

    use super::*;

    fn on_gaps(partition: Option<&str>) -> Trigger {
        Trigger::new(
            "site.content",
            Wake::Signal { kind: "content_gap".into(), partition: partition.map(Into::into) },
            run("site.content"),
        )
        .with_partition("back-office")
    }

    fn gap() -> Signal {
        Signal {
            id: uuid::Uuid::new_v4(),
            kind: SignalKind::ContentGap {
                topic: Bounded::new("parking", 80).unwrap(),
                question_count: 1,
                samples: vec![],
            },
            emitted_by: "concierge".into(),
            emitted_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn a_signal_trigger_is_warned_until_its_partition_files_signals() {
        let builder = || kernel().workflows(Arc::new(Targets(vec!["site.content"])));
        for partition in [Some("back-office"), None] {
            let kernel = builder().trigger(on_gaps(partition)).build().unwrap();
            let unfiled: Vec<_> = kernel
                .inventory()
                .warnings
                .into_iter()
                .filter(|warning| warning.code == InventoryWarning::SIGNAL_UNFILED)
                .collect();
            assert_eq!(unfiled.len(), 1, "{partition:?}");
            assert_eq!(unfiled[0].subject.as_deref(), Some("site.content"));
        }
        let elsewhere = builder()
            .trigger(on_gaps(Some("back-office")))
            .signals(Arc::new(InMemorySignalStore::new()), "public")
            .build()
            .unwrap();
        assert!(
            codes(&elsewhere.inventory())
                .contains(&(InventoryWarning::SIGNAL_UNFILED, Some("site.content"))),
            "a sink in another partition does not file this trigger's signals"
        );
        let filed = builder()
            .trigger(on_gaps(Some("back-office")))
            .signals(Arc::new(InMemorySignalStore::new()), "back-office")
            .build()
            .unwrap();
        let inventory = filed.inventory();
        assert!(inventory.warnings.iter().all(|w| w.code != InventoryWarning::SIGNAL_UNFILED));
        assert_eq!(inventory.signal_partitions, ["back-office".into()]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_kernels_signal_sink_starts_the_signal_trigger() {
        let stores = KernelStores::in_memory();
        let kernel = Kernel::builder()
            .config(quiet())
            .stores(&stores)
            .workflows(Arc::new(Targets(vec!["site.content"])))
            .trigger(on_gaps(Some("back-office")))
            .signals(Arc::new(InMemorySignalStore::new()), "back-office")
            .build()
            .unwrap();
        kernel.start().await.unwrap();
        let sink = kernel.signal_sink("back-office").unwrap();
        assert!(matches!(sink.emit(gap()).await.unwrap(), SignalOutcome::Filed { count: 1, .. }));
        let started = tokio::time::Instant::now();
        let done = loop {
            let runs = stores.runs.list(RunFilter::default()).await.unwrap();
            if let Some(run) = runs
                .into_iter()
                .find(|run| run.target == "site.content" && run.state == RunState::Completed)
            {
                break run;
            }
            assert!(started.elapsed() < Duration::from_secs(10), "the signal trigger never ran");
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        kernel.stop().await.unwrap();
        assert!(
            done.idempotency_key
                .as_deref()
                .is_some_and(|key| key.starts_with("signal:back-office:"))
        );
        assert!(!done.authenticated, "a signal carries no principal");
        let item = stores.agenda.list(&Default::default()).await.unwrap();
        assert_eq!(item.len(), 1, "the signal is an agenda item of the back office");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_operator_reads_the_inventory_over_the_control_plane() {
    let kernel = Kernel::builder()
        .config(
            quiet().with_control(
                ControlPlaneConfig::default()
                    .without_socket()
                    .with_http("127.0.0.1:0".parse().unwrap())
                    .with_pairing(Pairing::new(Arc::new(InMemoryDeviceStore::new()))),
            ),
        )
        .stores(&KernelStores::in_memory())
        .trigger(Trigger::new("orders", Wake::Webhook { hook: "orders".into() }, run("triage")))
        .build()
        .unwrap();
    let plane = ControlPlane::start(&kernel).await.unwrap();
    let operator = ControlClient::http(format!("http://{}", plane.http_addr().unwrap()));
    let inventory = operator.inventory().await.unwrap();
    assert_eq!(inventory, kernel.inventory());
    assert_eq!(inventory.triggers[0].target, "triage");

    // A paired device, even one that reads the process table, is not the operator.
    let request = IssueRequest {
        name: Some("phone".into()),
        scopes: vec![DeviceScope::Read],
        ..IssueRequest::default()
    };
    let issued = operator.issue_pairing_code(&request).await.unwrap();
    let base = format!("http://{}", plane.http_addr().unwrap());
    let claimed =
        ControlClient::http(base.clone()).claim_pairing(&issued.code, None).await.unwrap();
    let device = ControlClient::http(base).with_bearer_token(claimed.token);
    assert!(device.health().await.is_ok());
    assert!(device.inventory().await.is_err(), "the inventory is the operator's");
}

#[test]
fn the_kernel_logs_its_warnings_when_it_starts() {
    use tracing_subscriber::layer::SubscriberExt as _;

    let logs = Captured::default();
    let writer = logs.clone();
    let subscriber = tracing_subscriber::registry().with(
        tracing_subscriber::fmt::layer().with_ansi(false).with_writer(move || writer.clone()),
    );
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    tracing::subscriber::with_default(subscriber, || {
        runtime.block_on(async {
            let kernel = kernel()
                .trigger(Trigger::new(
                    "orders",
                    Wake::Webhook { hook: "orders".into() },
                    run("triage"),
                ))
                .build()
                .unwrap();
            kernel.start().await.unwrap();
            kernel.stop().await.unwrap();
        });
    });
    let text = logs.text();
    assert!(text.contains("kernel started"), "{text}");
    for code in [InventoryWarning::TRIGGER_UNSERVED, InventoryWarning::STORE_IN_MEMORY] {
        assert!(
            text.lines().any(|line| line.contains("WARN") && line.contains(code)),
            "no warning {code} in: {text}"
        );
    }
}

/// A `MakeWriter` into a shared buffer.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Captured {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap_or_else(PoisonError::into_inner)).into_owned()
    }
}

impl std::io::Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
