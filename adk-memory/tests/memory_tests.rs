use adk_core::{Content, Part};
use adk_memory::*;
use chrono::Utc;

#[tokio::test]
async fn test_add_and_search() {
    let service = InMemoryMemoryService::new();

    let entries = vec![
        MemoryEntry {
            content: Content::new("assistant").with_text("The weather is sunny today"),
            author: "assistant".to_string(),
            timestamp: Utc::now(),
        },
        MemoryEntry {
            content: Content::new("assistant").with_text("I like programming in Rust"),
            author: "assistant".to_string(),
            timestamp: Utc::now(),
        },
    ];

    service.add_session("app1", "user1", "session1", entries).await.unwrap();

    let search_resp = service
        .search(SearchRequest {
            query: "weather sunny".to_string(),
            user_id: "user1".to_string(),
            app_name: "app1".to_string(),
            limit: None,
            min_score: None,
            project_id: None,
        })
        .await
        .unwrap();

    assert_eq!(search_resp.memories.len(), 1);
    if let Part::Text { text } = &search_resp.memories[0].content.parts[0] {
        assert!(text.contains("weather"));
    }
}

#[tokio::test]
async fn test_search_no_results() {
    let service = InMemoryMemoryService::new();

    let entries = vec![MemoryEntry {
        content: Content::new("assistant").with_text("The weather is sunny"),
        author: "assistant".to_string(),
        timestamp: Utc::now(),
    }];

    service.add_session("app1", "user1", "session1", entries).await.unwrap();

    let search_resp = service
        .search(SearchRequest {
            query: "programming rust".to_string(),
            user_id: "user1".to_string(),
            app_name: "app1".to_string(),
            limit: None,
            min_score: None,
            project_id: None,
        })
        .await
        .unwrap();

    assert_eq!(search_resp.memories.len(), 0);
}

#[tokio::test]
async fn test_multiple_sessions() {
    let service = InMemoryMemoryService::new();

    service
        .add_session(
            "app1",
            "user1",
            "session1",
            vec![MemoryEntry {
                content: Content::new("assistant").with_text("First session content"),
                author: "assistant".to_string(),
                timestamp: Utc::now(),
            }],
        )
        .await
        .unwrap();

    service
        .add_session(
            "app1",
            "user1",
            "session2",
            vec![MemoryEntry {
                content: Content::new("assistant").with_text("Second session content"),
                author: "assistant".to_string(),
                timestamp: Utc::now(),
            }],
        )
        .await
        .unwrap();

    let search_resp = service
        .search(SearchRequest {
            query: "session content".to_string(),
            user_id: "user1".to_string(),
            app_name: "app1".to_string(),
            limit: None,
            min_score: None,
            project_id: None,
        })
        .await
        .unwrap();

    assert_eq!(search_resp.memories.len(), 2);
}

#[tokio::test]
async fn test_user_isolation() {
    let service = InMemoryMemoryService::new();

    service
        .add_session(
            "app1",
            "user1",
            "session1",
            vec![MemoryEntry {
                content: Content::new("assistant").with_text("User1 data"),
                author: "assistant".to_string(),
                timestamp: Utc::now(),
            }],
        )
        .await
        .unwrap();

    service
        .add_session(
            "app1",
            "user2",
            "session1",
            vec![MemoryEntry {
                content: Content::new("assistant").with_text("User2 data"),
                author: "assistant".to_string(),
                timestamp: Utc::now(),
            }],
        )
        .await
        .unwrap();

    let search_resp = service
        .search(SearchRequest {
            query: "data".to_string(),
            user_id: "user1".to_string(),
            app_name: "app1".to_string(),
            limit: None,
            min_score: None,
            project_id: None,
        })
        .await
        .unwrap();

    assert_eq!(search_resp.memories.len(), 1);
    if let Part::Text { text } = &search_resp.memories[0].content.parts[0] {
        assert!(text.contains("User1"));
    }
}

#[tokio::test]
async fn test_empty_content_filtered() {
    let service = InMemoryMemoryService::new();

    let entries = vec![MemoryEntry {
        content: Content::new("assistant"),
        author: "assistant".to_string(),
        timestamp: Utc::now(),
    }];

    service.add_session("app1", "user1", "session1", entries).await.unwrap();

    let search_resp = service
        .search(SearchRequest {
            query: "anything".to_string(),
            user_id: "user1".to_string(),
            app_name: "app1".to_string(),
            limit: None,
            min_score: None,
            project_id: None,
        })
        .await
        .unwrap();

    assert_eq!(search_resp.memories.len(), 0);
}

// === Task 14.2: Backward compatibility — SearchRequest deserialization ===

#[test]
fn test_search_request_deserializes_without_project_id() {
    let json = r#"{"query":"test","user_id":"user1","app_name":"app1"}"#;
    let req: adk_memory::SearchRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.project_id, None);
    assert_eq!(req.query, "test");
    assert_eq!(req.user_id, "user1");
    assert_eq!(req.app_name, "app1");
}

#[test]
fn test_search_request_deserializes_with_project_id() {
    let json = r#"{"query":"test","user_id":"user1","app_name":"app1","project_id":"proj1"}"#;
    let req: adk_memory::SearchRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.project_id, Some("proj1".to_string()));
}

// === Task 14.3: Backward compatibility — Adapter without project_id ===

#[tokio::test]
async fn test_adapter_without_project_preserves_behavior() {
    use adk_core::Memory;
    use std::sync::Arc;

    let service = Arc::new(InMemoryMemoryService::new());

    // Add a global entry via the service directly
    service
        .add_session(
            "app1",
            "user1",
            "session1",
            vec![MemoryEntry {
                content: Content::new("assistant").with_text("global memory data"),
                author: "assistant".to_string(),
                timestamp: Utc::now(),
            }],
        )
        .await
        .unwrap();

    // Create adapter WITHOUT with_project_id()
    let adapter = MemoryServiceAdapter::new(service, "app1", "user1");

    // Search should return global entries
    let results = adapter.search("global memory").await.unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].author, "assistant");

    // Add via adapter (should be global)
    adapter
        .add(adk_core::MemoryEntry {
            content: Content::new("user").with_text("adapter added data"),
            author: "user".to_string(),
        })
        .await
        .unwrap();

    // Both entries should be searchable as global
    let results = adapter.search("data").await.unwrap();
    assert_eq!(results.len(), 2);
}
