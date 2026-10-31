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

#[tokio::test]
async fn test_sqlite_create_and_append_errors() {
    let service = in_memory().await;
    common::integrity_contract::assert_create_and_append_errors_contract(&service, "errors_app")
        .await;
}

#[tokio::test]
async fn test_sqlite_event_order() {
    let service = in_memory().await;
    common::integrity_contract::assert_event_order_contract(&service, "order_app").await;
}

#[tokio::test]
async fn test_sqlite_nul_round_trip() {
    let service = in_memory().await;
    common::integrity_contract::assert_nul_round_trip_contract(&service, "nul_app").await;
}

#[tokio::test]
async fn test_sqlite_list_order() {
    let service = in_memory().await;
    common::integrity_contract::assert_list_order_contract(&service, "list_app").await;
}

#[tokio::test]
async fn test_sqlite_rewind_contract() {
    let service = in_memory().await;
    common::integrity_contract::assert_rewind_contract(&service, "rewind_app").await;
}

#[tokio::test]
async fn test_sqlite_delete_all_sessions_erases_the_user_tier() {
    let service = in_memory().await;
    common::state_contract::assert_delete_all_sessions_contract(
        &service,
        "erase_app",
        "erase_app_2",
        true,
    )
    .await;
}

#[adk_core::async_trait]
impl common::subject_data::OpenSessions for FileDatabase {
    async fn service(&self) -> std::sync::Arc<dyn adk_session::SessionService> {
        std::sync::Arc::new(FileDatabase::service(self).await)
    }
}

mod subject_data {
    adk_core::subject_data_store_conformance_tests!(
        harness = crate::common::subject_data::SessionData(super::FileDatabase::new())
    );
}
