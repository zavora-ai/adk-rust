mod common;

use adk_session::InMemorySessionService;

#[tokio::test]
async fn test_inmemory_service_contract() {
    let service = InMemorySessionService::new();
    common::session_contract::assert_session_contract(&service, "contract_app", "contract_app_2")
        .await;
}

#[tokio::test]
async fn test_inmemory_shared_state_contract() {
    let service = InMemorySessionService::new();
    common::state_contract::assert_shared_state_contract(&service, "state_app", "user1", "user2")
        .await;
}

#[tokio::test]
async fn test_inmemory_concurrent_state_writes() {
    let service = InMemorySessionService::new();
    common::state_contract::assert_concurrent_state_writes(&service, "state_app", "user1", 8).await;
}

#[tokio::test]
async fn test_inmemory_event_filter_contract() {
    let service = InMemorySessionService::new();
    common::state_contract::assert_event_filter_contract(&service, "filter_app", "user1").await;
}

#[tokio::test]
async fn test_inmemory_create_and_append_errors() {
    let service = InMemorySessionService::new();
    common::integrity_contract::assert_create_and_append_errors_contract(&service, "errors_app")
        .await;
}

#[tokio::test]
async fn test_inmemory_event_order() {
    let service = InMemorySessionService::new();
    common::integrity_contract::assert_event_order_contract(&service, "order_app").await;
}

#[tokio::test]
async fn test_inmemory_nul_round_trip() {
    let service = InMemorySessionService::new();
    common::integrity_contract::assert_nul_round_trip_contract(&service, "nul_app").await;
}

#[tokio::test]
async fn test_inmemory_list_order() {
    let service = InMemorySessionService::new();
    common::integrity_contract::assert_list_order_contract(&service, "list_app").await;
}

#[tokio::test]
async fn test_inmemory_rewind_contract() {
    let service = InMemorySessionService::new();
    common::integrity_contract::assert_rewind_contract(&service, "rewind_app").await;
}

#[tokio::test]
async fn test_inmemory_delete_all_sessions_erases_the_user_tier() {
    let service = InMemorySessionService::new();
    common::state_contract::assert_delete_all_sessions_contract(
        &service,
        "erase_app",
        "erase_app_2",
        true,
    )
    .await;
}
