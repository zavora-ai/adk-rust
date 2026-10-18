//! Keyword search of `InMemoryMemoryService`: word segmentation, ranking, and wordless entries.

use adk_core::{Content, Part};
use adk_memory::*;
use chrono::{Duration, Utc};

fn entry(text: &str, minutes_ago: i64) -> MemoryEntry {
    MemoryEntry {
        content: Content::new("user").with_text(text),
        author: "user".to_string(),
        timestamp: Utc::now() - Duration::minutes(minutes_ago),
    }
}

fn request(query: &str, limit: Option<usize>, min_score: Option<f32>) -> SearchRequest {
    SearchRequest {
        query: query.to_string(),
        user_id: "u1".to_string(),
        app_name: "app".to_string(),
        limit,
        min_score,
        project_id: None,
    }
}

async fn texts(service: &InMemoryMemoryService, request: SearchRequest) -> Vec<String> {
    service
        .search(request)
        .await
        .unwrap()
        .memories
        .iter()
        .map(|memory| text::extract_text(&memory.content))
        .collect()
}

#[tokio::test]
async fn words_next_to_punctuation_match() {
    let service = InMemoryMemoryService::new();
    service
        .add_session(
            "app",
            "u1",
            "s1",
            vec![
                entry("Hometown: Nairobi.", 3),
                entry("Favourite colour: teal!", 2),
                entry("(Kibo) is the dog's name", 1),
            ],
        )
        .await
        .unwrap();

    assert_eq!(texts(&service, request("hometown", None, None)).await, vec!["Hometown: Nairobi."]);
    assert_eq!(texts(&service, request("Nairobi?", None, None)).await, vec!["Hometown: Nairobi."]);
    assert_eq!(texts(&service, request("teal", None, None)).await, vec!["Favourite colour: teal!"]);
    assert_eq!(
        texts(&service, request("kibo", None, None)).await,
        vec!["(Kibo) is the dog's name"]
    );
}

#[tokio::test]
async fn the_entry_matching_the_most_query_words_ranks_first() {
    // Each store iterates its sessions in a different order; ranking must not depend on it.
    for _ in 0..10 {
        let service = InMemoryMemoryService::new();
        for index in 0..30 {
            service
                .add_session(
                    "app",
                    "u1",
                    &format!("decoy-{index}"),
                    vec![entry(&format!("Rust note {index}"), index)],
                )
                .await
                .unwrap();
        }
        service
            .add_session(
                "app",
                "u1",
                "target",
                vec![entry("Rust ownership and borrowing, explained", 99)],
            )
            .await
            .unwrap();

        let found = texts(&service, request("rust ownership borrowing", Some(3), None)).await;

        // The full match first, then the newest partial matches.
        assert_eq!(
            found,
            vec!["Rust ownership and borrowing, explained", "Rust note 0", "Rust note 1"]
        );
    }
}

#[tokio::test]
async fn min_score_keeps_entries_matching_that_share_of_the_query() {
    let service = InMemoryMemoryService::new();
    service
        .add_session(
            "app",
            "u1",
            "s1",
            vec![
                entry("rust ownership borrowing", 3),
                entry("rust ownership", 2),
                entry("rust", 1),
            ],
        )
        .await
        .unwrap();

    let query = "rust ownership borrowing";
    assert_eq!(
        texts(&service, request(query, None, Some(1.0))).await,
        vec!["rust ownership borrowing"]
    );
    assert_eq!(texts(&service, request(query, None, Some(0.5))).await.len(), 2);
    assert_eq!(texts(&service, request(query, None, None)).await.len(), 3);
}

#[tokio::test]
async fn entries_without_words_are_kept() {
    let service = InMemoryMemoryService::new();
    let image = MemoryEntry {
        content: Content {
            role: "user".to_string(),
            parts: vec![Part::InlineData {
                mime_type: "image/png".to_string(),
                data: vec![0x89, b'P', b'N', b'G'],
                uri: None,
                annotations: None,
            }],
        },
        author: "user".to_string(),
        timestamp: Utc::now(),
    };
    service
        .add_session("app", "u1", "s1", vec![image.clone(), entry("photo of the whiteboard", 1)])
        .await
        .unwrap();
    service.add_entry("app", "u1", image).await.unwrap();

    let recent = service.list_recent("app", "u1", 10).await.unwrap();
    let images =
        recent.iter().filter(|memory| matches!(memory.content.parts[0], Part::InlineData { .. }));
    assert_eq!(images.count(), 2);
    assert_eq!(recent.len(), 3);
    // An entry without words matches no keyword query.
    assert_eq!(
        texts(&service, request("photo png image", None, None)).await,
        vec!["photo of the whiteboard"]
    );
}
