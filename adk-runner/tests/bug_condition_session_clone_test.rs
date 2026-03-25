//! Bug Condition Exploration Test — Session Eager Clone
//!
//! **Property 1 (Bug 1): MutableSession events_len avoids full clone**
//!
//! Asserts that `MutableSession::events_len()` exists and returns the correct
//! event count in O(1) without cloning the full `Vec<Event>`.
//!
//! On unfixed code, `events_len()` does not exist, so this test fails to
//! compile. On fixed code, it compiles and passes.
//!
//! **Validates: Requirements 1.1, 1.2**

use std::collections::HashMap;
use std::sync::Arc;

use adk_runner::MutableSession;
use adk_session::{Event, Events, Session, State};
use proptest::prelude::*;

// ---------------------------------------------------------------------------
// Mock types
// ---------------------------------------------------------------------------

struct MockEvents {
    events: Vec<Event>,
}

impl Events for MockEvents {
    fn all(&self) -> Vec<Event> {
        self.events.clone()
    }

    fn len(&self) -> usize {
        self.events.len()
    }

    fn at(&self, index: usize) -> Option<&Event> {
        self.events.get(index)
    }
}

struct MockState {
    data: HashMap<String, serde_json::Value>,
}

impl adk_session::ReadonlyState for MockState {
    fn get(&self, key: &str) -> Option<serde_json::Value> {
        self.data.get(key).cloned()
    }

    fn all(&self) -> HashMap<String, serde_json::Value> {
        self.data.clone()
    }
}

impl State for MockState {
    fn get(&self, key: &str) -> Option<serde_json::Value> {
        self.data.get(key).cloned()
    }

    fn set(&mut self, key: String, value: serde_json::Value) {
        self.data.insert(key, value);
    }

    fn all(&self) -> HashMap<String, serde_json::Value> {
        self.data.clone()
    }
}

struct MockSession {
    events: MockEvents,
    state: MockState,
}

impl Session for MockSession {
    fn id(&self) -> &str {
        "session-1"
    }

    fn app_name(&self) -> &str {
        "test-app"
    }

    fn user_id(&self) -> &str {
        "user-1"
    }

    fn state(&self) -> &dyn State {
        &self.state
    }

    fn events(&self) -> &dyn Events {
        &self.events
    }

    fn last_update_time(&self) -> chrono::DateTime<chrono::Utc> {
        chrono::Utc::now()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_event(author: &str) -> Event {
    let mut e = Event::new("inv-test");
    e.author = author.to_string();
    e
}

fn make_session(event_count: usize, state_count: usize) -> Arc<dyn Session> {
    let events: Vec<Event> = (0..event_count).map(|i| make_event(&format!("agent-{i}"))).collect();
    let mut data = HashMap::new();
    for i in 0..state_count {
        data.insert(format!("key-{i}"), serde_json::json!(i));
    }
    Arc::new(MockSession { events: MockEvents { events }, state: MockState { data } })
}

// ---------------------------------------------------------------------------
// Property test
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: production-hardening, Property 1: Bug Condition — Session events_len**
    ///
    /// *For any* MutableSession constructed from a session with M > 0 events,
    /// `events_len()` SHALL exist and return the correct count without cloning
    /// the full event vector.
    ///
    /// **Validates: Requirements 1.1, 1.2**
    #[test]
    fn prop_events_len_exists_and_returns_correct_count(
        event_count in 1usize..50,
        state_count in 0usize..20,
    ) {
        let session = make_session(event_count, state_count);
        let mutable = MutableSession::new(session);

        // events_len() must exist (compile-time check) and return the correct count
        let len = mutable.events_len();
        prop_assert_eq!(len, event_count, "events_len() returned wrong count");
    }
}

/// Verify events_len returns 0 for an empty session.
#[test]
fn events_len_zero_for_empty_session() {
    let session = make_session(0, 0);
    let mutable = MutableSession::new(session);
    assert_eq!(mutable.events_len(), 0);
}

/// Verify events_len tracks appended events.
#[test]
fn events_len_tracks_appended_events() {
    let session = make_session(3, 0);
    let mutable = MutableSession::new(session);
    assert_eq!(mutable.events_len(), 3);

    mutable.append_event(make_event("new-agent"));
    assert_eq!(mutable.events_len(), 4);
}
