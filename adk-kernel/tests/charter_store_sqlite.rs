//! The in-memory and SQLite charter, KPI, autonomy and signal stores (spec 3.5 §7.10,
//! §7.11, §11): the four conformance suites in memory, on a file opened as `adk-rust` opens
//! `harness.db`, and again in WAL mode, plus their migrations beside the kernel's other
//! stores on one pool.
//!
//! Every SQLite `open` is a new connection pool over the same file, as a restarted kernel or
//! a second replica would create, so the pools contend for SQLite's write lock.

#![cfg(feature = "charter")]

use std::sync::Arc;

use adk_kernel::charter::{
    AutonomyStore, CharterStore, InMemoryAutonomyStore, InMemoryCharterStore, InMemoryKpiStore,
    InMemorySignalStore, KpiStore, SignalStore,
};
use adk_kernel::conformance::charter::{
    AutonomyStoreHarness, CharterStoreHarness, KpiStoreHarness, SignalStoreHarness,
};

struct InMemory;

#[async_trait::async_trait]
impl CharterStoreHarness for InMemory {
    async fn open(&self) -> Arc<dyn CharterStore> {
        Arc::new(InMemoryCharterStore::new())
    }

    fn is_durable(&self) -> bool {
        false
    }
}

#[async_trait::async_trait]
impl KpiStoreHarness for InMemory {
    async fn open(&self) -> Arc<dyn KpiStore> {
        Arc::new(InMemoryKpiStore::new())
    }

    fn is_durable(&self) -> bool {
        false
    }
}

#[async_trait::async_trait]
impl AutonomyStoreHarness for InMemory {
    async fn open(&self) -> Arc<dyn AutonomyStore> {
        Arc::new(InMemoryAutonomyStore::new())
    }

    fn is_durable(&self) -> bool {
        false
    }
}

#[async_trait::async_trait]
impl SignalStoreHarness for InMemory {
    async fn open(&self) -> Arc<dyn SignalStore> {
        Arc::new(InMemorySignalStore::new())
    }

    fn is_durable(&self) -> bool {
        false
    }
}

mod memory {
    use super::*;

    adk_kernel::charter_store_conformance_tests! { harness = InMemory }
    adk_kernel::kpi_store_conformance_tests! { harness = InMemory }
    adk_kernel::autonomy_store_conformance_tests! { harness = InMemory }
    adk_kernel::signal_store_conformance_tests! { harness = InMemory }
}

#[cfg(feature = "sqlite")]
mod sqlite {
    use std::str::FromStr;
    use std::time::Duration;

