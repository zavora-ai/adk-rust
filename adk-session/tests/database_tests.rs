#[cfg(feature = "database")]
mod tests {
    use adk_session::*;
    use chrono::{Duration, Utc};
    use serde_json::json;
    use std::collections::HashMap;

    #[tokio::test]
    async fn test_database_create_session() {
        let service = DatabaseSessionService::new(":memory:").await.unwrap();
        service.migrate().await.unwrap();

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
    async fn test_database_get_session() {
        let service = DatabaseSessionService::new(":memory:").await.unwrap();
        service.migrate().await.unwrap();

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
    async fn test_database_list_sessions() {
        let service = DatabaseSessionService::new(":memory:").await.unwrap();
        service.migrate().await.unwrap();

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
    async fn test_database_delete_session() {
        let service = DatabaseSessionService::new(":memory:").await.unwrap();
        service.migrate().await.unwrap();

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

    #[tokio::test]
    async fn test_database_append_event_persists_state_and_identity() {
        let service = DatabaseSessionService::new(":memory:").await.unwrap();
        service.migrate().await.unwrap();

        service
            .create(CreateRequest {
                app_name: "test_app".to_string(),
                user_id: "user1".to_string(),
                session_id: Some("session1".to_string()),
                state: HashMap::new(),
            })
            .await
            .unwrap();

        let before = service
            .get(GetRequest {
                app_name: "test_app".to_string(),
                user_id: "user1".to_string(),
                session_id: "session1".to_string(),
                num_recent_events: None,
                after: None,
            })
            .await
            .unwrap();
        let before_updated_at = before.last_update_time();

        let mut event = Event::new("inv-1");
        event.author = "agent".to_string();
        event.timestamp = Utc::now() + Duration::seconds(1);
        event.actions.state_delta.insert("result".to_string(), json!("ok"));
        event.actions.state_delta.insert(format!("{}locale", KEY_PREFIX_APP), json!("en-US"));
        event.actions.state_delta.insert(format!("{}name", KEY_PREFIX_USER), json!("Alice"));
        event
            .actions
            .state_delta
            .insert(format!("{}scratch", KEY_PREFIX_TEMP), json!("do-not-persist"));

        service.append_event("session1", event).await.unwrap();

        let after = service
            .get(GetRequest {
                app_name: "test_app".to_string(),
                user_id: "user1".to_string(),
                session_id: "session1".to_string(),
                num_recent_events: None,
                after: None,
            })
            .await
            .unwrap();

        assert_eq!(after.events().len(), 1);
        assert_eq!(after.state().get("result"), Some(json!("ok")));
        assert_eq!(after.state().get("app:locale"), Some(json!("en-US")));
        assert_eq!(after.state().get("user:name"), Some(json!("Alice")));
        assert_eq!(after.state().get("temp:scratch"), None);
        assert!(after.last_update_time() > before_updated_at);
    }

    #[tokio::test]
    async fn test_database_delete_cleans_up_events_for_recreated_session() {
        let service = DatabaseSessionService::new(":memory:").await.unwrap();
        service.migrate().await.unwrap();

        service
            .create(CreateRequest {
                app_name: "test_app".to_string(),
                user_id: "user1".to_string(),
                session_id: Some("session1".to_string()),
                state: HashMap::new(),
            })
            .await
            .unwrap();

        let mut event = Event::new("inv-1");
        event.author = "agent".to_string();
        event.actions.state_delta.insert("result".to_string(), json!("ok"));
        service.append_event("session1", event).await.unwrap();

        let with_event = service
            .get(GetRequest {
                app_name: "test_app".to_string(),
                user_id: "user1".to_string(),
                session_id: "session1".to_string(),
                num_recent_events: None,
                after: None,
            })
            .await
            .unwrap();
        assert_eq!(with_event.events().len(), 1);

        service
            .delete(DeleteRequest {
                app_name: "test_app".to_string(),
                user_id: "user1".to_string(),
                session_id: "session1".to_string(),
            })
            .await
            .unwrap();

        service
            .create(CreateRequest {
                app_name: "test_app".to_string(),
                user_id: "user1".to_string(),
                session_id: Some("session1".to_string()),
                state: HashMap::new(),
            })
            .await
            .unwrap();

        let recreated = service
            .get(GetRequest {
                app_name: "test_app".to_string(),
                user_id: "user1".to_string(),
                session_id: "session1".to_string(),
                num_recent_events: None,
                after: None,
            })
            .await
            .unwrap();
        assert_eq!(recreated.events().len(), 0);
    }
}
