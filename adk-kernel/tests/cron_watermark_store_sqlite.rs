//! The in-memory and SQLite cron watermark stores (spec 3.2 §7.4, §11, §13 Q8): the
//! conformance suite in memory, and on a file opened as `adk-rust` opens `harness.db`, and
//! again in WAL mode, plus its migrations beside the session and process stores' on one pool.
//!
//! Every SQLite `open` is a new connection pool over the same file, as a restarted kernel or
//! a second replica would create, so the pools contend for SQLite's write lock.

use std::sync::Arc;

use adk_kernel::conformance::CronWatermarkStoreHarness;
use adk_kernel::{CronWatermarkStore, InMemoryCronWatermarkStore};

struct InMemory;

#[async_trait::async_trait]
impl CronWatermarkStoreHarness for InMemory {
    async fn open(&self) -> Arc<dyn CronWatermarkStore> {
        Arc::new(InMemoryCronWatermarkStore::new())
    }

    fn is_durable(&self) -> bool {
        false
    }
}

mod memory {
    use super::*;

    adk_kernel::cron_watermark_store_conformance_tests! { harness = InMemory }
}

#[cfg(feature = "sqlite")]
mod sqlite {
    use std::str::FromStr;
    use std::time::Duration;

    use super::*;
    use adk_kernel::{KernelError, SqliteCronWatermarkStore, SqliteProcessStore};
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
                .join(format!("adk-kernel-watermark-{}.db", uuid::Uuid::new_v4()));
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
    impl CronWatermarkStoreHarness for FileDatabase {
        async fn open(&self) -> Arc<dyn CronWatermarkStore> {
            let store = SqliteCronWatermarkStore::new(self.pool().await);
            store.migrate().await.expect("watermark schema migrates");
            Arc::new(store)
        }
    }

    mod harness_db {
        use super::*;

        adk_kernel::cron_watermark_store_conformance_tests! {
            harness = FileDatabase::new(Journal::HarnessDb)
        }
    }

    mod wal {
        use super::*;

        adk_kernel::cron_watermark_store_conformance_tests! { harness = FileDatabase::new(Journal::Wal) }
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
    /// records its step in its own registry, and migrating again changes nothing.
    #[tokio::test]
    async fn migrations_share_harness_db() {
        let database = FileDatabase::new(Journal::HarnessDb);
        let pool = database.pool().await;
        SqliteProcessStore::new(pool.clone()).migrate().await.expect("process schema migrates");
        let sessions_before = count(&pool, "_adk_session_migrations").await;
        let store = SqliteCronWatermarkStore::new(pool.clone());
        store.migrate().await.expect("migrate");
        store.migrate().await.expect("migrate again");
        assert_eq!(count(&pool, "_adk_kernel_cron_watermark_migrations").await, 1);
        assert_eq!(count(&pool, "_adk_kernel_process_migrations").await, 2);
        assert_eq!(count(&pool, "_adk_session_migrations").await, sessions_before);
        store.advance("0_0_*_*_*_*", chrono::Utc::now()).await.expect("advance");
        assert_eq!(count(&pool, "adk_kernel_cron_watermarks").await, 1);
    }

    /// A database a newer release migrated is refused rather than written in an old layout.
    #[tokio::test]
    async fn a_newer_database_is_refused() {
        let database = FileDatabase::new(Journal::HarnessDb);
        let pool = database.pool().await;
        let store = SqliteCronWatermarkStore::new(pool.clone());
        store.migrate().await.expect("migrate");
        sqlx::query(
            "INSERT INTO _adk_kernel_cron_watermark_migrations (version, description, applied_at) \
             VALUES (99, 'from the future', '2030-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .expect("insert");
        let error = store.migrate().await.expect_err("a newer schema");
        assert!(matches!(error, KernelError::Store { backend: "sqlite", .. }), "{error}");
    }
}
