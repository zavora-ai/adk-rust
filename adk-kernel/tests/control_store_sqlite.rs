//! The in-memory and SQLite control stores (spec 3.3 §10 K3, K4): the conformance suite on
//! memory and on a file opened as `adk-rust` opens `harness.db`, and the store's migrations
//! beside the process store's on one pool.

#![cfg(feature = "sqlite")]

use std::sync::Arc;

use adk_kernel::conformance::ControlStoreHarness;
use adk_kernel::control::{
    AreaSettings, AwayCursor, Change, ControlStore, InMemoryControlStore, SqliteControlStore,
};
use adk_kernel::{KernelError, PartitionMode, SqliteProcessStore};
use adk_session::SqliteSessionService;
use sqlx::sqlite::SqlitePool;

struct InMemory;

#[async_trait::async_trait]
impl ControlStoreHarness for InMemory {
    async fn open(&self) -> Arc<dyn ControlStore> {
        Arc::new(InMemoryControlStore::new())
    }

    fn is_durable(&self) -> bool {
        false
    }
}

mod in_memory {
    use super::*;

    adk_kernel::control_store_conformance_tests! { harness = InMemory }
}

/// A database file, removed when the test ends.
struct FileDatabase {
    path: std::path::PathBuf,
}

impl FileDatabase {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("adk-kernel-control-{}.db", uuid::Uuid::new_v4()));
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
impl ControlStoreHarness for FileDatabase {
    async fn open(&self) -> Arc<dyn ControlStore> {
        let store = SqliteControlStore::new(self.pool().await);
        store.migrate().await.expect("control schema migrates");
        Arc::new(store)
    }
}

mod harness_db {
    use super::*;

    adk_kernel::control_store_conformance_tests! { harness = FileDatabase::new() }
}

async fn count(pool: &SqlitePool, table: &str) -> i64 {
    let (count,): (i64,) =
        sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
            .fetch_one(pool)
            .await
            .expect("count");
    count
}

/// The control store migrates on one pool beside the process store, each in its own
/// registry, migrating again changes nothing, and what it keeps survives a new pool, as it
/// survives `adk-rust up` restarting.
#[tokio::test]
async fn migrations_share_harness_db_and_settings_survive_a_restart() {
    let database = FileDatabase::new();
    let pool = database.pool().await;
    SqliteProcessStore::new(pool.clone()).migrate().await.expect("process store migrates");
    let store = SqliteControlStore::new(pool.clone());
    store.migrate().await.expect("migrate");
    store.migrate().await.expect("migrate again");
    assert_eq!(count(&pool, "_adk_kernel_control_migrations").await, 1);
    assert_eq!(count(&pool, "_adk_kernel_process_migrations").await, 1);

    let now = chrono::Utc::now();
    let settings = AreaSettings::new("default")
        .with_daily_spending_limit(Change::new(Some(2_500_000), "ana", now))
        .with_mode(Change::new(PartitionMode::Auto, "ana", now));
    store.put_area_settings(settings.clone()).await.expect("put");
    store.advance_cursor(AwayCursor::new("ana", "home", 12, now)).await.expect("advance");

    let reopened = SqliteControlStore::new(database.pool().await);
    reopened.migrate().await.expect("migrate");
    assert_eq!(reopened.area_settings(&"default".into()).await.expect("get"), Some(settings));
    assert_eq!(reopened.cursor("ana", "home").await.expect("get").map(|c| c.seq), Some(12));
}

/// A store used before `migrate` fails with a store error naming the backend.
#[tokio::test]
async fn an_unmigrated_store_fails_with_a_store_error() {
    let pool = SqlitePool::connect("sqlite::memory:").await.expect("SQLite opens");
    let error = SqliteControlStore::new(pool).cursor("ana", "home").await.expect_err("no table");
    assert!(matches!(error, KernelError::Store { backend: "sqlite", .. }), "{error}");
}
