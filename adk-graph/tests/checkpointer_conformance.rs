//! The in-process checkpointers pass the shared conformance suite.

use std::sync::Arc;

use adk_graph::checkpoint::{Checkpointer, MemoryCheckpointer};
use adk_graph::conformance::CheckpointerHarness;

#[derive(Default)]
struct InMemory(Arc<MemoryCheckpointer>);

#[async_trait::async_trait]
impl CheckpointerHarness for InMemory {
    async fn open(&self) -> Arc<dyn Checkpointer> {
        self.0.clone()
    }
}

mod memory {
    adk_graph::checkpointer_conformance_tests!(harness = super::InMemory::default());
}

/// Each `open` is a new pool over one database file, as a restarted process would open.
#[cfg(feature = "sqlite")]
struct Sqlite(std::path::PathBuf);

#[cfg(feature = "sqlite")]
impl Sqlite {
    fn new() -> Self {
        Self(
            std::env::temp_dir().join(format!("adk-graph-conformance-{}.db", uuid::Uuid::new_v4())),
        )
    }
}

#[cfg(feature = "sqlite")]
impl Drop for Sqlite {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
        }
    }
}

#[cfg(feature = "sqlite")]
#[async_trait::async_trait]
impl CheckpointerHarness for Sqlite {
    async fn open(&self) -> Arc<dyn Checkpointer> {
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&self.0)
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .busy_timeout(std::time::Duration::from_secs(30));
        let pool = sqlx::SqlitePool::connect_with(options).await.expect("open sqlite");
        Arc::new(
            adk_graph::checkpoint::SqliteCheckpointer::from_pool(pool)
                .await
                .expect("apply checkpoint schema"),
        )
    }
}

#[cfg(feature = "sqlite")]
mod sqlite {
    adk_graph::checkpointer_conformance_tests!(harness = super::Sqlite::new());
}
