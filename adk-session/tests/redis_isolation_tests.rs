//! Tenant isolation of the Redis session backend against a live server.
//!
//! Key construction is covered by `redis_keys_property_tests.rs`; these tests
//! confirm identifiers containing `:` stay apart end to end, including the
//! reverse lookup `append_event` uses to find a session's owner.
//!
//! Run with a server at `REDIS_URL` (default `redis://localhost:6379`):
//!
//! ```bash
//! cargo nextest run -p adk-session --features redis --run-ignored only redis_isolation
//! ```

#![cfg(feature = "redis")]

use adk_session::*;
use serde_json::json;
use std::collections::HashMap;

async fn service() -> RedisSessionService {
    let url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_string());
    RedisSessionService::new(RedisSessionConfig { url, ttl: None, cluster_nodes: None })
        .await
        .expect("connect to redis")
}

async fn create(service: &RedisSessionService, app: &str, user: &str, session: &str, owner: &str) {
    service
        .create(CreateRequest {
            app_name: app.to_string(),
            user_id: user.to_string(),
            session_id: Some(session.to_string()),
            state: HashMap::from([("owner".to_string(), json!(owner))]),
        })
        .await
        .expect("create session");
}

async fn get(
    service: &RedisSessionService,
    app: &str,
    user: &str,
    session: &str,
) -> Box<dyn Session> {
    service
        .get(GetRequest {
            app_name: app.to_string(),
            user_id: user.to_string(),
            session_id: session.to_string(),
            num_recent_events: None,
            after: None,
        })
        .await
        .expect("get session")
}

#[tokio::test]
#[ignore = "requires a Redis server at REDIS_URL"]
async fn redis_isolation_colon_identifiers_do_not_share_sessions() {
    let service = service().await;
    let run = uuid::Uuid::new_v4();
    let (app, app_with_colon) = (format!("iso-{run}"), format!("iso-{run}:b"));
    let (first, second) = (format!("s1-{run}"), format!("s2-{run}"));

    // Before the fix both sessions' user-state hashes were `user_state:iso-…:b:c`.
    create(&service, &app_with_colon, "c", &first, "first").await;
    create(&service, &app, "b:c", &second, "second").await;

    let mut event = Event::new("inv-1");
    event.author = "agent".to_string();
    event.actions.state_delta.insert("user:marker".to_string(), json!("first-only"));
    service.append_event(&first, event).await.expect("append via reverse lookup");

    let one = get(&service, &app_with_colon, "c", &first).await;
    let two = get(&service, &app, "b:c", &second).await;
    assert_eq!(
        [one.state().get("owner"), one.state().get("user:marker"), Some(json!(one.events().len()))],
        [Some(json!("first")), Some(json!("first-only")), Some(json!(1))]
    );
    assert_eq!(
        [two.state().get("owner"), two.state().get("user:marker"), Some(json!(two.events().len()))],
        [Some(json!("second")), None, Some(json!(0))]
    );

    for (app_name, user_id, session_id) in [(&app_with_colon, "c", &first), (&app, "b:c", &second)]
    {
        service
            .delete(DeleteRequest {
                app_name: app_name.clone(),
                user_id: user_id.to_string(),
                session_id: session_id.clone(),
            })
            .await
            .expect("delete session");
    }
}
