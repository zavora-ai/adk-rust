#[cfg(feature = "database")]
mod tests {
    use adk_session::*;
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
            .list(ListRequest {
                app_name: "test_app".to_string(),
                user_id: "user1".to_string(),
            })
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
}
