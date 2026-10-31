//! `KernelStores` and `KernelBuilder::require_durable` (spec 3.2 §7.1, §13 Q8; #1278): one
//! bundle of every stateful kernel store, opened on one backend, that the builder takes; a
//! kernel that requires durable stores refuses to keep one in memory and names it; a store
//! the bundle opened outlives the process that opened it.
//!
//! The PostgreSQL, Redis, MongoDB, Neo4j and Firestore round trips are `#[ignore]`d because
//! they need a server:
//!
//! ```bash
//! DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres REDIS_URL=redis://localhost:6379 \
//!   cargo nextest run -p adk-kernel --features postgres,redis --run-ignored only \
//!   --test integration -E 'test(/^kernel_stores::/)'
//! ```

use std::sync::Arc;

use adk_core::InMemoryRunStore;
use adk_kernel::stores::{KernelStores, StoreOrigin, StoreUse};
use adk_kernel::{
    InMemoryProcessStore, Kernel, KernelError, ProcessKind, ProcessMeta, ProcessStore, QueueClass,
};

fn reason(error: KernelError) -> String {
    match error {
        KernelError::Config { reason } => reason,
        other => panic!("expected a configuration error, got {other}"),
    }
}

fn used<'a>(kernel: &'a Kernel, store: &str) -> &'a StoreUse {
    kernel.store_uses().iter().find(|used| used.store == store).unwrap_or_else(|| {
        panic!("no `{store}` in {:?}", kernel.store_uses());
    })
}

#[cfg(feature = "sqlite")]
fn sqlite_url(dir: &tempfile::TempDir) -> String {
    format!("sqlite://{}?mode=rwc", dir.path().join("kernel.db").display())
}

#[test]
fn require_durable_names_every_store_the_builder_would_default() {
    let refused = Kernel::builder()
        .run_store(Arc::new(InMemoryRunStore::new()))
        .require_durable()
        .build()
        .unwrap_err();
    let reason = reason(refused);
    for store in [
        "`process_store`",
        "`agenda_store`",
        "`partitions`",
        "`watermark_store`",
        "`agent_store`",
        "`governance`",
        "`control_store`",
    ] {
        assert!(reason.contains(store), "expected {store} in: {reason}");
    }
    assert!(reason.contains("KernelBuilder::process_store"), "{reason}");
    assert!(!reason.contains("`run_store`"), "a run store set is taken as durable: {reason}");
}

#[test]
fn require_durable_refuses_an_in_memory_bundle_naming_it() {
    let reason = reason(
        Kernel::builder().stores(&KernelStores::in_memory()).require_durable().build().unwrap_err(),
    );
    assert!(reason.contains("`run_store` (the stores `memory` keep it in memory)"), "{reason}");
    assert!(reason.contains("`governance`"), "{reason}");
}

#[test]
fn without_require_durable_a_defaulted_store_is_recorded_and_the_kernel_builds() {
    let kernel = Kernel::builder().run_store(Arc::new(InMemoryRunStore::new())).build().unwrap();
    let defaulted: Vec<&str> = kernel
        .store_uses()
        .iter()
        .filter(|used| used.origin == StoreOrigin::Defaulted)
        .map(|used| used.store.as_str())
        .collect();
    assert_eq!(
        defaulted,
        [
            "process_store",
            "agenda_store",
            "partitions",
            "watermark_store",
            "agent_store",
            "governance",
            "control_store"
        ]
    );
    assert_eq!(used(&kernel, "run_store").origin, StoreOrigin::Set);
    assert!(kernel.store_uses().iter().all(|used| used.store != "schedule_store"));
}

#[tokio::test]
async fn a_store_set_on_the_builder_wins_over_the_bundles_whichever_comes_first() {
    let mine: Arc<dyn ProcessStore> = Arc::new(InMemoryProcessStore::new());
    let stores = KernelStores::in_memory();
    for kernel in [
        Kernel::builder().process_store(Arc::clone(&mine)).stores(&stores).build().unwrap(),
        Kernel::builder().stores(&stores).process_store(Arc::clone(&mine)).build().unwrap(),
    ] {
        assert_eq!(used(&kernel, "process_store").origin, StoreOrigin::Set);
        let run = used(&kernel, "run_store");
        assert_eq!(
            (run.origin, run.in_memory, run.bundle.as_deref()),
            (StoreOrigin::Bundle, true, Some("memory"))
        );
        mine.put(ProcessMeta::new("run-1", QueueClass::Background, ProcessKind::Goal))
            .await
            .unwrap();
        assert!(kernel.processes().process_store().get("run-1").await.unwrap().is_some());
    }
}

