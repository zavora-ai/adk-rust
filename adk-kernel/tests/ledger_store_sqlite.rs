//! The SQLite ledger store (spec 3.2 §7.1, §7.10, §11, §13 Q8): the conformance suite on a
//! file opened as `adk-rust` opens `harness.db`, and again on a bare WAL pool, plus its migrations
//! beside the session and process stores on one pool.
//!
//! Every `open` is a new connection pool over the same file, as a restarted kernel or a
//! second process would create, so the pools contend for SQLite's write lock.

#![cfg(all(feature = "proc", feature = "sqlite"))]

use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use adk_kernel::conformance::LedgerStoreHarness;
use adk_kernel::proc::{
    Activity, Ledger, LedgerKey, LedgerKind, LedgerQuery, LedgerSink, LedgerStore,
    SqliteLedgerStore,
};
use adk_kernel::{KernelError, SqliteProcessStore};
use adk_session::SqliteSessionService;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool};

/// How the database file is opened.
#[derive(Clone, Copy)]
enum Journal {
    /// As `adk-rust` opens `harness.db`: `SqliteSessionService::new`, with its 30-second
    /// busy timeout, switched to write-ahead logging.
    HarnessDb,
    /// A bare pool in write-ahead-logging mode, with the same busy timeout.
    Wal,
}

/// A database file, removed when the test ends.
struct FileDatabase {
    path: std::path::PathBuf,
    journal: Journal,
}

impl FileDatabase {
    fn new(journal: Journal) -> Self {
        let path =
            std::env::temp_dir().join(format!("adk-kernel-ledger-{}.db", uuid::Uuid::new_v4()));
        Self { path, journal }
    }

    fn url(&self) -> String {
        format!("sqlite://{}?mode=rwc", self.path.display())
    }

    /// A new pool over the file, with the session store migrated beside the kernel's.
    async fn pool(&self) -> SqlitePool {
        match self.journal {
            Journal::HarnessDb => {
                let sessions = SqliteSessionService::new(&self.url()).await.expect("SQLite opens");
                sqlx::query("PRAGMA journal_mode = WAL")
                    .execute(sessions.pool())
                    .await
                    .expect("WAL");
                sessions.migrate().await.expect("session schema migrates");
                sessions.pool().clone()
            }
            Journal::Wal => {
                let options = SqliteConnectOptions::from_str(&self.url())
                    .expect("valid URL")
                    .journal_mode(SqliteJournalMode::Wal)
                    .busy_timeout(Duration::from_secs(30));
                SqlitePool::connect_with(options).await.expect("SQLite opens")
            }
        }
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
impl LedgerStoreHarness for FileDatabase {
    async fn open(&self) -> Arc<dyn LedgerStore> {
        let store = SqliteLedgerStore::new(self.pool().await);
        store.migrate().await.expect("ledger schema migrates");
        Arc::new(store)
    }
}

mod harness_db {
    use super::*;

    adk_kernel::ledger_store_conformance_tests! { harness = FileDatabase::new(Journal::HarnessDb) }
}

mod wal {
    use super::*;

    adk_kernel::ledger_store_conformance_tests! { harness = FileDatabase::new(Journal::Wal) }
}

async fn count(pool: &SqlitePool, table: &str) -> i64 {
    let (count,): (i64,) =
        sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
            .fetch_one(pool)
            .await
            .expect("count");
    count
}

/// The store migrates on the pool of `harness.db` beside the session and process stores,
/// records its steps in its own registry, and migrating again changes nothing.
#[tokio::test]
async fn migrations_share_harness_db() {
    let database = FileDatabase::new(Journal::HarnessDb);
    let pool = database.pool().await;
    SqliteProcessStore::new(pool.clone()).migrate().await.expect("process schema");
    let store = Arc::new(SqliteLedgerStore::new(pool.clone()));
    store.migrate().await.expect("migrate");
    store.migrate().await.expect("migrate again");
    assert_eq!(count(&pool, "_adk_kernel_ledger_migrations").await, 1);
    assert_eq!(count(&pool, "_adk_kernel_process_migrations").await, 1);
    let ledger = Ledger::new(store.clone(), LedgerKey::from_bytes([1; 32]));
    ledger.record(Activity::new(LedgerKind::Intent, "harness")).await.expect("record");
    assert_eq!(count(&pool, "adk_kernel_ledger").await, 1);
    assert_eq!(count(&pool, "adk_kernel_processes").await, 0);
    assert_eq!(store.list(&LedgerQuery::default()).await.unwrap()[0].kind, "intent");
}

/// A database a newer release migrated is refused rather than written in an old layout.
#[tokio::test]
async fn a_newer_database_is_refused() {
    let database = FileDatabase::new(Journal::HarnessDb);
    let pool = database.pool().await;
    let store = SqliteLedgerStore::new(pool.clone());
    store.migrate().await.expect("migrate");
    sqlx::query(
        "INSERT INTO _adk_kernel_ledger_migrations (version, description, applied_at) \
         VALUES (99, 'from the future', '2030-01-01T00:00:00Z')",
    )
    .execute(&pool)
    .await
    .expect("insert");
    let error = store.migrate().await.expect_err("a newer schema");
    assert!(
        matches!(&error, KernelError::Store { backend: "sqlite", reason } if reason.contains("v99")),
        "{error}"
    );
}

/// A store used before `migrate` fails with a store error naming the backend.
#[tokio::test]
async fn an_unmigrated_store_fails_with_a_store_error() {
    let pool = SqlitePool::connect("sqlite::memory:").await.expect("SQLite opens");
    let error =
        SqliteLedgerStore::new(pool).list(&LedgerQuery::default()).await.expect_err("no table");
    assert!(matches!(error, KernelError::Store { backend: "sqlite", .. }), "{error}");
}
