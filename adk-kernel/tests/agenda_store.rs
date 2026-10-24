//! The in-memory and SQLite agenda stores pass the agenda store conformance suite (spec 3.2
//! §7.5, §11, §13 Q8). The SQLite suite runs on a file opened as `adk-rust` opens
//! `harness.db`, and again on a bare WAL pool; every `open` is a new pool over the same file, as
//! a restarted kernel or a second process would create.

use std::sync::Arc;

use adk_kernel::conformance::AgendaStoreHarness;
use adk_kernel::{AgendaStore, InMemoryAgendaStore};

struct InMemory;

#[async_trait::async_trait]
impl AgendaStoreHarness for InMemory {
    async fn open(&self) -> Arc<dyn AgendaStore> {
        Arc::new(InMemoryAgendaStore::new())
    }

    fn is_durable(&self) -> bool {
        false
    }
}

mod memory {
    use super::*;

    adk_kernel::agenda_store_conformance_tests! { harness = InMemory }
}

#[cfg(feature = "sqlite")]
mod sqlite {
    use std::str::FromStr;
    use std::sync::Arc;
    use std::time::Duration;

    use adk_core::principal::SubjectRef;
    use adk_kernel::conformance::AgendaStoreHarness;
    use adk_kernel::{
        AgendaItem, AgendaOrigin, AgendaStore, ProcessKind, ProcessMeta, ProcessStore, QueueClass,
        SqliteAgendaStore, SqliteProcessStore,
    };
    use adk_session::SqliteSessionService;
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool};

    #[derive(Clone, Copy)]
    enum Journal {
        HarnessDb,
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
                std::env::temp_dir().join(format!("adk-kernel-agenda-{}.db", uuid::Uuid::new_v4()));
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
    impl AgendaStoreHarness for FileDatabase {
        async fn open(&self) -> Arc<dyn AgendaStore> {
            let store = SqliteAgendaStore::new(self.pool().await);
            store.migrate().await.expect("agenda schema migrates");
            Arc::new(store)
        }
    }

    mod harness_db {
        use super::*;

        adk_kernel::agenda_store_conformance_tests! { harness = FileDatabase::new(Journal::HarnessDb) }
    }

    mod wal {
        use super::*;

        adk_kernel::agenda_store_conformance_tests! { harness = FileDatabase::new(Journal::Wal) }
    }

    async fn count(pool: &SqlitePool, table: &str) -> i64 {
        let (count,): (i64,) =
            sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
                .fetch_one(pool)
                .await
                .expect("count");
        count
    }

    /// The agenda migrates on the pool of `harness.db` beside the session and process
    /// stores, records its steps in its own registry, and migrating again changes nothing.
    #[tokio::test]
    async fn migrations_share_harness_db() {
        let database = FileDatabase::new(Journal::HarnessDb);
        let pool = database.pool().await;
        let processes = SqliteProcessStore::new(pool.clone());
        processes.migrate().await.expect("process schema");
        let agenda = SqliteAgendaStore::new(pool.clone());
        agenda.migrate().await.expect("migrate");
        agenda.migrate().await.expect("migrate again");
        assert_eq!(count(&pool, "_adk_kernel_agenda_migrations").await, 1);
        assert_eq!(count(&pool, "_adk_kernel_process_migrations").await, 1);
        agenda
            .put(AgendaItem::new(
                "item-1",
                SubjectRef::human("alice"),
                "t",
                "r",
                AgendaOrigin::User,
                chrono::Utc::now(),
            ))
            .await
            .expect("put");
        processes
            .put(ProcessMeta::new("run-1", QueueClass::Background, ProcessKind::Proposed))
            .await
            .expect("put");
        assert_eq!(count(&pool, "adk_kernel_agenda").await, 1);
        assert_eq!(count(&pool, "adk_kernel_processes").await, 1);
    }
}
