#[cfg(feature = "database")]
mod tests {
    use adk_session::*;
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
    async fn test_database_append_event_persists_event() {
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
    async fn test_database_append_event_applies_state_delta() {
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

        let mut event = Event::new("inv1");
        event.actions.state_delta.insert("session_key".to_string(), json!("session_value"));
        event.actions.state_delta.insert("app:app_key".to_string(), json!("app_value"));
        event.actions.state_delta.insert("user:user_key".to_string(), json!("user_value"));
        event.actions.state_delta.insert("temp:temp_key".to_string(), json!("temp_value"));

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

        assert_eq!(session.state().get("session_key"), Some(json!("session_value")));
        assert_eq!(session.state().get("app:app_key"), Some(json!("app_value")));
        assert_eq!(session.state().get("user:user_key"), Some(json!("user_value")));
        assert_eq!(session.state().get("temp:temp_key"), None);
    }

    #[tokio::test]
    async fn test_database_append_event_rejects_ambiguous_session_id() {
        let service = DatabaseSessionService::new(":memory:").await.unwrap();
        service.migrate().await.unwrap();

        service
            .create(CreateRequest {
                app_name: "app_one".to_string(),
                user_id: "user_one".to_string(),
                session_id: Some("shared_session".to_string()),
                state: HashMap::new(),
            })
            .await
            .unwrap();

        service
            .create(CreateRequest {
                app_name: "app_two".to_string(),
                user_id: "user_two".to_string(),
                session_id: Some("shared_session".to_string()),
                state: HashMap::new(),
            })
            .await
            .unwrap();

        let event = Event::new("inv1");
        let err = service.append_event("shared_session", event).await.unwrap_err();
        assert!(
            err.to_string().contains("ambiguous"),
            "expected ambiguous session_id error, got: {}",
            err
        );
    }
}
