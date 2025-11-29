//! Validates: docs/official_docs/sessions/sessions.md
//!
//! This example demonstrates basic session creation and management.
//!
//! Run modes:
//!   cargo run --example session_basic -p adk-rust-guide              # Validation mode
//!   cargo run --example session_basic -p adk-rust-guide -- chat      # Interactive console

use adk_rust::prelude::*;
use adk_rust::session::{CreateRequest, GetRequest, ListRequest, DeleteRequest, SessionService};
use adk_rust_guide::{print_success, print_validating};
use std::collections::HashMap;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    print_validating("sessions/sessions.md");

    // Create an in-memory session service
    let session_service = InMemorySessionService::new();

    // =========================================================================
    // 1. Create a session
    // =========================================================================
    println!("\n--- Creating Session ---");
    
    let session = session_service.create(CreateRequest {
        app_name: "my_app".to_string(),
        user_id: "user_123".to_string(),
        session_id: None,  // Auto-generate UUID
        state: HashMap::new(),
    }).await?;

    let session_id = session.id().to_string();
    println!("Created session: {}", session_id);
    println!("App name: {}", session.app_name());
    println!("User ID: {}", session.user_id());
    println!("Last updated: {}", session.last_update_time());

    // =========================================================================
    // 2. Retrieve the session
    // =========================================================================
    println!("\n--- Retrieving Session ---");
    
    let retrieved = session_service.get(GetRequest {
        app_name: "my_app".to_string(),
        user_id: "user_123".to_string(),
        session_id: session_id.clone(),
        num_recent_events: None,
        after: None,
    }).await?;

    println!("Retrieved session: {}", retrieved.id());
    println!("Events count: {}", retrieved.events().len());

    // =========================================================================
    // 3. Create another session and list all
    // =========================================================================
    println!("\n--- Creating Second Session ---");
    
    let session2 = session_service.create(CreateRequest {
        app_name: "my_app".to_string(),
        user_id: "user_123".to_string(),
        session_id: Some("custom-session-id".to_string()),
        state: HashMap::new(),
    }).await?;

    println!("Created session with custom ID: {}", session2.id());

    // List all sessions for the user
    println!("\n--- Listing Sessions ---");
    
    let sessions = session_service.list(ListRequest {
        app_name: "my_app".to_string(),
        user_id: "user_123".to_string(),
    }).await?;

    println!("Found {} sessions for user_123:", sessions.len());
    for s in &sessions {
        println!("  - {} (updated: {})", s.id(), s.last_update_time());
    }

    // =========================================================================
    // 4. Delete a session
    // =========================================================================
    println!("\n--- Deleting Session ---");
    
    session_service.delete(DeleteRequest {
        app_name: "my_app".to_string(),
        user_id: "user_123".to_string(),
        session_id: session_id.clone(),
    }).await?;

    println!("Deleted session: {}", session_id);

    // Verify deletion by listing again
    let remaining = session_service.list(ListRequest {
        app_name: "my_app".to_string(),
        user_id: "user_123".to_string(),
    }).await?;

    println!("Remaining sessions: {}", remaining.len());

    // =========================================================================
    // 5. Access session state and events
    // =========================================================================
    println!("\n--- Accessing State and Events ---");
    
    let session = session_service.get(GetRequest {
        app_name: "my_app".to_string(),
        user_id: "user_123".to_string(),
        session_id: "custom-session-id".to_string(),
        num_recent_events: None,
        after: None,
    }).await?;

    // Access state
    let state = session.state();
    let all_state = state.all();
    println!("State entries: {}", all_state.len());

    // Access events
    let events = session.events();
    println!("Events: {}", events.len());
    println!("Events empty: {}", events.is_empty());

    print_success("session_basic");

    println!("\nTip: Run with 'chat' for interactive mode:");
    println!("  cargo run --example session_basic -p adk-rust-guide -- chat");

    Ok(())
}
