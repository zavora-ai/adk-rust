//! The kernel's Firestore stores (spec 3.2 §7.1, §7.4, §11, §13 Q8): the process, agenda,
//! driver, ledger, partition and cron watermark conformance suites against the Firestore
//! emulator, the identifier limit, and the stores sharing one client with adk-session's run and
//! schedule stores.
//!
//! The tests are `#[ignore]`d because they need the emulator; CI's nightly `durable-stores`
//! job runs them. Every case writes under fresh identifiers, so the cases share one
//! emulator database, and every `open` is a new client, as a restarted or second replica
//! would create:
//!
//! ```bash
//! gcloud emulators firestore start --host-port=127.0.0.1:8080 &
//! FIRESTORE_EMULATOR_HOST=127.0.0.1:8080 \
//!   cargo nextest run -p adk-kernel --features firestore --run-ignored only \
//!   --test store_conformance_firestore
//! ```
//!
//! `FIRESTORE_PROJECT_ID` names the emulator project (default `adk-conformance`).

#![cfg(feature = "firestore")]

use std::sync::Arc;

use adk_core::{DurableRunStore, ScheduleStore};
use adk_kernel::conformance::{AgendaStoreHarness, ProcessStoreHarness};
use adk_kernel::{
    AgendaStore, FirestoreAgendaStore, FirestoreProcessStore, KernelError, ProcessKind,
    ProcessMeta, ProcessStore, QueueClass,
};
use adk_session::firestore_stores::connect_firestore_emulator;
use firestore::FirestoreDb;

/// A new client per call, as a restarted or second replica would create.
async fn emulator_db() -> FirestoreDb {
    let project =
        std::env::var("FIRESTORE_PROJECT_ID").unwrap_or_else(|_| "adk-conformance".to_string());
    connect_firestore_emulator(&project).await.expect("connect to the Firestore emulator")
}

struct Emulator;

#[async_trait::async_trait]
impl ProcessStoreHarness for Emulator {
    async fn open(&self) -> Arc<dyn ProcessStore> {
        let store = FirestoreProcessStore::from_db(emulator_db().await);
        store.migrate().await.expect("migrate");
        Arc::new(store)
    }
}

#[async_trait::async_trait]
impl AgendaStoreHarness for Emulator {
    async fn open(&self) -> Arc<dyn AgendaStore> {
        let store = FirestoreAgendaStore::from_db(emulator_db().await);
        store.migrate().await.expect("migrate");
        Arc::new(store)
    }
}

#[cfg(feature = "drivers")]
#[async_trait::async_trait]
impl adk_kernel::conformance::DriverStoreHarness for Emulator {
    async fn open(&self) -> Arc<dyn adk_kernel::drivers::DriverStore> {
        let store = adk_kernel::stores::FirestoreDriverStore::from_db(emulator_db().await);
        store.migrate().await.expect("migrate");
        Arc::new(store)
    }
}

#[cfg(feature = "proc")]
#[async_trait::async_trait]
impl adk_kernel::conformance::LedgerStoreHarness for Emulator {
    async fn open(&self) -> Arc<dyn adk_kernel::proc::LedgerStore> {
        let store = adk_kernel::proc::FirestoreLedgerStore::from_db(emulator_db().await);
        store.migrate().await.expect("migrate");
        Arc::new(store)
    }
}

#[async_trait::async_trait]
impl adk_kernel::conformance::PartitionStoreHarness for Emulator {
    async fn open(&self) -> Arc<dyn adk_kernel::PartitionStore> {
        let store = adk_kernel::FirestorePartitionStore::from_db(emulator_db().await);
        store.migrate().await.expect("migrate");
        Arc::new(store)
    }
}

#[async_trait::async_trait]
impl adk_kernel::conformance::CronWatermarkStoreHarness for Emulator {
    async fn open(&self) -> Arc<dyn adk_kernel::CronWatermarkStore> {
        let store = adk_kernel::FirestoreCronWatermarkStore::from_db(emulator_db().await);
        store.migrate().await.expect("migrate");
        Arc::new(store)
    }
}

mod process {
    use super::*;

