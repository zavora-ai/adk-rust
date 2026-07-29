#![cfg(feature = "sqlite")]

mod common;

use adk_session::SqliteSessionService;

#[tokio::test]
async fn test_sqlite_service_contract() {
    let service = SqliteSessionService::new(":memory:").await.expect("SQLite service starts");
    service.migrate().await.expect("SQLite schema migrates");

    common::session_contract::assert_session_contract(&service, "contract_app", "contract_app_2")
        .await;
}
