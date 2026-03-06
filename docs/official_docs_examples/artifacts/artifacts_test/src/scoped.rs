//! ScopedArtifacts Example
//!
//! Demonstrates session isolation and user-scoped artifacts with user: prefix.
//!
//! Run:
//!   cd doc-test/artifacts/artifacts_test
//!   cargo run --bin scoped

use adk_artifact::{InMemoryArtifactService, ScopedArtifacts};
use adk_core::types::{SessionId, UserId};
use adk_core::{Artifacts, Part};
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("ScopedArtifacts Example");
    println!("=======================\n");

    let service = Arc::new(InMemoryArtifactService::new());

    // Create two sessions for the same user
    let session1 = ScopedArtifacts::new(
        service.clone(),
        "my_app".to_string(),
        UserId::new("user_123").unwrap(),
        SessionId::new("session_1").unwrap(),
    );

    let session2 = ScopedArtifacts::new(
        service.clone(),
        "my_app".to_string(),
        UserId::new("user_123").unwrap(),
        SessionId::new("session_2").unwrap(),
    );

    // --- Session-scoped artifacts (default) ---
    println!("1. Session-scoped artifacts (isolated):");

    // Save in session 1
    session1.save("notes.txt", &Part::Text("Session 1 notes".to_string() )).await?;
    println!("   Session 1: Saved notes.txt");

    // Save in session 2
    session2.save("notes.txt", &Part::Text("Session 2 notes".to_string() )).await?;
    println!("   Session 2: Saved notes.txt");

    // Load from each - they're isolated
    let s1_notes = session1.load("notes.txt").await?;
    let s2_notes = session2.load("notes.txt").await?;

    if let (Part::Text(t1 ), Part::Text(t2 )) = (s1_notes, s2_notes) {
        println!("   Session 1 loaded: {}", t1);
        println!("   Session 2 loaded: {}", t2);
    }

    // List shows only session-specific files
    let s1_files = session1.list().await?;
    let s2_files = session2.list().await?;
    println!("   Session 1 files: {:?}", s1_files);
    println!("   Session 2 files: {:?}", s2_files);

    // --- User-scoped artifacts (shared with user: prefix) ---
    println!("\n2. User-scoped artifacts (shared across sessions):");

    // Save user-scoped artifact from session 1
    session1
        .save("user:profile.json", &Part::Text(r#"{"name": "Alice"}"#.to_string()))
        .await?;
    println!("   Session 1: Saved user:profile.json");

    // Load from session 2 - same artifact!
    let profile = session2.load("user:profile.json").await?;
    if let Some(text) = profile.as_text() {
        println!("   Session 2 loaded: {}", text);
    }

    // Both sessions see user-scoped files in their list
    let s1_files = session1.list().await?;
    let s2_files = session2.list().await?;
    println!("   Session 1 files: {:?}", s1_files);
    println!("   Session 2 files: {:?}", s2_files);

    // --- Simple API demonstration ---
    println!("\n3. Simple API (no app/user/session in each call):");

    let artifacts = ScopedArtifacts::new(
        service.clone(),
        "demo_app".to_string(),
        UserId::new("demo_user").unwrap(),
        SessionId::new("demo_session").unwrap(),
    );

    // Save - just name and data
    let version = artifacts
        .save(
            "report.pdf",
            &Part::InlineData {
                mime_type: "application/pdf".parse().unwrap(),
                data: vec![0x25, 0x50, 0x44, 0x46].into(), // PDF header
            },
        )
        .await?;
    println!("   Saved report.pdf as version {}", version);

    // Load - just name
    let part = artifacts.load("report.pdf").await?;
    if let Part::InlineData { mime_type, data } = part {
        println!("   Loaded {} ({} bytes)", mime_type, data.len());
    }

    // List - no parameters
    let files = artifacts.list().await?;
    println!("   Files: {:?}", files);

    println!("\n✓ All scoped operations completed successfully!");

    Ok(())
}
