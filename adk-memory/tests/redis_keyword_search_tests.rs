//! Keyword search of the Redis memory backend against a live server: the same word
//! segmentation and ranking as `InMemoryMemoryService` (`keyword_search_tests.rs`).
//!
//! Run with a server at `REDIS_URL` (default `redis://localhost:6379`):
//!
//! ```bash
//! cargo nextest run -p adk-memory --features redis-memory --run-ignored only redis_keyword
//! ```

#![cfg(feature = "redis-memory")]

use adk_core::Content;
use adk_memory::{
    MemoryEntry, MemoryService, RedisMemoryConfig, RedisMemoryService, SearchRequest, text,
};
use chrono::{Duration, Utc};

fn entry(text: &str, minutes_ago: i64) -> MemoryEntry {
    MemoryEntry {
        content: Content::new("user").with_text(text),
        author: "user".to_string(),
        timestamp: Utc::now() - Duration::minutes(minutes_ago),
    }
}

async fn service() -> RedisMemoryService {
    let url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_string());
    RedisMemoryService::new(RedisMemoryConfig { url, ttl: None }).await.expect("connect to redis")
}

async fn texts(service: &RedisMemoryService, app: &str, query: &str, limit: usize) -> Vec<String> {
    service
        .search(SearchRequest {
            query: query.to_string(),
            user_id: "u1".to_string(),
            app_name: app.to_string(),
            limit: Some(limit),
            min_score: None,
            project_id: None,
        })
        .await
        .expect("search")
        .memories
        .iter()
        .map(|memory| text::extract_text(&memory.content))
        .collect()
}

#[tokio::test]
#[ignore = "requires a Redis server at REDIS_URL"]
async fn redis_keyword_words_next_to_punctuation_match() {
    let service = service().await;
    let app = format!("keyword-{}", uuid::Uuid::new_v4());
    service
        .add_session(&app, "u1", "s1", vec![entry("Hometown: Nairobi.", 2), entry("teal!", 1)])
        .await
        .unwrap();

    assert_eq!(texts(&service, &app, "hometown", 10).await, vec!["Hometown: Nairobi."]);
    assert_eq!(texts(&service, &app, "teal", 10).await, vec!["teal!"]);
    service.delete_user(&app, "u1").await.unwrap();
}

#[tokio::test]
#[ignore = "requires a Redis server at REDIS_URL"]
async fn redis_keyword_the_entry_matching_the_most_query_words_ranks_first() {
    let service = service().await;
    let app = format!("keyword-{}", uuid::Uuid::new_v4());
    for index in 0..30 {
        service
            .add_session(
                &app,
                "u1",
                &format!("decoy-{index}"),
                vec![entry(&format!("Rust note {index}"), index)],
            )
            .await
            .unwrap();
    }
    service
        .add_session(&app, "u1", "target", vec![entry("Rust ownership and borrowing", 99)])
        .await
        .unwrap();

    assert_eq!(
        texts(&service, &app, "rust ownership borrowing", 3).await,
        vec!["Rust ownership and borrowing", "Rust note 0", "Rust note 1"]
    );
    service.delete_user(&app, "u1").await.unwrap();
}
