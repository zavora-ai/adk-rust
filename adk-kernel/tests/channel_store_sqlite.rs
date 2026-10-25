//! The in-memory and SQLite channel binding stores (spec 3.3 §10 K1): the conformance suite
//! on memory and on a file opened as `adk-rust` opens `harness.db`, and the store's
//! migrations beside the device store's on one pool.

#![cfg(all(feature = "channels", feature = "sqlite"))]

use std::sync::Arc;

use adk_kernel::KernelError;
use adk_kernel::channels::{
    ChannelBindingStore, ChannelPairingRequest, InMemoryChannelBindingStore,
    SqliteChannelBindingStore, accept_pairing,
};
use adk_kernel::conformance::ChannelBindingStoreHarness;
use adk_kernel::pairing::SqliteDeviceStore;
use adk_session::SqliteSessionService;
use sqlx::sqlite::SqlitePool;

struct InMemory;

#[async_trait::async_trait]
impl ChannelBindingStoreHarness for InMemory {
    async fn open(&self) -> Arc<dyn ChannelBindingStore> {
        Arc::new(InMemoryChannelBindingStore::new())
    }

    fn is_durable(&self) -> bool {
        false
    }
}

mod in_memory {
    use super::*;

    adk_kernel::channel_binding_store_conformance_tests! { harness = InMemory }
}

/// A database file, removed when the test ends.
struct FileDatabase {
    path: std::path::PathBuf,
}

impl FileDatabase {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("adk-kernel-channel-{}.db", uuid::Uuid::new_v4()));
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
impl ChannelBindingStoreHarness for FileDatabase {
    async fn open(&self) -> Arc<dyn ChannelBindingStore> {
        let store = SqliteChannelBindingStore::new(self.pool().await);
        store.migrate().await.expect("channel schema migrates");
        Arc::new(store)
    }
}

mod harness_db {
    use super::*;

    adk_kernel::channel_binding_store_conformance_tests! { harness = FileDatabase::new() }
}

async fn count(pool: &SqlitePool, table: &str) -> i64 {
    let (count,): (i64,) =
        sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
            .fetch_one(pool)
            .await
            .expect("count");
    count
}

/// The channel store migrates on one pool beside the device store, each in its own
/// registry, and an account paired through it survives a new pool.
#[tokio::test]
async fn migrations_share_harness_db_and_pairings_survive_a_restart() {
    let database = FileDatabase::new();
    let pool = database.pool().await;
    SqliteDeviceStore::new(pool.clone()).migrate().await.expect("device store migrates");
    let store = SqliteChannelBindingStore::new(pool.clone());
    store.migrate().await.expect("migrate");
    store.migrate().await.expect("migrate again");
    assert_eq!(count(&pool, "_adk_kernel_channel_migrations").await, 1);
    assert_eq!(count(&pool, "_adk_kernel_device_migrations").await, 3);

    let now = chrono::Utc::now();
    let request = ChannelPairingRequest::new(
        adk_kernel_code_hash("ABCD-EFGH-2345"),
        "telegram",
        "4217",
        "4217",
        now,
        now + chrono::Duration::hours(1),
    );
    store.put_request(request).await.expect("put");
    let binding = accept_pairing(&store, "abcd efgh 2345", "ana", &["operator".into()], now)
        .await
        .expect("the code pairs the account");

    let reopened = SqliteChannelBindingStore::new(database.pool().await);
    reopened.migrate().await.expect("migrate");
    assert_eq!(reopened.get_binding(&binding.id).await.expect("get"), Some(binding));
}

/// The hash a store keeps of `code`: SHA-256 of the code upper-cased without separators.
fn adk_kernel_code_hash(code: &str) -> String {
    use sha2::Digest as _;
    let normalized: String =
        code.chars().filter(|c| *c != '-').map(|c| c.to_ascii_uppercase()).collect();
    sha2::Sha256::digest(normalized.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A store used before `migrate` fails with a store error naming the backend.
#[tokio::test]
async fn an_unmigrated_store_fails_with_a_store_error() {
    let pool = SqlitePool::connect("sqlite::memory:").await.expect("SQLite opens");
    let error = SqliteChannelBindingStore::new(pool).get_binding("x").await.expect_err("no table");
    assert!(matches!(error, KernelError::Store { backend: "sqlite", .. }), "{error}");
}