    adk_kernel::process_store_conformance_tests! {
        #[ignore = "needs FIRESTORE_EMULATOR_HOST; CI's nightly durable-stores job runs it"]
        harness = Emulator
    }
}

mod agenda {
    use super::*;

    adk_kernel::agenda_store_conformance_tests! {
        #[ignore = "needs FIRESTORE_EMULATOR_HOST; CI's nightly durable-stores job runs it"]
        harness = Emulator
    }
}

#[cfg(feature = "drivers")]
mod drivers {
    use super::*;

    adk_kernel::driver_store_conformance_tests! {
        #[ignore = "needs FIRESTORE_EMULATOR_HOST; CI's nightly durable-stores job runs it"]
        harness = Emulator
    }
}

#[cfg(feature = "proc")]
mod ledger {
    use super::*;

    adk_kernel::ledger_store_conformance_tests! {
        #[ignore = "needs FIRESTORE_EMULATOR_HOST; CI's nightly durable-stores job runs it"]
        harness = Emulator
    }
}

mod partition {
    use super::*;

    adk_kernel::partition_store_conformance_tests! {
        #[ignore = "needs FIRESTORE_EMULATOR_HOST; CI's nightly durable-stores job runs it"]
        harness = Emulator
    }
}

mod watermark {
    use super::*;

    adk_kernel::cron_watermark_store_conformance_tests! {
        #[ignore = "needs FIRESTORE_EMULATOR_HOST; CI's nightly durable-stores job runs it"]
        harness = Emulator
    }
}

/// A pid too long to name a document is refused on write and reads as absent.
#[tokio::test]
#[ignore = "needs FIRESTORE_EMULATOR_HOST; CI's nightly durable-stores job runs it"]
async fn an_id_too_long_for_a_document_name_is_refused() {
    let store = FirestoreProcessStore::from_db(emulator_db().await);
    let pid = "p".repeat(701);
    let error = store
        .put(ProcessMeta::new(pid.as_str(), QueueClass::Background, ProcessKind::Goal))
        .await
        .expect_err("too long");
    assert!(
        matches!(&error, KernelError::Store { backend: "firestore", reason } if reason.contains("700 bytes")),
        "{error}"
    );
    assert_eq!(store.get(&pid).await.expect("get"), None);
    assert!(!store.delete(&pid).await.expect("delete"));
}

/// A cron source name too long to name a document is refused on advance and reads as absent.
#[tokio::test]
#[ignore = "needs FIRESTORE_EMULATOR_HOST; CI's nightly durable-stores job runs it"]
async fn a_source_too_long_for_a_document_name_is_refused() {
    use adk_kernel::CronWatermarkStore as _;

    let store = adk_kernel::FirestoreCronWatermarkStore::from_db(emulator_db().await);
    let source = "s".repeat(701);
    let error = store.advance(&source, chrono::Utc::now()).await.expect_err("too long");
    assert!(
        matches!(&error, KernelError::Store { backend: "firestore", reason } if reason.contains("700 bytes")),
        "{error}"
    );
    assert_eq!(store.read(&source).await.expect("read"), None);
    assert!(!store.delete(&source).await.expect("delete"));
}

/// One client serves the kernel's stores beside adk-session's run and schedule stores.
#[tokio::test]
#[ignore = "needs FIRESTORE_EMULATOR_HOST; CI's nightly durable-stores job runs it"]
async fn one_client_serves_the_kernel_and_run_stores() {
    let db = emulator_db().await;
    let runs = adk_session::FirestoreRunStore::from_db(db.clone());
    let schedules = adk_session::FirestoreScheduleStore::from_db(db.clone());
    let processes = FirestoreProcessStore::from_db(db);
    let pid = format!("run-{}", uuid::Uuid::new_v4());
    let meta = ProcessMeta::new(pid.as_str(), QueueClass::Background, ProcessKind::Goal);
    processes.put(meta.clone()).await.expect("put");
    assert_eq!(processes.get(&pid).await.expect("get"), Some(meta));
    assert!(runs.get(&pid).await.expect("read the run store").is_none());
    schedules.list().await.expect("list schedules");
    assert!(processes.delete(&pid).await.expect("delete"));
}
