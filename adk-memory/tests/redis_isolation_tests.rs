//! Tenant isolation of the Redis memory backend against a live server.
//!
//! The key-construction and SCAN-pattern logic is unit-tested in
//! `src/redis.rs`; these tests confirm the same boundaries hold end to end.
//!
//! Run with a server at `REDIS_URL` (default `redis://localhost:6379`):
//!
//! ```bash
//! cargo nextest run -p adk-memory --features redis-memory --run-ignored only redis_isolation
//! ```

#![cfg(feature = "redis-memory")]

use adk_core::Content;
use adk_memory::{
    MemoryEntry, MemoryService, RedisMemoryConfig, RedisMemoryService, SearchRequest,
};

fn entry(text: &str) -> MemoryEntry {
    MemoryEntry {
        content: Content::new("user").with_text(text),
        author: "user".to_string(),
        timestamp: chrono::Utc::now(),
    }
}

async fn service() -> RedisMemoryService {
    let url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_string());
    RedisMemoryService::new(RedisMemoryConfig { url, ttl: None }).await.expect("connect to redis")
}

async fn count(
    service: &RedisMemoryService,
    app: &str,
    user: &str,
    project: Option<&str>,
) -> usize {
    service
        .search(SearchRequest {
            query: "remembered".to_string(),
            user_id: user.to_string(),
            app_name: app.to_string(),
            limit: Some(100),
            min_score: None,
            project_id: project.map(str::to_string),
        })
        .await
        .expect("search")
        .memories
        .len()
}

#[tokio::test]
#[ignore = "requires a Redis server at REDIS_URL"]
async fn redis_isolation_delete_user_spares_prefix_sharing_and_glob_users() {
    let service = service().await;
    let app = format!("isolation-{}", uuid::Uuid::new_v4());
    let users = ["alice", "alice2", "alice-admin", "alice:x", "*"];

    for user in users {
        service.add_session(&app, user, "s1", vec![entry("remembered fact")]).await.unwrap();
        service
            .add_session_to_project(&app, user, "s1", "proj", vec![entry("remembered project")])
            .await
            .unwrap();
    }

    service.delete_user(&app, "*").await.unwrap();
    assert_eq!(count(&service, &app, "*", Some("proj")).await, 0);
    for user in ["alice", "alice2", "alice-admin", "alice:x"] {
        assert_eq!(count(&service, &app, user, Some("proj")).await, 2, "{user} lost data");
    }

    service.delete_user(&app, "alice").await.unwrap();
    assert_eq!(count(&service, &app, "alice", Some("proj")).await, 0);
    for user in ["alice2", "alice-admin", "alice:x"] {
        assert_eq!(count(&service, &app, user, Some("proj")).await, 2, "{user} lost data");
    }

    for user in users {
        service.delete_user(&app, user).await.unwrap();
    }
}

#[tokio::test]
#[ignore = "requires a Redis server at REDIS_URL"]
async fn redis_isolation_colon_identifiers_do_not_share_keys() {
    let service = service().await;
    let app = format!("isolation-{}", uuid::Uuid::new_v4());
    let app_with_colon = format!("{app}:b");

    service.add_session(&app_with_colon, "c", "s1", vec![entry("remembered one")]).await.unwrap();
    service.add_session(&app, "b:c", "s1", vec![entry("remembered two")]).await.unwrap();

    assert_eq!(count(&service, &app_with_colon, "c", None).await, 1);
    assert_eq!(count(&service, &app, "b:c", None).await, 1);

    service.delete_user(&app, "b:c").await.unwrap();
    assert_eq!(count(&service, &app_with_colon, "c", None).await, 1);
    service.delete_user(&app_with_colon, "c").await.unwrap();
}
