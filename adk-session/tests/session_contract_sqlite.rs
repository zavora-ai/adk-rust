#![cfg(feature = "sqlite")]

mod common;

use adk_session::SqliteSessionService;

async fn in_memory() -> SqliteSessionService {
    let service = SqliteSessionService::new(":memory:").await.expect("SQLite service starts");
    service.migrate().await.expect("SQLite schema migrates");
    service
}

/// A database file, so pooled connections contend for the write lock as separate
/// processes would.
struct FileDatabase {
    path: std::path::PathBuf,
}

impl FileDatabase {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("adk-session-{}.db", uuid::Uuid::new_v4()));
        Self { path }
    }

    async fn service(&self) -> SqliteSessionService {
        let url = format!("sqlite://{}?mode=rwc", self.path.display());
        let service = SqliteSessionService::new(&url).await.expect("SQLite service starts");
        service.migrate().await.expect("SQLite schema migrates");
        service
    }
}

impl Drop for FileDatabase {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm", "-journal"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.path.display()));
        }
    }
}

#[tokio::test]
async fn test_sqlite_service_contract() {
    let service = in_memory().await;
    common::session_contract::assert_session_contract(&service, "contract_app", "contract_app_2")
        .await;
}

#[tokio::test]
async fn test_sqlite_shared_state_contract() {
    let service = in_memory().await;
    common::state_contract::assert_shared_state_contract(&service, "state_app", "user1", "user2")
        .await;
}

#[tokio::test]
async fn test_sqlite_concurrent_state_writes_in_memory() {
    let service = in_memory().await;
    common::state_contract::assert_concurrent_state_writes(&service, "state_app", "user1", 8).await;
}

#[tokio::test]
async fn test_sqlite_concurrent_state_writes_file() {
    let database = FileDatabase::new();
    let service = database.service().await;
    common::state_contract::assert_concurrent_state_writes(&service, "state_app", "user1", 8).await;
}

#[tokio::test]
async fn test_sqlite_event_filter_contract() {
    let service = in_memory().await;
    common::state_contract::assert_event_filter_contract(&service, "filter_app", "user1").await;
}
