//! SQLite memory service example.
//!
//! Demonstrates storing and searching memory entries using SQLite with FTS5.
//! No external infrastructure needed — uses in-memory SQLite.
//!
//! # Run
//!
//! ```bash
//! cargo run -p sqlite-memory-example
//! ```

use adk_core::Content;
use adk_memory::{MemoryEntry, MemoryService, SearchRequest, SqliteMemoryService};
use chrono::Utc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let service = SqliteMemoryService::new("sqlite::memory:").await?;
    service.migrate().await?;
    println!("sqlite memory service initialized with FTS5");

    service.health_check().await?;
    println!("health check passed");

    // Add entries for session 1
    let entries_s1 = vec![
        MemoryEntry {
            content: Content::new("user").with_text("I love Rust programming and async patterns"),
            author: "user".into(),
            timestamp: Utc::now(),
        },
        MemoryEntry {
            content: Content::new("assistant")
                .with_text("Rust has great async support with tokio runtime"),
            author: "assistant".into(),
            timestamp: Utc::now(),
        },
    ];
    service.add_session("test_app", "user1", "session1", entries_s1).await?;
    println!("added 2 entries to session1");

    // Add entries for session 2
    let entries_s2 = vec![MemoryEntry {
        content: Content::new("user")
            .with_text("Python is good for data science and machine learning"),
        author: "user".into(),
        timestamp: Utc::now(),
    }];
    service.add_session("test_app", "user1", "session2", entries_s2).await?;
    println!("added 1 entry to session2");

    // FTS5 search for "Rust"
    let results = service
        .search(SearchRequest {
            app_name: "test_app".into(),
            user_id: "user1".into(),
            query: "Rust".into(),
            limit: Some(10),
            min_score: None,
        })
        .await?;
    println!("search 'Rust' returned {} results", results.memories.len());
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

    // FTS5 search for "Python"
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

    // Delete session1
    service.delete_session("test_app", "user1", "session1").await?;
    println!("deleted session1 memories");

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

    // GDPR delete
    service.delete_user("test_app", "user1").await?;
    println!("GDPR delete completed for user1");

    let results = service
        .search(SearchRequest {
            app_name: "test_app".into(),
            user_id: "user1".into(),
            query: "Python".into(),
            limit: Some(10),
            min_score: None,
        })
        .await?;
    println!("search after GDPR delete returned {} results (expected 0)", results.memories.len());

    println!("\nsqlite memory example completed successfully");
    Ok(())
}
