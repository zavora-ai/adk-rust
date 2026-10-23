//! The in-memory and SQLite driver stores (spec 3.2 §7.7, §11, §13 Q8): the conformance
//! suite on memory and on a file opened as `adk-rust` opens `harness.db`, and the store's
//! migrations beside the process store's on one pool.

#![cfg(all(feature = "drivers", feature = "sqlite"))]

use std::sync::Arc;

use adk_kernel::conformance::DriverStoreHarness;
use adk_kernel::drivers::{DriverManifest, DriverRecord, DriverStore};
use adk_kernel::stores::{InMemoryDriverStore, SqliteDriverStore};
use adk_kernel::{KernelError, SqliteProcessStore};
use adk_session::SqliteSessionService;
use sqlx::sqlite::SqlitePool;

struct InMemory;

#[async_trait::async_trait]
impl DriverStoreHarness for InMemory {
    async fn open(&self) -> Arc<dyn DriverStore> {
        Arc::new(InMemoryDriverStore::new())
    }

    fn is_durable(&self) -> bool {
        false
    }
}

mod in_memory {
    use super::*;

    adk_kernel::driver_store_conformance_tests! { harness = InMemory }
}

/// A database file, removed when the test ends.
struct FileDatabase {
    path: std::path::PathBuf,
}

impl FileDatabase {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("adk-kernel-driver-{}.db", uuid::Uuid::new_v4()));
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
impl DriverStoreHarness for FileDatabase {
    async fn open(&self) -> Arc<dyn DriverStore> {
        let store = SqliteDriverStore::new(self.pool().await);
        store.migrate().await.expect("driver schema migrates");
        Arc::new(store)
    }
}

mod harness_db {
    use super::*;

    adk_kernel::driver_store_conformance_tests! { harness = FileDatabase::new() }
}

async fn count(pool: &SqlitePool, table: &str) -> i64 {
    let (count,): (i64,) =
        sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
            .fetch_one(pool)
            .await
            .expect("count");
    count
}

/// The driver store migrates on one pool beside the process store, each in its own
/// registry, and migrating again changes nothing.
#[tokio::test]
async fn migrations_share_harness_db_with_the_process_store() {
    let database = FileDatabase::new();
    let pool = database.pool().await;
    SqliteProcessStore::new(pool.clone()).migrate().await.expect("process store migrates");
    let store = SqliteDriverStore::new(pool.clone());
    store.migrate().await.expect("migrate");
    store.migrate().await.expect("migrate again");
    assert_eq!(count(&pool, "_adk_kernel_driver_migrations").await, 1);
    assert_eq!(count(&pool, "_adk_kernel_process_migrations").await, 1);
    let manifest = DriverManifest::from_toml("id = \"fs\"\ncommand = \"mcp-fs\"\n").unwrap();
    store.put(DriverRecord::new(manifest, "alice", chrono::Utc::now())).await.expect("put");
    assert_eq!(count(&pool, "adk_kernel_drivers").await, 1);
    assert_eq!(count(&pool, "adk_kernel_processes").await, 0);
}

/// A store used before `migrate` fails with a store error naming the backend.
#[tokio::test]
async fn an_unmigrated_store_fails_with_a_store_error() {
    let pool = SqlitePool::connect("sqlite::memory:").await.expect("SQLite opens");
    let error = SqliteDriverStore::new(pool).get("fs").await.expect_err("no table");
    assert!(matches!(error, KernelError::Store { backend: "sqlite", .. }), "{error}");
}