    use super::*;
    use adk_kernel::charter::{
        SqliteAutonomyStore, SqliteCharterStore, SqliteKpiStore, SqliteSignalStore,
    };
    use adk_kernel::{KernelError, SqliteProcessStore};
    use adk_session::SqliteSessionService;
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool};

    /// How the database file is opened.
    #[derive(Clone, Copy)]
    enum Journal {
        /// As `adk-rust` opens `harness.db`.
        HarnessDb,
        /// Write-ahead logging, with the same busy timeout.
        Wal,
    }

    /// A database file, removed when the test ends.
    struct FileDatabase {
        path: std::path::PathBuf,
        journal: Journal,
    }

    impl FileDatabase {
        fn new(journal: Journal) -> Self {
            let path = std::env::temp_dir()
                .join(format!("adk-kernel-charter-{}.db", uuid::Uuid::new_v4()));
            Self { path, journal }
        }

        fn url(&self) -> String {
            format!("sqlite://{}?mode=rwc", self.path.display())
        }

        async fn pool(&self) -> SqlitePool {
            match self.journal {
                Journal::HarnessDb => {
                    let sessions =
                        SqliteSessionService::new(&self.url()).await.expect("SQLite opens");
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
    impl CharterStoreHarness for FileDatabase {
        async fn open(&self) -> Arc<dyn CharterStore> {
            let store = SqliteCharterStore::new(self.pool().await);
            store.migrate().await.expect("charter schema migrates");
            Arc::new(store)
        }
    }

    #[async_trait::async_trait]
    impl KpiStoreHarness for FileDatabase {
        async fn open(&self) -> Arc<dyn KpiStore> {
            let store = SqliteKpiStore::new(self.pool().await);
            store.migrate().await.expect("charter schema migrates");
            Arc::new(store)
        }
    }

    #[async_trait::async_trait]
    impl AutonomyStoreHarness for FileDatabase {
        async fn open(&self) -> Arc<dyn AutonomyStore> {
            let store = SqliteAutonomyStore::new(self.pool().await);
            store.migrate().await.expect("charter schema migrates");
            Arc::new(store)
        }
    }

    #[async_trait::async_trait]
    impl SignalStoreHarness for FileDatabase {
        async fn open(&self) -> Arc<dyn SignalStore> {
            let store = SqliteSignalStore::new(self.pool().await);
            store.migrate().await.expect("charter schema migrates");
            Arc::new(store)
        }
    }

    mod harness_db {
        use super::*;

        adk_kernel::charter_store_conformance_tests! { harness = FileDatabase::new(Journal::HarnessDb) }
        adk_kernel::kpi_store_conformance_tests! { harness = FileDatabase::new(Journal::HarnessDb) }
        adk_kernel::autonomy_store_conformance_tests! { harness = FileDatabase::new(Journal::HarnessDb) }
        adk_kernel::signal_store_conformance_tests! { harness = FileDatabase::new(Journal::HarnessDb) }
    }

    mod wal {
        use super::*;

        adk_kernel::charter_store_conformance_tests! { harness = FileDatabase::new(Journal::Wal) }
        adk_kernel::kpi_store_conformance_tests! { harness = FileDatabase::new(Journal::Wal) }
        adk_kernel::autonomy_store_conformance_tests! { harness = FileDatabase::new(Journal::Wal) }
        adk_kernel::signal_store_conformance_tests! { harness = FileDatabase::new(Journal::Wal) }
    }

    async fn count(pool: &SqlitePool, table: &str) -> i64 {
        let (count,): (i64,) =
            sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
                .fetch_one(pool)
                .await
                .expect("count");
        count
    }

    /// The four stores migrate one registry on the pool of `harness.db` beside the session
    /// and process stores, and migrating again changes nothing.
    #[tokio::test]
    async fn migrations_share_harness_db() {
        let database = FileDatabase::new(Journal::HarnessDb);
        let pool = database.pool().await;
        SqliteProcessStore::new(pool.clone()).migrate().await.expect("process schema migrates");
        let sessions_before = count(&pool, "_adk_session_migrations").await;
        SqliteCharterStore::new(pool.clone()).migrate().await.expect("migrate");
        SqliteKpiStore::new(pool.clone()).migrate().await.expect("migrate");
        SqliteAutonomyStore::new(pool.clone()).migrate().await.expect("migrate");
        SqliteSignalStore::new(pool.clone()).migrate().await.expect("migrate again");
        assert_eq!(count(&pool, "_adk_kernel_charter_migrations").await, 1);
        assert_eq!(count(&pool, "_adk_kernel_process_migrations").await, 2);
        assert_eq!(count(&pool, "_adk_session_migrations").await, sessions_before);
        assert_eq!(count(&pool, "adk_kernel_charters").await, 0);
    }

    /// A database a newer release migrated is refused rather than written in an old layout.
    #[tokio::test]
    async fn a_newer_database_is_refused() {
        let database = FileDatabase::new(Journal::HarnessDb);
        let pool = database.pool().await;
        let store = SqliteCharterStore::new(pool.clone());
        store.migrate().await.expect("migrate");
        sqlx::query(
            "INSERT INTO _adk_kernel_charter_migrations (version, description, applied_at) \
             VALUES (99, 'from the future', '2030-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .expect("insert");
        let error = store.migrate().await.expect_err("a newer schema");
        assert!(matches!(error, KernelError::Store { backend: "sqlite", .. }), "{error}");
    }
}
