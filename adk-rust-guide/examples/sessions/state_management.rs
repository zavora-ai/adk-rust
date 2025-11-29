//! Validates: docs/official_docs/sessions/state.md
//!
//! This example demonstrates session state management with prefixes.
//!
//! Run modes:
//!   cargo run --example state_management -p adk-rust-guide              # Validation mode
//!   cargo run --example state_management -p adk-rust-guide -- chat      # Interactive console

use adk_rust::prelude::*;
use adk_rust::session::{
    CreateRequest, GetRequest, SessionService,
    KEY_PREFIX_APP, KEY_PREFIX_USER, KEY_PREFIX_TEMP,
};
use adk_rust_guide::{print_success, print_validating};
use serde_json::json;
use std::collections::HashMap;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    print_validating("sessions/state.md");

    let session_service = InMemorySessionService::new();

    // =========================================================================
    // 1. State Key Prefixes
    // =========================================================================
    println!("\n--- State Key Prefixes ---");
    println!("App prefix:  '{}' - Shared across all users", KEY_PREFIX_APP);
    println!("User prefix: '{}' - Shared across user's sessions", KEY_PREFIX_USER);
    println!("Temp prefix: '{}' - Cleared after each invocation", KEY_PREFIX_TEMP);
    println!("No prefix:   Session-scoped (default)");

    // =========================================================================
    // 2. Create session with initial state using different scopes
    // =========================================================================
    println!("\n--- Creating Session with Multi-Scope State ---");

    let mut initial_state = HashMap::new();
    
    // App-scoped state (shared across all users)
    initial_state.insert(
        format!("{}version", KEY_PREFIX_APP),
        json!("1.0.0")
    );
    initial_state.insert(
        format!("{}theme", KEY_PREFIX_APP),
        json!("dark")
    );
    
    // User-scoped state (shared across user's sessions)
    initial_state.insert(
        format!("{}name", KEY_PREFIX_USER),
        json!("Alice")
    );
    initial_state.insert(
        format!("{}language", KEY_PREFIX_USER),
        json!("en")
    );
    
    // Session-scoped state (no prefix)
    initial_state.insert(
        "topic".to_string(),
        json!("Getting started with ADK")
    );
    initial_state.insert(
        "turn_count".to_string(),
        json!(0)
    );

    let session1 = session_service.create(CreateRequest {
        app_name: "my_app".to_string(),
        user_id: "alice".to_string(),
        session_id: Some("session1".to_string()),
        state: initial_state,
    }).await?;

    println!("Created session: {}", session1.id());

    // =========================================================================
    // 3. Read state from session
    // =========================================================================
    println!("\n--- Reading State ---");

    let state = session1.state();
    
    // Read app-scoped state
    if let Some(version) = state.get("app:version") {
        println!("App version: {}", version);
    }
    
    // Read user-scoped state
    if let Some(name) = state.get("user:name") {
        println!("User name: {}", name);
    }
    
    // Read session-scoped state
    if let Some(topic) = state.get("topic") {
        println!("Topic: {}", topic);
    }

    // Get all state
    println!("\nAll state entries:");
    for (key, value) in state.all() {
        println!("  {}: {}", key, value);
    }

    // =========================================================================
    // 4. Demonstrate state scoping across sessions
    // =========================================================================
    println!("\n--- State Scoping Across Sessions ---");

    // Create a second session for the same user
    let mut session2_state = HashMap::new();
    session2_state.insert("topic".to_string(), json!("Advanced topics"));

    let session2 = session_service.create(CreateRequest {
        app_name: "my_app".to_string(),
        user_id: "alice".to_string(),
        session_id: Some("session2".to_string()),
        state: session2_state,
    }).await?;

    println!("Created second session: {}", session2.id());

    // Session 2 should inherit app and user state
    let s2_state = session2.state();
    
    println!("\nSession 2 state (inherits app and user scopes):");
    
    // App state is shared
    let app_version = s2_state.get("app:version");
    println!("  app:version = {:?} (inherited)", app_version);
    
    // User state is shared
    let user_name = s2_state.get("user:name");
    println!("  user:name = {:?} (inherited)", user_name);
    
    // Session state is separate
    let topic = s2_state.get("topic");
    println!("  topic = {:?} (session-specific)", topic);

    // =========================================================================
    // 5. Demonstrate state isolation between users
    // =========================================================================
    println!("\n--- State Isolation Between Users ---");

    // Create a session for a different user
    let mut bob_state = HashMap::new();
    bob_state.insert(
        format!("{}name", KEY_PREFIX_USER),
        json!("Bob")
    );
    bob_state.insert("topic".to_string(), json!("Bob's topic"));

    let bob_session = session_service.create(CreateRequest {
        app_name: "my_app".to_string(),
        user_id: "bob".to_string(),
        session_id: Some("bob_session".to_string()),
        state: bob_state,
    }).await?;

    let bob_state = bob_session.state();
    
    println!("Bob's session state:");
    
    // App state is shared across users
    let app_version = bob_state.get("app:version");
    println!("  app:version = {:?} (shared with Alice)", app_version);
    
    // User state is isolated
    let user_name = bob_state.get("user:name");
    println!("  user:name = {:?} (Bob's own)", user_name);
    
    // Session state is isolated
    let topic = bob_state.get("topic");
    println!("  topic = {:?} (Bob's session)", topic);

    // =========================================================================
    // 6. Verify state assertions
    // =========================================================================
    println!("\n--- Verifying State Behavior ---");

    // Retrieve Alice's first session to verify state persistence
    let alice_s1 = session_service.get(GetRequest {
        app_name: "my_app".to_string(),
        user_id: "alice".to_string(),
        session_id: "session1".to_string(),
        num_recent_events: None,
        after: None,
    }).await?;

    let s1_state = alice_s1.state();
    
    // Verify app state is consistent
    assert_eq!(
        s1_state.get("app:version"),
        Some(json!("1.0.0")),
        "App state should be preserved"
    );
    println!("✓ App state preserved correctly");

    // Verify user state is consistent
    assert_eq!(
        s1_state.get("user:name"),
        Some(json!("Alice")),
        "User state should be preserved"
    );
    println!("✓ User state preserved correctly");

    // Verify session state is isolated
    assert_eq!(
        s1_state.get("topic"),
        Some(json!("Getting started with ADK")),
        "Session state should be isolated"
    );
    println!("✓ Session state isolated correctly");

    print_success("state_management");

    println!("\nTip: Run with 'chat' for interactive mode:");
    println!("  cargo run --example state_management -p adk-rust-guide -- chat");

    Ok(())
}
