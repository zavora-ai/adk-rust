//! Sessions Basic Example
//!
//! Demonstrates basic session operations: create, get, list, delete.
//!
//! Run:
//!   cd doc-test/sessions/sessions_test
//!   cargo run --bin basic

use adk_session::{
    CreateRequest, DeleteRequest, Event, GetRequest, InMemorySessionService, ListRequest,
    SessionService, KEY_PREFIX_USER,
};
use serde_json::json;
use std::collections::HashMap;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("Sessions Basic Example");
    println!("======================\n");

    let service = InMemorySessionService::new();

    // 1. Create session with initial state
    println!("1. Creating session...");
    let mut initial_state = HashMap::new();
    initial_state.insert(format!("{}name", KEY_PREFIX_USER), json!("Alice"));
    initial_state.insert("topic".to_string(), json!("Getting started"));

    let session = service
        .create(CreateRequest {
            app_name: "demo".to_string(),
            user_id: "alice".to_string(),
            session_id: None, // Auto-generate
            state: initial_state,
        })
        .await?;

    println!("   Session ID: {}", session.id());
    println!("   App: {}", session.app_name());
    println!("   User: {}", session.user_id());

    // 2. Check state
    println!("\n2. Checking state...");
    let state = session.state();
    println!("   user:name = {:?}", state.get("user:name"));
    println!("   topic = {:?}", state.get("topic"));

    // 3. Append an event
    println!("\n3. Appending event...");
    let event = Event::new("inv_001");
    service.append_event(session.id(), event).await?;
    println!("   Event appended");

    // 4. Retrieve session with events
    println!("\n4. Retrieving session...");
    let session = service
        .get(GetRequest {
            app_name: "demo".to_string(),
            user_id: "alice".to_string(),
            session_id: session.id().to_string(),
            num_recent_events: None,
            after: None,
        })
        .await?;

    println!("   Events count: {}", session.events().len());
    println!("   Last updated: {}", session.last_update_time());

    // 5. List all sessions
    println!("\n5. Listing sessions...");
    let sessions = service
        .list(ListRequest {
            app_name: "demo".to_string(),
            user_id: "alice".to_string(),
        })
        .await?;

    println!("   Total sessions: {}", sessions.len());
    for s in &sessions {
        println!("   - {} (events: {})", s.id(), s.events().len());
    }

    // 6. Delete session
    println!("\n6. Deleting session...");
    let session_id = session.id().to_string();
    service
        .delete(DeleteRequest {
            app_name: "demo".to_string(),
            user_id: "alice".to_string(),
            session_id,
        })
        .await?;
    println!("   Session deleted");

    // Verify deletion
    let sessions = service
        .list(ListRequest {
            app_name: "demo".to_string(),
            user_id: "alice".to_string(),
        })
        .await?;
    println!("   Remaining sessions: {}", sessions.len());

    println!("\n✓ All session operations completed successfully!");

    Ok(())
}
