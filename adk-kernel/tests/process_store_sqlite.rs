//! The SQLite process store (spec 3.2 §7.1, §11, §13 Q8): the conformance suite on a file
//! opened as `adk-rust` opens `harness.db`, and again on a bare WAL pool, plus its migrations
//! beside the session store's on one pool.
//!
//! Every `open` is a new connection pool over the same file, as a restarted kernel or a
//! second process would create, so the pools contend for SQLite's write lock.

#![cfg(feature = "sqlite")]

use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use adk_kernel::conformance::ProcessStoreHarness;
use adk_kernel::{
    KernelError, ProcessKind, ProcessMeta, ProcessStore, QueueClass, SqliteProcessStore,
};
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
            std::env::temp_dir().join(format!("adk-kernel-process-{}.db", uuid::Uuid::new_v4()));
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
impl ProcessStoreHarness for FileDatabase {
    async fn open(&self) -> Arc<dyn ProcessStore> {
        let store = SqliteProcessStore::new(self.pool().await);
        store.migrate().await.expect("process schema migrates");
        Arc::new(store)
    }
}

mod harness_db {
    use super::*;

    adk_kernel::process_store_conformance_tests! { harness = FileDatabase::new(Journal::HarnessDb) }
}

mod wal {
    use super::*;

    adk_kernel::process_store_conformance_tests! { harness = FileDatabase::new(Journal::Wal) }
}

async fn count(pool: &SqlitePool, table: &str) -> i64 {
    let (count,): (i64,) =
        sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
            .fetch_one(pool)
            .await
            .expect("count");
    count
}

/// The store migrates on the pool of `harness.db` beside the session store, records its
/// steps in its own registry, and migrating again changes nothing.
#[tokio::test]
async fn migrations_share_harness_db() {
    let database = FileDatabase::new(Journal::HarnessDb);
    let pool = database.pool().await;
    let sessions_before = count(&pool, "_adk_session_migrations").await;
    let store = SqliteProcessStore::new(pool.clone());
    store.migrate().await.expect("migrate");
    store.migrate().await.expect("migrate again");
    assert_eq!(count(&pool, "_adk_kernel_process_migrations").await, 1);
    assert_eq!(count(&pool, "_adk_session_migrations").await, sessions_before);
    store
        .put(ProcessMeta::new("run-1", QueueClass::Background, ProcessKind::Goal))
        .await
        .expect("put");
    assert_eq!(count(&pool, "adk_kernel_processes").await, 1);
    assert_eq!(count(&pool, "sessions").await, 0, "the session tables are untouched");
}

/// A database a newer release migrated is refused rather than written in an old layout.
#[tokio::test]
async fn a_newer_database_is_refused() {
    let database = FileDatabase::new(Journal::HarnessDb);
    let pool = database.pool().await;
    let store = SqliteProcessStore::new(pool.clone());
    store.migrate().await.expect("migrate");
    sqlx::query(
        "INSERT INTO _adk_kernel_process_migrations (version, description, applied_at) \
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
    assert_eq!(error.code(), adk_kernel::error::STORE_CODE);
}

/// A store used before `migrate` fails with a store error naming the backend, not a panic.
#[tokio::test]
async fn an_unmigrated_store_fails_with_a_store_error() {
    let pool = SqlitePool::connect("sqlite::memory:").await.expect("SQLite opens");
    let error = SqliteProcessStore::new(pool).get("run-1").await.expect_err("no table");
    assert!(matches!(error, KernelError::Store { backend: "sqlite", .. }), "{error}");
}
