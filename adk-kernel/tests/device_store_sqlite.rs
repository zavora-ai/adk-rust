//! The in-memory and SQLite device stores (spec 3.3 §10 K2, §11 S-I7): the conformance
//! suite on memory and on a file opened as `adk-rust` opens `harness.db`, and the store's
//! migrations beside the process store's on one pool.

#![cfg(feature = "sqlite")]

use std::sync::Arc;

use adk_kernel::conformance::DeviceStoreHarness;
use adk_kernel::pairing::{
    DeviceStore, InMemoryDeviceStore, IssueCode, Pairing, SqliteDeviceStore,
};
use adk_kernel::{KernelError, SqliteProcessStore};
use adk_session::SqliteSessionService;
use sqlx::sqlite::SqlitePool;

struct InMemory;

#[async_trait::async_trait]
impl DeviceStoreHarness for InMemory {
    async fn open(&self) -> Arc<dyn DeviceStore> {
        Arc::new(InMemoryDeviceStore::new())
    }

    fn is_durable(&self) -> bool {
        false
    }
}

mod in_memory {
    use super::*;

    adk_kernel::device_store_conformance_tests! { harness = InMemory }
}

/// A database file, removed when the test ends.
struct FileDatabase {
    path: std::path::PathBuf,
}

impl FileDatabase {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("adk-kernel-device-{}.db", uuid::Uuid::new_v4()));
        Self { path }
    }

    /// A new pool over the file, opened as `adk-rust` opens `harness.db`.
    async fn pool(&self) -> SqlitePool {
        let url = format!("sqlite://{}?mode=rwc", self.path.display());
        let sessions = SqliteSessionService::new(&url).await.expect("SQLite opens");
        sessions.migrate().await.expect("session schema migrates");
        sessions.pool().clone()
    }
}

impl Drop for FileDatabase {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm", "-journal"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.path.display()));
        }
    }
}

#[async_trait::async_trait]
impl DeviceStoreHarness for FileDatabase {
    async fn open(&self) -> Arc<dyn DeviceStore> {
        let store = SqliteDeviceStore::new(self.pool().await);
        store.migrate().await.expect("device schema migrates");
        Arc::new(store)
    }
}

mod harness_db {
    use super::*;

    adk_kernel::device_store_conformance_tests! { harness = FileDatabase::new() }
}

async fn count(pool: &SqlitePool, table: &str) -> i64 {
    let (count,): (i64,) =
        sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
            .fetch_one(pool)
            .await
            .expect("count");
    count
}

/// The device store migrates on one pool beside the process store, each in its own
/// registry, migrating again changes nothing, and a device paired through it survives a new
/// pool, as it survives `adk-rust up` restarting.
#[tokio::test]
async fn migrations_share_harness_db_and_devices_survive_a_restart() {
    let database = FileDatabase::new();
    let pool = database.pool().await;
    SqliteProcessStore::new(pool.clone()).migrate().await.expect("process store migrates");
    let store = SqliteDeviceStore::new(pool.clone());
    store.migrate().await.expect("migrate");
    store.migrate().await.expect("migrate again");
    assert_eq!(count(&pool, "_adk_kernel_device_migrations").await, 3);
    assert_eq!(count(&pool, "_adk_kernel_process_migrations").await, 2);

    let pairing = Pairing::new(Arc::new(store));
    let issued = pairing.issue(IssueCode::new("ana", ["operator"])).await.expect("issue");
    assert_eq!(count(&pool, "adk_kernel_pairing_codes").await, 1);
    let claimed = pairing.claim(&issued.code, Some("Pixel")).await.expect("claim");
    assert_eq!(count(&pool, "adk_kernel_pairing_codes").await, 0, "a claim takes the code");

    let reopened = SqliteDeviceStore::new(database.pool().await);
    reopened.migrate().await.expect("migrate");
    let device = Pairing::new(Arc::new(reopened))
        .authenticate(&claimed.token)
        .await
        .expect("the token works");
    assert_eq!(device.name, "Pixel");
}

/// A store used before `migrate` fails with a store error naming the backend.
#[tokio::test]
async fn an_unmigrated_store_fails_with_a_store_error() {
    let pool = SqlitePool::connect("sqlite::memory:").await.expect("SQLite opens");
    let error = SqliteDeviceStore::new(pool).get_device("abc").await.expect_err("no table");
    assert!(matches!(error, KernelError::Store { backend: "sqlite", .. }), "{error}");
}
