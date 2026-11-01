//! `charter::CharterKit` (spec 3.5 §7.10, §7.11, §7.20; #1278): a product binds each site
//! process kind to its own target and installs the kit on a kernel, which then runs the bound
//! jobs, runs the KPI tracker as its `kernel.kpi` process — in `ps`, held during a freeze — and
//! names every kind no job is bound to in its inventory.

#![cfg(feature = "charter")]

use std::sync::Arc;
use std::time::Duration;

use adk_core::audit_log::AuditActor;
use adk_core::{RunFilter, RunRecord, RunState};
use adk_kernel::charter::{
    CharterKit, InMemoryCharterStore, InMemoryKpiStore, InMemorySignalStore, KPI_TRACKER_TARGET,
    KpiTracker, SiteProcessKind,
};
use adk_kernel::control::ControlPlaneConfig;
use adk_kernel::stores::KernelStores;
use adk_kernel::{Kernel, KernelConfig, ProcessFilter, ProcessKind};
use adk_server::background::DurableWorkerConfig;
use serde_json::json;
use uuid::Uuid;

const BACK_OFFICE: &str = "bakery.back-office";

fn kit(site: Uuid) -> CharterKit {
    CharterKit::new(site, "bakery", BACK_OFFICE, Arc::new(InMemorySignalStore::new()))
}

fn tracker(site: Uuid) -> KpiTracker {
    KpiTracker::new(site, Arc::new(InMemoryCharterStore::new()), Arc::new(InMemoryKpiStore::new()))
}

fn config() -> KernelConfig {
    KernelConfig::default()
        .with_app_name("bakery")
        .with_control(ControlPlaneConfig::default().without_socket())
        .with_worker(DurableWorkerConfig::default().with_poll_interval(Duration::from_millis(20)))
        .with_scheduler_tick(Duration::from_millis(50))
}

async fn kpi_runs(stores: &KernelStores) -> Vec<RunRecord> {
    let runs = stores.runs.list(RunFilter::default()).await.unwrap();
    runs.into_iter().filter(|run| run.target == KPI_TRACKER_TARGET).collect()
}

async fn completed(stores: &KernelStores, at_least: usize) -> Vec<RunRecord> {
    let started = tokio::time::Instant::now();
    loop {
        let done: Vec<RunRecord> = kpi_runs(stores)
            .await
            .into_iter()
            .filter(|run| run.state == RunState::Completed)
            .collect();
        if done.len() >= at_least {
            return done;
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{at_least} tracker runs never completed"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[test]
fn every_kind_no_job_is_bound_to_is_named_in_the_inventory() {
    let site = Uuid::new_v4();
    let kernel = kit(site)
        .job(SiteProcessKind::Probe, "website.guardian.probe", json!({ "site": site }))
        .job(SiteProcessKind::Digest, "website.steward.digest", json!({ "site": site }))
        .skip(SiteProcessKind::Outreach, "the site has no model")
        .install(Kernel::builder().config(config()).stores(&KernelStores::in_memory()))
        .build()
        .unwrap();
    let inventory = kernel.inventory();
    let triggers: Vec<&str> = inventory.triggers.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(triggers, ["site.probe", "site.digest"]);
    assert!(inventory.triggers.iter().all(|t| t.partition.as_str() == BACK_OFFICE));
    let unbound: Vec<(&str, &str)> = inventory
        .warnings
        .iter()
        .filter(|warning| warning.code == CharterKit::JOB_UNBOUND)
        .map(|warning| (warning.subject.as_deref().unwrap(), warning.message.as_str()))
        .collect();
    let kinds: Vec<&str> = unbound.iter().map(|(kind, _)| *kind).collect();
    assert_eq!(
        kinds,
        ["site.watch", "site.content", "site.reviews", "site.outreach", "site.support"]
    );
    let outreach = unbound.iter().find(|(kind, _)| *kind == "site.outreach").unwrap().1;
    assert!(outreach.ends_with("the site has no model"), "{outreach}");
    assert_eq!(inventory.signal_partitions, [BACK_OFFICE.into()]);
}

#[test]
fn a_bound_signal_kind_hears_the_kits_signals() {
    let site = Uuid::new_v4();
    let kernel = kit(site)
        .job(SiteProcessKind::Content, "growth", json!({ "text": "Close the gap." }))
        .install(Kernel::builder().config(config()).stores(&KernelStores::in_memory()))
        .build()
        .unwrap();
    let inventory = kernel.inventory();
    assert!(
        inventory
            .warnings
            .iter()
            .all(|w| w.code != adk_kernel::inventory::InventoryWarning::SIGNAL_UNFILED),
        "{:?}",
        inventory.warnings
    );
    let content = &inventory.triggers[0];
    assert_eq!((content.target.as_str(), content.queue.as_str()), ("growth", "background"));
    assert!(kernel.signal_sink(BACK_OFFICE).is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_kpi_tracker_is_a_kernel_process_held_during_a_freeze() {
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    let stores = KernelStores::in_memory();
    // Two sites on one kernel, as an agency runs them: one executor serves both trackers.
    let builder = Kernel::builder().config(config()).stores(&stores);
    let builder = kit(a).tracker(tracker(a)).tracker_cron("* * * * * *").install(builder);
    let kernel =
        kit(b).tracker(tracker(b)).tracker_cron("* * * * * *").install(builder).build().unwrap();
    let inventory = kernel.inventory();
    let served: Vec<bool> = inventory
        .triggers
        .iter()
        .filter(|t| t.target == KPI_TRACKER_TARGET)
        .map(|t| t.served)
        .collect();
    assert_eq!(served, [true, true]);
    kernel.start().await.unwrap();

    let started = tokio::time::Instant::now();
    loop {
        let sites: std::collections::BTreeSet<String> = completed(&stores, 1)
            .await
            .iter()
            .map(|run| run.input["site"].as_str().unwrap().to_string())
            .collect();
        if sites.len() == 2 {
            break;
        }
        assert!(started.elapsed() < Duration::from_secs(10), "both sites' trackers run");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let processes = kernel.processes().ps(ProcessFilter::default()).await.unwrap();
    assert!(
        processes.iter().any(|process| matches!(
            &process.meta.kind,
            ProcessKind::Triggered { source } if *source == format!("{KPI_TRACKER_TARGET}:{a}")
        )),
        "the tracker's runs are processes in ps"
    );

    // While frozen, a tracker run is not taken.
    kernel.freeze("maintenance", AuditActor::default()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let before = completed(&stores, 0).await.len();
    tokio::time::sleep(Duration::from_millis(2_500)).await;
    assert_eq!(completed(&stores, 0).await.len(), before, "no tracker run completes while frozen");
    kernel.unfreeze(AuditActor::default()).await.unwrap();
    completed(&stores, before + 1).await;
    kernel.stop().await.unwrap();
}
