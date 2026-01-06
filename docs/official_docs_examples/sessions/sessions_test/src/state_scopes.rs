//! State Scopes Example
//!
//! Demonstrates state scoping with app:, user:, and session prefixes.
//!
//! Run:
//!   cd doc-test/sessions/sessions_test
//!   cargo run --bin state_scopes

use adk_session::{
    CreateRequest, GetRequest, InMemorySessionService, SessionService, KEY_PREFIX_APP,
    KEY_PREFIX_USER,
};
use serde_json::json;
use std::collections::HashMap;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("State Scopes Example");
    println!("====================\n");

    let service = InMemorySessionService::new();

    // 1. Create first session with app and user state
    println!("1. Creating session 1 for Alice...");
    let mut state1 = HashMap::new();
    state1.insert(format!("{}theme", KEY_PREFIX_APP), json!("dark"));
    state1.insert(format!("{}language", KEY_PREFIX_USER), json!("en"));
    state1.insert("context".to_string(), json!("session1_context"));

    let session1 = service
        .create(CreateRequest {
            app_name: "my_app".to_string(),
            user_id: "alice".to_string(),
            session_id: Some("s1".to_string()),
            state: state1,
        })
        .await?;

    println!("   Session 1 state:");
    println!("   - app:theme = {:?}", session1.state().get("app:theme"));
    println!(
        "   - user:language = {:?}",
        session1.state().get("user:language")
    );
    println!("   - context = {:?}", session1.state().get("context"));

    // 2. Create second session for same user (inherits app and user state)
    println!("\n2. Creating session 2 for Alice...");
    let mut state2 = HashMap::new();
    state2.insert("context".to_string(), json!("session2_context"));

    let session2 = service
        .create(CreateRequest {
            app_name: "my_app".to_string(),
            user_id: "alice".to_string(),
            session_id: Some("s2".to_string()),
            state: state2,
        })
        .await?;

    println!("   Session 2 state (inherits app/user state):");
    println!("   - app:theme = {:?}", session2.state().get("app:theme"));
    println!(
        "   - user:language = {:?}",
        session2.state().get("user:language")
    );
    println!("   - context = {:?}", session2.state().get("context"));

    // 3. Create session for different user (inherits only app state)
    println!("\n3. Creating session for Bob...");
    let mut state3 = HashMap::new();
    state3.insert(format!("{}language", KEY_PREFIX_USER), json!("fr"));
    state3.insert("context".to_string(), json!("bob_context"));

    let session3 = service
        .create(CreateRequest {
            app_name: "my_app".to_string(),
            user_id: "bob".to_string(),
            session_id: Some("s3".to_string()),
            state: state3,
        })
        .await?;

    println!("   Bob's session state (inherits only app state):");
    println!("   - app:theme = {:?}", session3.state().get("app:theme"));
    println!(
        "   - user:language = {:?}",
        session3.state().get("user:language")
    );
    println!("   - context = {:?}", session3.state().get("context"));

    // 4. Verify state isolation
    println!("\n4. Verifying state isolation...");

    // Re-fetch session 1 to verify it still has its own context
    let session1_refetch = service
        .get(GetRequest {
            app_name: "my_app".to_string(),
            user_id: "alice".to_string(),
            session_id: "s1".to_string(),
            num_recent_events: None,
            after: None,
        })
        .await?;

    assert_eq!(
        session1_refetch.state().get("context"),
        Some(json!("session1_context"))
    );
    assert_eq!(
        session2.state().get("context"),
        Some(json!("session2_context"))
    );
    assert_eq!(
        session3.state().get("context"),
        Some(json!("bob_context"))
    );

    println!("   ✓ Session contexts are isolated");

    // App state is shared
    assert_eq!(
        session1_refetch.state().get("app:theme"),
        Some(json!("dark"))
    );
    assert_eq!(session2.state().get("app:theme"), Some(json!("dark")));
    assert_eq!(session3.state().get("app:theme"), Some(json!("dark")));

    println!("   ✓ App state is shared across all sessions");

    // User state is shared per user
    assert_eq!(
        session1_refetch.state().get("user:language"),
        Some(json!("en"))
    );
    assert_eq!(session2.state().get("user:language"), Some(json!("en")));
    assert_eq!(session3.state().get("user:language"), Some(json!("fr")));

    println!("   ✓ User state is shared per user");

    println!("\n✓ State scoping works correctly!");

    Ok(())
}
