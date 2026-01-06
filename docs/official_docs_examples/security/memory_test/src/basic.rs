//! Memory doc-test - validates memory.md documentation

use adk_memory::{InMemoryMemoryService, MemoryService, MemoryEntry, SearchRequest};
use adk_core::Content;
use chrono::Utc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Memory Doc-Test ===\n");

    // From docs: MemoryEntry creation
    let entry = MemoryEntry {
        content: Content::new("user").with_text("I prefer dark mode"),
        author: "user".to_string(),
        timestamp: Utc::now(),
    };
    assert_eq!(entry.author, "user");
    println!("✓ MemoryEntry creation works");

    // From docs: InMemoryMemoryService
    let memory = InMemoryMemoryService::new();

    // From docs: Store memories from a session
    let entries = vec![
        MemoryEntry {
            content: Content::new("user").with_text("I like Rust programming"),
            author: "user".to_string(),
            timestamp: Utc::now(),
        },
        MemoryEntry {
            content: Content::new("assistant").with_text("Rust is great for systems programming"),
            author: "assistant".to_string(),
            timestamp: Utc::now(),
        },
    ];

    memory.add_session("my_app", "user-123", "session-1", entries).await?;
    println!("✓ add_session works");

    // From docs: Search memories
    let request = SearchRequest {
        query: "Rust".to_string(),
        user_id: "user-123".to_string(),
        app_name: "my_app".to_string(),
    };

    let response = memory.search(request).await?;
    assert!(!response.memories.is_empty());
    println!("✓ search works - found {} memories", response.memories.len());

    // From docs: Memory isolation by user
    let entries_a = vec![MemoryEntry {
        content: Content::new("user").with_text("User A topic"),
        author: "user".to_string(),
        timestamp: Utc::now(),
    }];
    let entries_b = vec![MemoryEntry {
        content: Content::new("user").with_text("User B topic"),
        author: "user".to_string(),
        timestamp: Utc::now(),
    }];

    memory.add_session("app", "user-a", "sess-1", entries_a).await?;
    memory.add_session("app", "user-b", "sess-1", entries_b).await?;

    // Search only returns user-a's memories
    let request = SearchRequest {
        query: "topic".to_string(),
        user_id: "user-a".to_string(),
        app_name: "app".to_string(),
    };
    let response = memory.search(request).await?;
    assert_eq!(response.memories.len(), 1);
    
    // Verify it's user-a's memory
    let text: String = response.memories[0].content.parts
        .iter()
        .filter_map(|p| p.text())
        .collect();
    assert!(text.contains("User A"));
    println!("✓ Memory isolation by user works");

    // From docs: Memory isolation by app
    let request = SearchRequest {
        query: "Rust".to_string(),
        user_id: "user-123".to_string(),
        app_name: "different_app".to_string(),  // Different app
    };
    let response = memory.search(request).await?;
    assert!(response.memories.is_empty());
    println!("✓ Memory isolation by app works");

    println!("\n=== All memory tests passed! ===");
    Ok(())
}