#[cfg(feature = "sqlite")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_durable_bundle_satisfies_require_durable_with_a_mirrored_kill_switch() {
    let dir = tempfile::tempdir().unwrap();
    let stores = KernelStores::open(&sqlite_url(&dir)).await.unwrap();
    let kernel = Kernel::builder()
        .config(
            adk_kernel::KernelConfig::default()
                .with_home(dir.path())
                .with_control(adk_kernel::control::ControlPlaneConfig::default().without_socket()),
        )
        .stores(&stores)
        .require_durable()
        .build()
        .unwrap();
    assert!(kernel.store_uses().iter().all(|used| !used.in_memory), "{:?}", kernel.store_uses());
    assert!(kernel.config().governance.is_mirrored());
    assert!(!kernel.config().control.has_default_control_store());
    assert_eq!(used(&kernel, "governance").origin, StoreOrigin::Bundle);
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn the_sqlite_bundle_survives_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let url = sqlite_url(&dir);
    round_trip(|| async { KernelStores::open(&url).await.unwrap() }).await;
}

/// Writes through one bundle and reads through a second opened the same way, as a restarted
/// or second replica would.
#[cfg(any(feature = "sqlite", feature = "postgres", feature = "redis"))]
async fn round_trip<F, Fut>(open: F)
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = KernelStores>,
{
    use adk_core::NewRun;
    use adk_kernel::Partition;
    use chrono::{TimeZone as _, Utc};

    let id = uuid::Uuid::new_v4().simple().to_string();
    let pid = format!("run-{id}");
    let partition = format!("work-{id}");
    let source = format!("cron-{id}");
    let cursor = Utc.with_ymd_and_hms(2026, 11, 1, 9, 0, 0).unwrap();
    let first = open().await;
    assert!(first.is_durable(), "{:?}", first.in_memory_stores());
    let run = first
        .runs
        .enqueue(NewRun::new("background", "adk-rust", "kernel", "digest", serde_json::json!({})))
        .await
        .unwrap()
        .record()
        .cloned()
        .expect("a new run");
    first
        .processes
        .put(ProcessMeta::new(pid.clone(), QueueClass::Background, ProcessKind::Goal))
        .await
        .unwrap();
    first.partitions.put(Partition::new(partition.as_str(), "work")).await.unwrap();
    first.watermarks.advance(&source, cursor).await.unwrap();
    drop(first);

    let second = open().await;
    assert!(second.runs.get(&run.run_id).await.unwrap().is_some());
    assert!(second.processes.get(&pid).await.unwrap().is_some());
    assert!(second.partitions.get(&partition.as_str().into()).await.unwrap().is_some());
    assert_eq!(second.watermarks.read(&source).await.unwrap(), Some(cursor));
}

#[cfg(feature = "postgres")]
#[tokio::test]
#[ignore = "needs PostgreSQL at DATABASE_URL"]
async fn the_postgres_bundle_survives_a_restart() {
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres@localhost:5432/postgres".to_string());
    round_trip(|| async { KernelStores::open(&url).await.unwrap() }).await;
}

#[cfg(feature = "redis")]
#[tokio::test]
#[ignore = "needs Redis at REDIS_URL"]
async fn the_redis_bundle_survives_a_restart() {
    let url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_string());
    round_trip(|| async { KernelStores::open(&url).await.unwrap() }).await;
}

/// The stores without a backend on MongoDB, Firestore and Neo4j, named as in memory.
#[cfg(any(feature = "mongodb", feature = "firestore", feature = "neo4j"))]
async fn partial_round_trip(url: &str) {
    let stores = KernelStores::open(url).await.unwrap();
    assert!(stores.is_in_memory("devices") && stores.is_in_memory("control"));
    assert!(!stores.is_in_memory("processes") && !stores.is_in_memory("runs"));
}

#[cfg(feature = "mongodb")]
#[tokio::test]
#[ignore = "needs MongoDB at MONGODB_URL"]
async fn the_mongodb_bundle_keeps_what_mongodb_holds() {
    let url = std::env::var("MONGODB_URL").unwrap_or_else(|_| "mongodb://localhost:27017".into());
    let url = format!("{}/adk_kernel_{}", url.trim_end_matches('/'), uuid::Uuid::new_v4().simple());
    partial_round_trip(&url).await;
}

#[cfg(feature = "neo4j")]
#[tokio::test]
#[ignore = "needs Neo4j at NEO4J_URL"]
async fn the_neo4j_bundle_keeps_what_neo4j_holds() {
    let url = std::env::var("NEO4J_URL")
        .unwrap_or_else(|_| "neo4j://neo4j:password@localhost:7687".into());
    partial_round_trip(&url).await;
}

#[cfg(feature = "firestore")]
#[tokio::test]
#[ignore = "needs the Firestore emulator at FIRESTORE_EMULATOR_HOST"]
async fn the_firestore_bundle_keeps_what_firestore_holds() {
    partial_round_trip("firestore://adk-kernel-test").await;
}
