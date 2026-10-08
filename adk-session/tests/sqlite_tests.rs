#[cfg(feature = "sqlite")]
mod tests {
    use adk_session::*;
    use chrono::{Duration, Utc};
    use serde_json::json;
    use std::collections::HashMap;

    #[tokio::test]
    async fn test_sqlite_create_session() {
        let service = SqliteSessionService::new(":memory:").await.unwrap();
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
    async fn test_sqlite_get_session() {
        let service = SqliteSessionService::new(":memory:").await.unwrap();
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
    async fn test_sqlite_list_sessions() {
        let service = SqliteSessionService::new(":memory:").await.unwrap();
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
                limit: None,
                offset: None,
            })
            .await
            .unwrap();

        assert_eq!(sessions.len(), 2);
    }

    #[tokio::test]
    async fn test_sqlite_delete_session() {
        let service = SqliteSessionService::new(":memory:").await.unwrap();
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
    async fn test_sqlite_append_event_persists_state_and_identity() {
        let service = SqliteSessionService::new(":memory:").await.unwrap();
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
    async fn test_sqlite_delete_cleans_up_events_for_recreated_session() {
        let service = SqliteSessionService::new(":memory:").await.unwrap();
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

    /// Collects formatted tracing output so tests can assert on emitted warnings.
    #[derive(Clone, Default)]
    struct CapturedLogs(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for CapturedLogs {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedLogs {
        type Writer = CapturedLogs;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    #[tokio::test]
    async fn test_sqlite_get_warns_and_skips_undecodable_event() {
        let logs = CapturedLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_max_level(tracing::Level::WARN)
            .with_ansi(false)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let service = SqliteSessionService::new(":memory:").await.unwrap();
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

        let mut kept = Event::new("inv-kept");
        kept.id = "event-kept".to_string();
        kept.timestamp = Utc::now();
        let mut corrupt = Event::new("inv-corrupt");
        corrupt.id = "event-corrupt".to_string();
        corrupt.timestamp = kept.timestamp + Duration::seconds(1);
        service.append_event("session1", kept).await.unwrap();
        service.append_event("session1", corrupt).await.unwrap();

        sqlx::query("UPDATE events SET actions = 'not json' WHERE id = 'event-corrupt'")
            .execute(service.pool())
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

        let event_ids: Vec<String> = session.events().all().into_iter().map(|e| e.id).collect();
        assert_eq!(event_ids, vec!["event-kept".to_string()]);

        let output = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
        assert!(output.contains("skipping stored event that failed to deserialize"), "{output}");
        assert!(output.contains("event.id=event-corrupt"), "{output}");
    }
}
