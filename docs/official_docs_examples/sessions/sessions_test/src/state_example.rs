//! State Management Example
//!
//! This example matches exactly what's documented in state.md
//!
//! Run:
//!   cd doc-test/sessions/sessions_test
//!   cargo run --bin state_example

use adk_core::types::{SessionId, UserId};
use adk_session::{
    CreateRequest, InMemorySessionService, KEY_PREFIX_APP, KEY_PREFIX_USER, SessionService,
};
use serde_json::json;
use std::collections::HashMap;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let service = InMemorySessionService::new();

    // Create first session with initial state
    let mut state1 = HashMap::new();
    state1.insert(format!("{}theme", KEY_PREFIX_APP), json!("dark"));
    state1.insert(format!("{}language", KEY_PREFIX_USER), json!("en"));
    state1.insert("context".to_string(), json!("session1"));

    let _session1 = service
        .create(CreateRequest {
            app_name: "my_app".to_string(),
            user_id: UserId::new("alice").unwrap(),
            session_id: SessionId::new("s1").ok(),
            state: state1,
        })
        .await?;

    // Create second session for same user
    let mut state2 = HashMap::new();
    state2.insert("context".to_string(), json!("session2"));

    let session2 = service
        .create(CreateRequest {
            app_name: "my_app".to_string(),
            user_id: UserId::new("alice").unwrap(),
            session_id: SessionId::new("s2").ok(),
            state: state2,
        })
        .await?;

    // Session 2 inherits app and user state
    let s2_state = session2.state();

    // App state is shared
    assert_eq!(s2_state.get("app:theme"), Some(json!("dark")));

    // User state is shared
    assert_eq!(s2_state.get("user:language"), Some(json!("en")));

    // Session state is separate
    assert_eq!(s2_state.get("context"), Some(json!("session2")));

    println!("State scoping works correctly!");
    Ok(())
}
