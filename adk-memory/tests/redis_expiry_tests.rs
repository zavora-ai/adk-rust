//! Native expiry of the Redis memory backend against a live server: a session list expires
//! with its latest entry, stays persistent while it holds an entry that never expires, and
//! `purge_expired` drops the index member of a list Redis removed.
//!
//! Run with a server at `REDIS_URL` (default `redis://localhost:6379`):
//!
//! ```bash
//! cargo nextest run -p adk-memory --features redis-memory --run-ignored only redis_expiry
//! ```

#![cfg(feature = "redis-memory")]

use adk_core::Content;
use adk_memory::{MemoryEntry, MemoryService, RedisMemoryConfig, RedisMemoryService};
use fred::prelude::*;
use std::time::Duration;

fn url() -> String {
    std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_string())
}

fn entry(text: &str) -> MemoryEntry {
    MemoryEntry::new(Content::new("user").with_text(text), "user")
}

async fn raw_client() -> Client {
    let client = Builder::from_config(Config::from_url(&url()).unwrap()).build().unwrap();
    client.init().await.expect("connect to redis");
    client
}

#[tokio::test]
#[ignore = "needs a Redis server at REDIS_URL"]
async fn redis_expiry_a_session_list_expires_with_its_latest_entry() {
    let service = RedisMemoryService::new(RedisMemoryConfig { url: url(), ttl: None })
        .await
        .expect("connect to redis");
    let redis = raw_client().await;
    let user = format!("u{}", uuid::Uuid::new_v4().simple());
    let key = format!("mem:app:{user}:s1");

    service
        .add_session(
            "app",
            &user,
            "s1",
            vec![
                entry("short").with_ttl(Duration::from_secs(60)),
                entry("long").with_ttl(Duration::from_secs(600)),
            ],
        )
        .await
        .unwrap();
    let ttl: i64 = redis.pttl(&key).await.unwrap();
    assert!((590_000..=600_000).contains(&ttl), "the key lives as long as its latest entry: {ttl}");

    service
        .add_session("app", &user, "s1", vec![entry("shorter").with_ttl(Duration::from_secs(5))])
        .await
        .unwrap();
    let ttl: i64 = redis.pttl(&key).await.unwrap();
    assert!(ttl > 590_000, "a shorter entry never shortens the key: {ttl}");

    service.add_session("app", &user, "s1", vec![entry("forever")]).await.unwrap();
    assert_eq!(redis.pttl::<i64, _>(&key).await.unwrap(), -1, "a lasting entry persists the key");

    service
        .add_session("app", &user, "s1", vec![entry("later").with_ttl(Duration::from_secs(60))])
        .await
        .unwrap();
    assert_eq!(redis.pttl::<i64, _>(&key).await.unwrap(), -1, "and keeps it persistent");
    service.delete_user("app", &user).await.unwrap();
}

#[tokio::test]
#[ignore = "needs a Redis server at REDIS_URL"]
async fn redis_expiry_redis_removes_an_expired_list_and_the_purge_drops_its_index_member() {
    let service = RedisMemoryService::new(RedisMemoryConfig {
        url: url(),
        ttl: Some(Duration::from_millis(500)),
    })
    .await
    .expect("connect to redis");
    let redis = raw_client().await;
    let user = format!("u{}", uuid::Uuid::new_v4().simple());

    service.add_session("app", &user, "s1", vec![entry("brief")]).await.unwrap();
    let index = format!("mem_idx:app:{user}");
    assert!(redis.sismember::<bool, _, _>(&index, "s1").await.unwrap());

    tokio::time::sleep(Duration::from_millis(900)).await;
    assert_eq!(
        redis.exists::<i64, _>(format!("mem:app:{user}:s1")).await.unwrap(),
        0,
        "Redis removed the list itself"
    );
    service.purge_expired().await.unwrap();
    assert!(
        !redis.sismember::<bool, _, _>(&index, "s1").await.unwrap(),
        "the purge dropped the index member of the removed list"
    );
}
