//! A memory keeps the artifacts it refers to beside its text: the text is what a search
//! matches, and the `Part::FileData` references come back with the entry.

use adk_core::Content;
use adk_memory::text::{ArtifactReference, extract_text};
use adk_memory::{InMemoryMemoryService, MemoryEntry, MemoryService, SearchRequest};

const FRAME: &str = "artifact://inspector/u1/s1/vision-ab12.png#0";

fn caption() -> MemoryEntry {
    MemoryEntry::new(Content::new("model").with_text("torn seal on carton 117"), "inspector")
        .with_id("caption-117")
        .with_artifact(FRAME, "image/png")
}

fn request(query: &str) -> SearchRequest {
    SearchRequest {
        query: query.into(),
        user_id: "u1".into(),
        app_name: "inspector".into(),
        limit: Some(5),
        min_score: None,
        project_id: None,
    }
}

async fn round_trip(service: &dyn MemoryService) {
    service.add_session("inspector", "u1", "s1", vec![caption()]).await.unwrap();
    let found = service.search(request("torn seal")).await.unwrap().memories;
    assert_eq!(found.len(), 1);
    assert_eq!(extract_text(&found[0].content), "torn seal on carton 117");
    assert_eq!(found[0].artifact_references(), vec![ArtifactReference::new(FRAME, "image/png")]);
    // The reference is not searchable text.
    assert!(service.search(request("vision-ab12")).await.unwrap().memories.is_empty());
    assert_eq!(service.delete_memory("inspector", "u1", "caption-117").await.unwrap(), 1);
}

#[tokio::test]
async fn in_memory_keeps_artifact_references() {
    round_trip(&InMemoryMemoryService::new()).await;
}

#[cfg(feature = "sqlite-memory")]
#[tokio::test]
async fn sqlite_keeps_artifact_references() {
    let service = adk_memory::SqliteMemoryService::new("sqlite::memory:").await.unwrap();
    service.migrate().await.unwrap();
    round_trip(&service).await;
}

#[cfg(feature = "graph-memory")]
#[tokio::test]
async fn graph_episodic_log_keeps_artifact_references() {
    let service = adk_memory::GraphMemoryService::new("sqlite::memory:").await.unwrap();
    service.migrate().await.unwrap();
    round_trip(&service).await;
}
