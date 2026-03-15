use adk_session::{
    CreateRequest, Event, EventActions, GetRequest, ListRequest, PostgresSessionService,
    SessionService,
};
use chrono::{Duration, Utc};
use serde_json::json;
use std::collections::HashMap;

const DATABASE_URL: &str = "postgres://adk:adk_test@localhost:5499/adk_sessions";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    println!("=== ADK PostgreSQL Multi-Session Example ===\n");

    let service = PostgresSessionService::new(DATABASE_URL).await?;
    service.migrate().await?;
    println!("Connected and migrated.\n");

    // --- Scenario 1: Multiple users sharing app-level state ---

    println!("--- Scenario 1: Shared App State Across Users ---\n");

    println!("1. Creating sessions for two users with shared app config...");
    let mut alice_state = HashMap::new();
    alice_state.insert("app:model".to_string(), json!("gpt-4"));
    alice_state.insert("app:max_tokens".to_string(), json!(4096));
    alice_state.insert("user:name".to_string(), json!("Alice"));
    alice_state.insert("user:role".to_string(), json!("admin"));
    alice_state.insert("task".to_string(), json!("code review"));

    let alice = service
        .create(CreateRequest {
            app_name: "shared-app".to_string(),
            user_id: "alice".to_string(),
            session_id: Some("alice-s1".to_string()),
            state: alice_state,
        })
        .await?;
    println!("   Alice session: {}", alice.id());

    let mut bob_state = HashMap::new();
    bob_state.insert("app:model".to_string(), json!("gpt-4"));
    bob_state.insert("app:max_tokens".to_string(), json!(4096));
    bob_state.insert("user:name".to_string(), json!("Bob"));
    bob_state.insert("user:role".to_string(), json!("developer"));
    bob_state.insert("task".to_string(), json!("debugging"));

    let bob = service
        .create(CreateRequest {
            app_name: "shared-app".to_string(),
            user_id: "bob".to_string(),
            session_id: Some("bob-s1".to_string()),
            state: bob_state,
        })
        .await?;
    println!("   Bob session: {}\n", bob.id());

    // Alice updates app-level config — this affects the app_states table
    println!("2. Alice updates app:model to gpt-4o...");
    let mut delta = HashMap::new();
    delta.insert("app:model".to_string(), json!("gpt-4o"));
    let event = make_event("evt-a1", "inv-a1", delta);
    service.append_event("alice-s1", event).await?;

    // Verify Alice sees the updated app state
    let alice_session = service
        .get(GetRequest {
            app_name: "shared-app".to_string(),
            user_id: "alice".to_string(),
            session_id: "alice-s1".to_string(),
            num_recent_events: None,
            after: None,
        })
        .await?;
    let alice_model = alice_session.state().all().get("app:model").cloned();
    println!("   Alice sees app:model = {}", alice_model.unwrap_or(json!("missing")));
    println!();

    // --- Scenario 2: Event filtering with num_recent_events and after ---

    println!("--- Scenario 2: Event Filtering ---\n");

    println!("3. Appending multiple events to Alice's session...");
    let base_time = Utc::now();
    for i in 2..=5 {
        let mut delta = HashMap::new();
        delta.insert("step".to_string(), json!(i));
        let mut event = make_event(&format!("evt-a{i}"), &format!("inv-a{i}"), delta);
        event.timestamp = base_time + Duration::seconds(i as i64);
        service.append_event("alice-s1", event).await?;
    }
    println!("   Appended events evt-a2 through evt-a5.\n");

    println!("4. Fetching with num_recent_events=2...");
    let recent = service
        .get(GetRequest {
            app_name: "shared-app".to_string(),
            user_id: "alice".to_string(),
            session_id: "alice-s1".to_string(),
            num_recent_events: Some(2),
            after: None,
        })
        .await?;
    let events = recent.events().all();
    println!("   Got {} events (requested 2 most recent):", events.len());
    for e in &events {
        println!("     - {} at {}", e.id, e.timestamp);
    }
    println!();

    println!("5. Fetching events after a specific timestamp...");
    let cutoff = base_time + Duration::seconds(3);
    let filtered = service
        .get(GetRequest {
            app_name: "shared-app".to_string(),
            user_id: "alice".to_string(),
            session_id: "alice-s1".to_string(),
            num_recent_events: None,
            after: Some(cutoff),
        })
        .await?;
    let events = filtered.events().all();
    println!("   Events after {cutoff}: {} found", events.len());
    for e in &events {
        println!("     - {} at {}", e.id, e.timestamp);
    }
    println!();

    // --- Scenario 3: Listing and cleanup ---

    println!("--- Scenario 3: Multi-User Session Listing ---\n");

    println!("6. Listing all sessions per user...");
    let alice_sessions = service
        .list(ListRequest {
            app_name: "shared-app".to_string(),
            user_id: "alice".to_string(),
            limit: None,
            offset: None,
        })
        .await?;
    let bob_sessions = service
        .list(ListRequest {
            app_name: "shared-app".to_string(),
            user_id: "bob".to_string(),
            limit: None,
            offset: None,
        })
        .await?;
    println!("   Alice has {} session(s)", alice_sessions.len());
    println!("   Bob has {} session(s)", bob_sessions.len());
    println!();

    // --- Scenario 4: Temp key stripping ---

    println!("--- Scenario 4: Temp Key Stripping ---\n");

    println!("7. Appending event with temp: keys...");
    let mut delta = HashMap::new();
    delta.insert("result".to_string(), json!("success"));
    delta.insert("temp:scratch_pad".to_string(), json!("intermediate data"));
    delta.insert("temp:debug_info".to_string(), json!("verbose logs"));
    let event = make_event("evt-a6", "inv-a6", delta);
    service.append_event("alice-s1", event).await?;

    let session = service
        .get(GetRequest {
            app_name: "shared-app".to_string(),
            user_id: "alice".to_string(),
            session_id: "alice-s1".to_string(),
            num_recent_events: None,
            after: None,
        })
        .await?;
    let state = session.state().all();
    let has_temp = state.keys().any(|k| k.starts_with("temp:"));
    println!("   result = {}", state.get("result").unwrap_or(&json!("missing")));
    println!("   temp keys persisted: {} (expected: false)", has_temp);
    assert!(!has_temp, "temp keys should be stripped");
    println!();

    // Cleanup
    println!("8. Cleaning up...");
    service
        .delete(adk_session::DeleteRequest {
            app_name: "shared-app".to_string(),
            user_id: "alice".to_string(),
            session_id: "alice-s1".to_string(),
        })
        .await?;
    service
        .delete(adk_session::DeleteRequest {
            app_name: "shared-app".to_string(),
            user_id: "bob".to_string(),
            session_id: "bob-s1".to_string(),
        })
        .await?;
    println!("   All sessions deleted.");

    println!("\n=== Multi-session example completed successfully ===");
    Ok(())
}

fn make_event(
    id: &str,
    invocation_id: &str,
    state_delta: HashMap<String, serde_json::Value>,
) -> Event {
    Event {
        id: id.to_string(),
        timestamp: Utc::now(),
        invocation_id: invocation_id.to_string(),
        branch: "main".to_string(),
        author: "user".to_string(),
        llm_request: None,
        llm_response: Default::default(),
        actions: EventActions { state_delta, ..Default::default() },
        long_running_tool_ids: vec![],
        provider_metadata: HashMap::new(),
    }
}
