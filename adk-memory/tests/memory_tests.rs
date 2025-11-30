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
        })
        .await
        .unwrap();

    assert_eq!(search_resp.memories.len(), 0);
}
