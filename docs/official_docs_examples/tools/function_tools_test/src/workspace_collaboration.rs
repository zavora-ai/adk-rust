//! Workspace collaboration example — multi-agent project building
//!
//! Demonstrates the collaborative workspace model where specialist agents
//! coordinate through typed collaboration events on a shared Workspace.
//!
//! Run: cargo run --bin workspace_collaboration

use adk_code::{
    CollaborationEvent, CollaborationEventKind, Workspace,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== Workspace Collaboration Doc-Test ===\n");

    // 1. Create a shared workspace via builder
    let temp_dir = std::env::temp_dir().join("adk_workspace_test");
    std::fs::create_dir_all(&temp_dir)?;

    let workspace = Workspace::new(&temp_dir)
        .project_name("demo-site")
        .session_id("session-001")
        .build();
    assert_eq!(workspace.root(), &temp_dir);
    println!("✓ Workspace created at {}", temp_dir.display());

    // 2. Verify workspace metadata
    let metadata = workspace.metadata();
    assert_eq!(metadata.project_name, "demo-site");
    assert_eq!(metadata.session_id.as_deref(), Some("session-001"));
    println!("✓ WorkspaceMetadata with project name and session ID");

    // 3. Create typed collaboration events using the builder API
    let need_work = CollaborationEvent::new(
        "req-001",
        "backend-api",
        "frontend_engineer",
        CollaborationEventKind::NeedWork,
    )
    .consumer("backend_engineer")
    .payload(serde_json::json!({
        "description": "Need REST API for user authentication",
        "endpoints": ["/api/login", "/api/register"]
    }));
    assert_eq!(need_work.correlation_id, "req-001");
    assert!(matches!(need_work.kind, CollaborationEventKind::NeedWork));
    println!("✓ NeedWork collaboration event created");

    // 4. Simulate work publication
    let work_published = CollaborationEvent::new(
        "req-001",
        "backend-api",
        "backend_engineer",
        CollaborationEventKind::WorkPublished,
    )
    .consumer("frontend_engineer")
    .payload(serde_json::json!({
        "files": ["src/routes/auth.rs"],
        "status": "complete"
    }));
    assert_eq!(work_published.correlation_id, "req-001");
    assert!(matches!(
        work_published.kind,
        CollaborationEventKind::WorkPublished
    ));
    println!("✓ WorkPublished event with matching correlation ID");

    // 5. Verify all collaboration event kinds
    let kinds = vec![
        CollaborationEventKind::NeedWork,
        CollaborationEventKind::WorkClaimed,
        CollaborationEventKind::WorkPublished,
        CollaborationEventKind::FeedbackRequested,
        CollaborationEventKind::FeedbackProvided,
        CollaborationEventKind::Blocked,
        CollaborationEventKind::Completed,
    ];
    assert_eq!(kinds.len(), 7);
    println!("✓ All 7 collaboration event kinds available");

    // 6. Verify publish on workspace
    workspace.publish(CollaborationEvent::new(
        "test-001",
        "test",
        "agent_a",
        CollaborationEventKind::NeedWork,
    ));
    println!("✓ Workspace publish works");

    // Cleanup
    std::fs::remove_dir_all(&temp_dir).ok();

    println!("\n=== All workspace collaboration tests passed! ===");
    Ok(())
}
