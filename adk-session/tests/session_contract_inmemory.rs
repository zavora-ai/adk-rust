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
