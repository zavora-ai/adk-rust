//! Typed Identity Append Event doc-test — validates typed session identity usage.
//!
//! Demonstrates using `AdkIdentity` with `append_event_for_identity()` for
//! unambiguous, multi-tenant-safe event appending.
//!
//! **Validates: Requirements 10.2, 10.4**

use adk_core::{AdkIdentity, AppName, SessionId, UserId};
use adk_session::{
    AppendEventRequest, CreateRequest, Event, GetRequest, InMemorySessionService, SessionService,
};
use std::collections::HashMap;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("=== Typed Identity Append Event Doc-Test ===\n");

    let service = InMemorySessionService::new();

    // -----------------------------------------------------------------------
    // 1. Create a session using the standard API
    // -----------------------------------------------------------------------
    println!("1. Creating session...");
    let session = service
        .create(CreateRequest {
            app_name: "weather-app".to_string(),
            user_id: "alice".to_string(),
            session_id: None,
            state: HashMap::new(),
        })
        .await?;

    let session_id_str = session.id().to_string();
    println!("   Session created: {session_id_str}");

    // -----------------------------------------------------------------------
    // 2. Construct AdkIdentity from typed identifiers
    // -----------------------------------------------------------------------
    println!("\n2. Constructing AdkIdentity...");
    let identity = AdkIdentity::new(
        AppName::try_from("weather-app")?,
        UserId::try_from("alice")?,
        SessionId::try_from(session_id_str.as_str())?,
    );
    println!("   {identity}");

    // -----------------------------------------------------------------------
    // 3. Append event using typed identity
    // -----------------------------------------------------------------------
    println!("\n3. Appending event with typed identity...");
    let event = Event::new("inv-001");
    service
        .append_event_for_identity(AppendEventRequest { identity: identity.clone(), event })
        .await?;
    println!("   ✓ Event appended via append_event_for_identity()");

    // Append a second event
    let event2 = Event::new("inv-002");
    service
        .append_event_for_identity(AppendEventRequest { identity: identity.clone(), event: event2 })
        .await?;
    println!("   ✓ Second event appended");

    // -----------------------------------------------------------------------
    // 4. Verify events were stored correctly
    // -----------------------------------------------------------------------
    println!("\n4. Verifying stored events...");
    let session = service
        .get(GetRequest {
            app_name: "weather-app".to_string(),
            user_id: "alice".to_string(),
            session_id: session_id_str.clone(),
            num_recent_events: None,
            after: None,
        })
        .await?;

    assert_eq!(session.events().len(), 2);
    println!("   ✓ Session has {} events", session.events().len());

    // -----------------------------------------------------------------------
    // 5. Multi-tenant isolation — same session_id, different app/user
    // -----------------------------------------------------------------------
    println!("\n5. Multi-tenant isolation...");

    // Create a session for a different user with the same session_id
    let shared_sid = "shared-session-42";
    service
        .create(CreateRequest {
            app_name: "weather-app".to_string(),
            user_id: "alice".to_string(),
            session_id: Some(shared_sid.to_string()),
            state: HashMap::new(),
        })
        .await?;

    service
        .create(CreateRequest {
            app_name: "weather-app".to_string(),
            user_id: "bob".to_string(),
            session_id: Some(shared_sid.to_string()),
            state: HashMap::new(),
        })
        .await?;

    // Append event only to alice's session using typed identity
    let alice_identity = AdkIdentity::new(
        AppName::try_from("weather-app")?,
        UserId::try_from("alice")?,
        SessionId::try_from(shared_sid)?,
    );
    service
        .append_event_for_identity(AppendEventRequest {
            identity: alice_identity,
            event: Event::new("inv-alice"),
        })
        .await?;

    // Verify alice has 1 event, bob has 0
    let alice_session = service
        .get(GetRequest {
            app_name: "weather-app".to_string(),
            user_id: "alice".to_string(),
            session_id: shared_sid.to_string(),
            num_recent_events: None,
            after: None,
        })
        .await?;
    assert_eq!(alice_session.events().len(), 1);

    let bob_session = service
        .get(GetRequest {
            app_name: "weather-app".to_string(),
            user_id: "bob".to_string(),
            session_id: shared_sid.to_string(),
            num_recent_events: None,
            after: None,
        })
        .await?;
    assert_eq!(bob_session.events().len(), 0);

    println!("   ✓ Alice's session has 1 event, Bob's has 0 — isolation confirmed");

    println!("\n=== All typed identity append event tests passed! ===");
    Ok(())
}
