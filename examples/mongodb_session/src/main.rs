use adk_session::{
    CreateRequest, DeleteRequest, Event, EventActions, GetRequest, ListRequest,
    MongoSessionService, SessionService,
};
use chrono::Utc;
use serde_json::json;
use std::collections::HashMap;

const MONGODB_URL: &str = "mongodb://localhost:27099/?directConnection=true";
const DATABASE_NAME: &str = "adk_sessions";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    println!("=== ADK MongoDB Session Example ===\n");

    // 1. Connect and migrate
    println!("1. Connecting to MongoDB...");
    let service = MongoSessionService::new(MONGODB_URL, DATABASE_NAME).await?;
    println!("   Connected.");

    println!("   Running migrations...");
    service.migrate().await?;
    println!("   Migrations complete.\n");

    // 2. Create a session with three-tier state
    println!("2. Creating session with three-tier state...");
    let mut initial_state = HashMap::new();
    initial_state.insert("app:theme".to_string(), json!("dark"));
    initial_state.insert("app:version".to_string(), json!("2.0"));
    initial_state.insert("user:name".to_string(), json!("Alice"));
    initial_state.insert("user:lang".to_string(), json!("en"));
    initial_state.insert("counter".to_string(), json!(0));
    initial_state.insert("temp:scratch".to_string(), json!("ephemeral"));

    let session = service
        .create(CreateRequest {
            app_name: "demo-app".to_string(),
            user_id: "alice".to_string(),
            session_id: Some("session-001".to_string()),
            state: initial_state,
        })
        .await?;

    println!("   Session created: {}", session.id());
    println!("   State keys: {:?}", session.state().all().keys().collect::<Vec<_>>());
    println!("   Note: temp:scratch was stripped.\n");

    // 3. Retrieve the session
    println!("3. Retrieving session...");
    let fetched = service
        .get(GetRequest {
            app_name: "demo-app".to_string(),
            user_id: "alice".to_string(),
            session_id: "session-001".to_string(),
            num_recent_events: None,
            after: None,
        })
        .await?;

    let state = fetched.state().all();
    println!("   Merged state ({} keys):", state.len());
    for (k, v) in &state {
        println!("     {k}: {v}");
    }
    assert!(!state.contains_key("temp:scratch"), "temp keys should not be persisted");
    println!();

    // 4. Append an event with state delta
    println!("4. Appending event with state updates...");
    let mut state_delta = HashMap::new();
    state_delta.insert("counter".to_string(), json!(1));
    state_delta.insert("app:version".to_string(), json!("2.1"));
    state_delta.insert("user:lang".to_string(), json!("fr"));
    state_delta.insert("temp:debug".to_string(), json!("will be stripped"));

    let event = Event {
        id: "evt-001".to_string(),
        timestamp: Utc::now(),
        invocation_id: "inv-001".to_string(),
        branch: "main".to_string(),
        author: "user".to_string(),
        llm_request: None,
        llm_response: Default::default(),
        actions: EventActions { state_delta, ..Default::default() },
        long_running_tool_ids: vec![],
        provider_metadata: HashMap::new(),
    };

    service.append_event("session-001", event).await?;
    println!("   Event appended.\n");

    // 5. Verify updated state
    println!("5. Verifying updated state...");
    let updated = service
        .get(GetRequest {
            app_name: "demo-app".to_string(),
            user_id: "alice".to_string(),
            session_id: "session-001".to_string(),
            num_recent_events: None,
            after: None,
        })
        .await?;

    let updated_state = updated.state().all();
    println!("   Updated state ({} keys):", updated_state.len());
    for (k, v) in &updated_state {
        println!("     {k}: {v}");
    }
    assert_eq!(updated_state.get("counter"), Some(&json!(1)));
    assert_eq!(updated_state.get("app:version"), Some(&json!("2.1")));
    assert_eq!(updated_state.get("user:lang"), Some(&json!("fr")));
    assert!(!updated_state.contains_key("temp:debug"));

    let events = updated.events().all();
    println!("   Events: {}", events.len());
    assert_eq!(events.len(), 1);
    println!();

    // 6. List sessions
    println!("6. Listing sessions for alice...");
    let sessions = service
        .list(ListRequest {
            app_name: "demo-app".to_string(),
            user_id: "alice".to_string(),
            limit: None,
            offset: None,
        })
        .await?;
    println!("   Found {} session(s)", sessions.len());
    for s in &sessions {
        println!("     - {} (app: {}, user: {})", s.id(), s.app_name(), s.user_id());
    }
    println!();

    // 7. Delete session
    println!("7. Deleting session...");
    service
        .delete(DeleteRequest {
            app_name: "demo-app".to_string(),
            user_id: "alice".to_string(),
            session_id: "session-001".to_string(),
        })
        .await?;
    println!("   Deleted.\n");

    // 8. Verify deletion
    println!("8. Verifying deletion...");
    let remaining = service
        .list(ListRequest {
            app_name: "demo-app".to_string(),
            user_id: "alice".to_string(),
            limit: None,
            offset: None,
        })
        .await?;
    println!("   Sessions remaining: {}", remaining.len());
    assert!(remaining.is_empty(), "session should be deleted");

    println!("\n=== All operations completed successfully ===");
    Ok(())
}
