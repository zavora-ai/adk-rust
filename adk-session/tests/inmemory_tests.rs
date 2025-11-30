use adk_session::*;
use serde_json::json;
use std::collections::HashMap;

#[tokio::test]
async fn test_create_session() {
    let service = InMemorySessionService::new();

    let req = CreateRequest {
        app_name: "test_app".to_string(),
        user_id: "user1".to_string(),
        session_id: Some("session1".to_string()),
        state: HashMap::new(),
    };

    let session = service.create(req).await.unwrap();
    assert_eq!(session.id(), "session1");
    assert_eq!(session.app_name(), "test_app");
    assert_eq!(session.user_id(), "user1");
}

#[tokio::test]
async fn test_get_session() {
    let service = InMemorySessionService::new();

    service
        .create(CreateRequest {
            app_name: "test_app".to_string(),
            user_id: "user1".to_string(),
            session_id: Some("session1".to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();

    let session = service
        .get(GetRequest {
            app_name: "test_app".to_string(),
            user_id: "user1".to_string(),
            session_id: "session1".to_string(),
            num_recent_events: None,
            after: None,
        })
        .await
        .unwrap();

    assert_eq!(session.id(), "session1");
}

#[tokio::test]
async fn test_state_scoping() {
    let service = InMemorySessionService::new();

    let mut state = HashMap::new();
    state.insert("app:key1".to_string(), json!("app_value"));
    state.insert("user:key2".to_string(), json!("user_value"));
    state.insert("key3".to_string(), json!("session_value"));

    let session = service
        .create(CreateRequest {
            app_name: "test_app".to_string(),
            user_id: "user1".to_string(),
            session_id: Some("session1".to_string()),
            state,
        })
        .await
        .unwrap();

    assert_eq!(session.state().get("app:key1").unwrap(), json!("app_value"));
    assert_eq!(session.state().get("user:key2").unwrap(), json!("user_value"));
    assert_eq!(session.state().get("key3").unwrap(), json!("session_value"));
}

#[tokio::test]
async fn test_append_event() {
    let service = InMemorySessionService::new();

    service
        .create(CreateRequest {
            app_name: "test_app".to_string(),
            user_id: "user1".to_string(),
            session_id: Some("session1".to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();

    let event = Event::new("inv1");
    service.append_event("session1", event).await.unwrap();

    let session = service
        .get(GetRequest {
            app_name: "test_app".to_string(),
            user_id: "user1".to_string(),
            session_id: "session1".to_string(),
            num_recent_events: None,
            after: None,
        })
        .await
        .unwrap();

    assert_eq!(session.events().len(), 1);
}

#[tokio::test]
async fn test_list_sessions() {
    let service = InMemorySessionService::new();

    service
        .create(CreateRequest {
            app_name: "test_app".to_string(),
            user_id: "user1".to_string(),
            session_id: Some("session1".to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();

    service
        .create(CreateRequest {
            app_name: "test_app".to_string(),
            user_id: "user1".to_string(),
            session_id: Some("session2".to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();

    let sessions = service
        .list(ListRequest { app_name: "test_app".to_string(), user_id: "user1".to_string() })
        .await
        .unwrap();

    assert_eq!(sessions.len(), 2);
}

#[tokio::test]
async fn test_delete_session() {
    let service = InMemorySessionService::new();

    service
        .create(CreateRequest {
            app_name: "test_app".to_string(),
            user_id: "user1".to_string(),
            session_id: Some("session1".to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();

    service
        .delete(DeleteRequest {
            app_name: "test_app".to_string(),
            user_id: "user1".to_string(),
            session_id: "session1".to_string(),
        })
        .await
        .unwrap();

    let result = service
        .get(GetRequest {
            app_name: "test_app".to_string(),
            user_id: "user1".to_string(),
            session_id: "session1".to_string(),
            num_recent_events: None,
            after: None,
        })
        .await;

    assert!(result.is_err());
}
