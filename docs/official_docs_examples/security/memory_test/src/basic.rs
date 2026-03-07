//! Memory Service Basic Example
//!
//! Demonstrates adding and searching long-term semantic memory.

use adk_core::types::{SessionId, UserId};
use adk_core::{Content, Role};
use adk_memory::{InMemoryMemoryService, MemoryEntry, MemoryService, SearchRequest};
use chrono::Utc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let memory = InMemoryMemoryService::new();

    // 1. Add some memories for a user
    let entries = vec![
        MemoryEntry {
            content: Content::new(Role::Model).with_text("The user's favorite color is blue."),
            author: "assistant".to_string(),
            timestamp: Utc::now(),
        },
        MemoryEntry {
            content: Content::new(Role::Model).with_text("The user lives in Richmond, VA."),
            author: "assistant".to_string(),
            timestamp: Utc::now(),
        },
    ];

    memory
        .add_session(
            "my_app",
            &UserId::new("user-123").unwrap(),
            &SessionId::new("session-1").unwrap(),
            entries,
        )
        .await?;

    // 2. Search memories
    let search_resp = memory
        .search(SearchRequest {
            query: "What is the user's favorite color?".to_string(),
            user_id: UserId::new("user-123").unwrap(),
            app_name: "my_app".to_string(),
        })
        .await?;

    println!("Found {} memories:", search_resp.memories.len());
    for mem in search_resp.memories {
        println!("  - {}", mem.content.text());
    }

    // 3. User Isolation
    let entries_a = vec![MemoryEntry {
        content: Content::new(Role::Model).with_text("Secret A"),
        author: "assistant".to_string(),
        timestamp: Utc::now(),
    }];
    let entries_b = vec![MemoryEntry {
        content: Content::new(Role::Model).with_text("Secret B"),
        author: "assistant".to_string(),
        timestamp: Utc::now(),
    }];

    memory
        .add_session(
            "app",
            &UserId::new("user-a").unwrap(),
            &SessionId::new("sess-1").unwrap(),
            entries_a,
        )
        .await?;
    memory
        .add_session(
            "app",
            &UserId::new("user-b").unwrap(),
            &SessionId::new("sess-1").unwrap(),
            entries_b,
        )
        .await?;

    let search_a = memory
        .search(SearchRequest {
            query: "Secret".to_string(),
            user_id: UserId::new("user-a").unwrap(),
            app_name: "app".to_string(),
        })
        .await?;

    println!("\nUser A search results: {}", search_a.memories.len());
    assert_eq!(search_a.memories[0].content.text(), "Secret A");

    // 4. Persistence verification (InMemory is per-instance)
    println!("\nMemory service is working correctly!");

    Ok(())
}
