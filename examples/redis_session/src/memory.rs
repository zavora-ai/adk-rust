//! Redis memory service example.
//!
//! Demonstrates storing and searching memory entries using Redis.
//!
//! # Prerequisites
//!
//! ```bash
//! docker run -d --name adk-redis-test -p 6399:6379 redis:7-alpine
//! ```
//!
//! # Run
//!
//! ```bash
//! cargo run -p redis-session-example --bin redis-memory-example
//! ```

use adk_core::Content;
use adk_memory::{
    MemoryEntry, MemoryService, RedisMemoryConfig, RedisMemoryService, SearchRequest,
};
use chrono::Utc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let config = RedisMemoryConfig { url: "redis://localhost:6399".into(), ttl: None };
    let service = RedisMemoryService::new(config).await?;
    println!("connected to redis for memory storage");

    // Health check
    service.health_check().await?;
    println!("health check passed");

    // Add memory entries for session 1
    let entries_s1 = vec![
        MemoryEntry {
            content: Content::new("user").with_text("I love Rust programming and async patterns"),
            author: "user".into(),
            timestamp: Utc::now(),
        },
        MemoryEntry {
            content: Content::new("assistant").with_text("Rust has great async support with tokio"),
            author: "assistant".into(),
            timestamp: Utc::now(),
        },
    ];
    service.add_session("test_app", "user1", "session1", entries_s1).await?;
    println!("added 2 entries to session1");

    // Add memory entries for session 2
    let entries_s2 = vec![MemoryEntry {
        content: Content::new("user")
            .with_text("Python is good for data science and machine learning"),
        author: "user".into(),
        timestamp: Utc::now(),
    }];
    service.add_session("test_app", "user1", "session2", entries_s2).await?;
    println!("added 1 entry to session2");

    // Search for "Rust"
    let results = service
        .search(SearchRequest {
            app_name: "test_app".into(),
            user_id: "user1".into(),
            query: "Rust async".into(),
            limit: Some(10),
            min_score: None,
        })
        .await?;
    println!("search 'Rust async' returned {} results", results.memories.len());
    for (i, entry) in results.memories.iter().enumerate() {
        let text: String = entry
            .content
            .parts
            .iter()
            .filter_map(|p| match p {
                adk_core::Part::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ");
        println!("  [{i}] ({}) {text}", entry.author);
    }

    // Search for "Python" — should find session2 entry
    let results = service
        .search(SearchRequest {
            app_name: "test_app".into(),
            user_id: "user1".into(),
            query: "Python".into(),
            limit: Some(10),
            min_score: None,
        })
        .await?;
    println!("search 'Python' returned {} results", results.memories.len());

    // Delete session1 memories
    service.delete_session("test_app", "user1", "session1").await?;
    println!("deleted session1 memories");

    // Verify session1 entries are gone
    let results = service
        .search(SearchRequest {
            app_name: "test_app".into(),
            user_id: "user1".into(),
            query: "Rust".into(),
            limit: Some(10),
            min_score: None,
        })
        .await?;
    println!("search 'Rust' after delete returned {} results (expected 0)", results.memories.len());

    // GDPR delete — remove all user data
    service.delete_user("test_app", "user1").await?;
    println!("GDPR delete completed for user1");

    // Verify all gone
    let results = service
        .search(SearchRequest {
            app_name: "test_app".into(),
            user_id: "user1".into(),
            query: "Python".into(),
            limit: Some(10),
            min_score: None,
        })
        .await?;
    println!(
        "search 'Python' after GDPR delete returned {} results (expected 0)",
        results.memories.len()
    );

    println!("\nredis memory example completed successfully");
    Ok(())
}
