//! The kernel's MongoDB stores (spec 3.2 §7.1, §11, §13 Q8): the process, agenda, driver,
//! ledger and partition conformance suites against a live server, and the stores sharing one
//! database with adk-session's run, schedule and governance stores.
//!
//! The tests are `#[ignore]`d because they need a server; CI's nightly `durable-stores` job
//! runs them on a single-node replica set and on a standalone server. Each case opens a
//! database of its own, and every `open` is a new client, as a restarted or second replica
//! would create:
//!
//! ```bash
//! MONGODB_URL=mongodb://localhost:27017 \
//!   cargo nextest run -p adk-kernel --features mongodb --run-ignored only \
//!   --test store_conformance_mongodb
//! ```

#![cfg(feature = "mongodb")]

use std::sync::Arc;

use adk_core::{DurableRunStore, GovernanceStateStore, ScheduleStore};
use adk_kernel::conformance::{AgendaStoreHarness, ProcessStoreHarness};
use adk_kernel::{
    AgendaStore, MongoAgendaStore, MongoProcessStore, ProcessKind, ProcessMeta, ProcessStore,
    QueueClass,
};

fn url() -> String {
    std::env::var("MONGODB_URL").unwrap_or_else(|_| "mongodb://localhost:27017".to_string())
}

/// A database of its own.
struct Database(String);

impl Database {
    fn new() -> Self {
        Self(format!("adk_kernel_{}", uuid::Uuid::new_v4().simple()))
    }
}

#[async_trait::async_trait]
impl ProcessStoreHarness for Database {
    async fn open(&self) -> Arc<dyn ProcessStore> {
        let store = MongoProcessStore::connect(&url(), &self.0).await.expect("connect");
        store.migrate().await.expect("migrate");
        Arc::new(store)
    }
}

#[async_trait::async_trait]
impl AgendaStoreHarness for Database {
    async fn open(&self) -> Arc<dyn AgendaStore> {
        let store = MongoAgendaStore::connect(&url(), &self.0).await.expect("connect");
        store.migrate().await.expect("migrate");
        Arc::new(store)
    }
}

#[cfg(feature = "drivers")]
#[async_trait::async_trait]
impl adk_kernel::conformance::DriverStoreHarness for Database {
    async fn open(&self) -> Arc<dyn adk_kernel::drivers::DriverStore> {
        let store =
            adk_kernel::stores::MongoDriverStore::connect(&url(), &self.0).await.expect("connect");
        store.migrate().await.expect("migrate");
        Arc::new(store)
    }
}

#[cfg(feature = "proc")]
#[async_trait::async_trait]
impl adk_kernel::conformance::LedgerStoreHarness for Database {
    async fn open(&self) -> Arc<dyn adk_kernel::proc::LedgerStore> {
        let store =
            adk_kernel::proc::MongoLedgerStore::connect(&url(), &self.0).await.expect("connect");
        store.migrate().await.expect("migrate");
        Arc::new(store)
    }
}

#[async_trait::async_trait]
impl adk_kernel::conformance::PartitionStoreHarness for Database {
    async fn open(&self) -> Arc<dyn adk_kernel::PartitionStore> {
        let store =
            adk_kernel::MongoPartitionStore::connect(&url(), &self.0).await.expect("connect");
        store.migrate().await.expect("migrate");
        Arc::new(store)
    }
}

mod process {
    use super::*;

    adk_kernel::process_store_conformance_tests! {
        #[ignore = "needs MONGODB_URL; CI's nightly durable-stores job runs it"]
        harness = Database::new()
    }
}

mod agenda {
    use super::*;

    adk_kernel::agenda_store_conformance_tests! {
        #[ignore = "needs MONGODB_URL; CI's nightly durable-stores job runs it"]
        harness = Database::new()
    }
}

#[cfg(feature = "drivers")]
mod drivers {
    use super::*;

    adk_kernel::driver_store_conformance_tests! {
        #[ignore = "needs MONGODB_URL; CI's nightly durable-stores job runs it"]
        harness = Database::new()
    }
}

#[cfg(feature = "proc")]
mod ledger {
    use super::*;

    adk_kernel::ledger_store_conformance_tests! {
        #[ignore = "needs MONGODB_URL; CI's nightly durable-stores job runs it"]
        harness = Database::new()
    }
}

mod partition {
    use super::*;

    adk_kernel::partition_store_conformance_tests! {
        #[ignore = "needs MONGODB_URL; CI's nightly durable-stores job runs it"]
        harness = Database::new()
    }
}

/// One database holds the kernel's stores beside adk-session's run, schedule and
/// governance stores, all over one client, and `migrate` runs again without error.
#[tokio::test]
#[ignore = "needs MONGODB_URL; CI's nightly durable-stores job runs it"]
async fn one_database_holds_the_kernel_and_run_stores() {
    let name = Database::new().0;
    let processes = MongoProcessStore::connect(&url(), &name).await.expect("connect");
    let db = processes.database().clone();
    let runs = adk_session::MongoRunStore::from_database(db.clone());
    let schedules = adk_session::MongoScheduleStore::from_database(db.clone());
    let governance = adk_session::MongoGovernanceStateStore::from_database(db.clone());
    let agenda = MongoAgendaStore::from_database(db.clone());
    for _ in 0..2 {
        runs.migrate().await.expect("migrate the run store");
        schedules.migrate().await.expect("migrate the schedule store");
        governance.migrate().await.expect("migrate the governance store");
        processes.migrate().await.expect("migrate the process store");
        agenda.migrate().await.expect("migrate the agenda store");
    }
    let meta = ProcessMeta::new("run-1", QueueClass::Background, ProcessKind::Goal);
    processes.put(meta.clone()).await.expect("put");
    assert_eq!(processes.get("run-1").await.expect("get"), Some(meta));
    assert!(runs.list(Default::default()).await.expect("list runs").is_empty());
    assert!(schedules.list().await.expect("list schedules").is_empty());
    assert!(governance.list().await.expect("list governance").is_empty());
    let collections = db.list_collection_names().await.expect("list collections");
    assert!(collections.iter().any(|name| name == "adk_kernel_processes"), "{collections:?}");
}

/// An unreachable server answers with a store error naming the backend, not a panic.
#[tokio::test]
async fn an_unreachable_server_is_a_store_error() {
    let store = MongoProcessStore::connect(
        "mongodb://127.0.0.1:1/?serverSelectionTimeoutMS=200&connectTimeoutMS=200",
        "adk_kernel_unreachable",
    )
    .await
    .expect("a client is built without a server");
    let error = store.get("run-1").await.expect_err("no server");
    assert!(matches!(&error, adk_kernel::KernelError::Store { backend: "mongodb", .. }), "{error}");
}
