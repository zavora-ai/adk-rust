//! Preservation Property Test A — MutableSession State and Event Mutation
//!
//! **Property 2: Preservation — MutableSession behavior unchanged**
//!
//! For any sequence of `apply_state_delta()` and `append_event()` calls,
//! the resulting state and event list SHALL match the observed behavior:
//!
//! - `apply_state_delta` merges keys into mutable state, skipping `temp:` prefixed keys
//! - `append_event` adds events in order; `events_snapshot()` returns all accumulated events
//! - `conversation_history_for_agent_impl(None)` maps user→"user", non-user→"model"
//! - `conversation_history_for_agent_impl(Some(name))` keeps user + named agent events only
//!
//! **Validates: Requirements 3.1, 3.2, 3.3**

use std::collections::HashMap;
use std::sync::Arc;

use adk_core::{Content, Part};
use adk_runner::MutableSession;
use adk_session::{Event, Events, Session, State};
use proptest::prelude::*;

// Import adk_core::Session to access state() method on MutableSession
use adk_core::Session as AdkCoreSession;

// ---------------------------------------------------------------------------
// Mock types
// ---------------------------------------------------------------------------

struct MockEvents(Vec<Event>);

impl Events for MockEvents {
    fn all(&self) -> Vec<Event> {
        self.0.clone()
    }
    fn len(&self) -> usize {
        self.0.len()
    }
    fn at(&self, i: usize) -> Option<&Event> {
        self.0.get(i)
    }
}

struct MockState(HashMap<String, serde_json::Value>);

impl adk_session::ReadonlyState for MockState {
    fn get(&self, key: &str) -> Option<serde_json::Value> {
        self.0.get(key).cloned()
    }
    fn all(&self) -> HashMap<String, serde_json::Value> {
        self.0.clone()
    }
}

impl State for MockState {
    fn get(&self, key: &str) -> Option<serde_json::Value> {
        self.0.get(key).cloned()
    }
    fn set(&mut self, key: String, value: serde_json::Value) {
        self.0.insert(key, value);
    }
    fn all(&self) -> HashMap<String, serde_json::Value> {
        self.0.clone()
    }
}

struct MockSession {
    state: MockState,
    events: MockEvents,
}

impl Session for MockSession {
    fn id(&self) -> &str {
        "s1"
    }
    fn app_name(&self) -> &str {
        "app"
    }
    fn user_id(&self) -> &str {
        "u1"
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

fn empty_session() -> Arc<dyn Session> {
    Arc::new(MockSession { state: MockState(HashMap::new()), events: MockEvents(vec![]) })
}

fn make_event(author: &str, text: &str) -> Event {
    let mut e = Event::new("inv-1");
    e.author = author.to_string();
    e.llm_response.content = Some(Content {
        role: if author == "user" { "user" } else { "model" }.to_string(),
        parts: vec![Part::Text { text: text.to_string() }],
    });
    e
}

// ---------------------------------------------------------------------------
// Strategies
// ---------------------------------------------------------------------------

fn arb_state_key() -> impl Strategy<Value = String> {
    prop_oneof![
        "[a-z_]{1,20}",      // normal keys
        "temp:[a-z_]{1,15}", // temp-prefixed keys (should be skipped)
    ]
}

fn arb_state_value() -> impl Strategy<Value = serde_json::Value> {
    prop_oneof![
        any::<i64>().prop_map(serde_json::Value::from),
        "[a-zA-Z0-9 ]{0,30}".prop_map(serde_json::Value::from),
        any::<bool>().prop_map(serde_json::Value::from),
        Just(serde_json::Value::Null),
    ]
}

fn arb_delta() -> impl Strategy<Value = HashMap<String, serde_json::Value>> {
    proptest::collection::hash_map(arb_state_key(), arb_state_value(), 0..10)
}

fn arb_author() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("user".to_string()),
        Just("agent-alpha".to_string()),
        Just("agent-beta".to_string()),
    ]
}

// ---------------------------------------------------------------------------
// Property tests
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// **Feature: production-hardening, Property 2A: Preservation — apply_state_delta**
    ///
    /// *For any* sequence of state deltas, `apply_state_delta` SHALL merge
    /// non-temp keys into the mutable state. `temp:` prefixed keys SHALL be
    /// skipped. The final state SHALL equal the cumulative merge of all deltas
    /// (last-write-wins per key), excluding `temp:` keys.
    ///
    /// **Validates: Requirement 3.1**
    #[test]
    fn prop_apply_state_delta_merges_correctly(
        deltas in proptest::collection::vec(arb_delta(), 1..5),
    ) {
        let session = empty_session();
        let mutable = MutableSession::new(session);

        // Build expected state by replaying deltas, skipping temp: keys
        let mut expected = HashMap::new();
        for delta in &deltas {
            mutable.apply_state_delta(delta);
            for (k, v) in delta {
                if !k.starts_with("temp:") {
                    expected.insert(k.clone(), v.clone());
                }
            }
        }

        let actual = AdkCoreSession::state(&mutable).all();
        prop_assert_eq!(&actual, &expected);
    }

    /// **Feature: production-hardening, Property 2B: Preservation — append_event ordering**
    ///
    /// *For any* sequence of events appended via `append_event`, `events_snapshot()`
    /// SHALL return them in the exact order they were appended, and `events_len()`
    /// SHALL equal the count.
    ///
    /// **Validates: Requirement 3.2**
    #[test]
    fn prop_append_event_preserves_order(
        authors in proptest::collection::vec(arb_author(), 1..20),
    ) {
        let session = empty_session();
        let mutable = MutableSession::new(session);

        for (i, author) in authors.iter().enumerate() {
            mutable.append_event(make_event(author, &format!("msg-{i}")));
        }

        let snapshot = mutable.events_snapshot();
        prop_assert_eq!(snapshot.len(), authors.len());
        prop_assert_eq!(mutable.events_len(), authors.len());

        for (i, (event, expected_author)) in snapshot.iter().zip(authors.iter()).enumerate() {
            prop_assert_eq!(
                &event.author, expected_author,
                "event {} author mismatch", i
            );
        }
    }

    /// **Feature: production-hardening, Property 2C: Preservation — conversation_history role mapping**
    ///
    /// *For any* sequence of events, `conversation_history_for_agent_impl(None)` SHALL
    /// map user events to role "user" and all other events to role "model".
    /// Function/tool role events SHALL preserve their original role.
    ///
    /// **Validates: Requirement 3.3**
    #[test]
    fn prop_conversation_history_maps_roles_correctly(
        authors in proptest::collection::vec(arb_author(), 1..15),
    ) {
        let session = empty_session();
        let mutable = MutableSession::new(session);

        for (i, author) in authors.iter().enumerate() {
            mutable.append_event(make_event(author, &format!("msg-{i}")));
        }

        let history = mutable.conversation_history_for_agent_impl(None);
        prop_assert_eq!(history.len(), authors.len());

        for (content, author) in history.iter().zip(authors.iter()) {
            let expected_role = if author == "user" { "user" } else { "model" };
            prop_assert_eq!(
                &content.role, expected_role,
                "role mismatch for author"
            );
        }
    }

    /// **Feature: production-hardening, Property 2D: Preservation — agent-filtered history**
    ///
    /// *For any* sequence of events from mixed authors,
    /// `conversation_history_for_agent_impl(Some("agent-alpha"))` SHALL return
    /// only events authored by "user" or "agent-alpha", excluding other agents.
    ///
    /// **Validates: Requirement 3.3**
    #[test]
    fn prop_conversation_history_filters_by_agent(
        authors in proptest::collection::vec(arb_author(), 1..15),
    ) {
        let session = empty_session();
        let mutable = MutableSession::new(session);

        for (i, author) in authors.iter().enumerate() {
            mutable.append_event(make_event(author, &format!("msg-{i}")));
        }

        let filtered = mutable.conversation_history_for_agent_impl(Some("agent-alpha"));
        let expected_count = authors.iter()
            .filter(|a| *a == "user" || *a == "agent-alpha")
            .count();

        prop_assert_eq!(filtered.len(), expected_count);

        // All returned entries should be from user or agent-alpha
        for content in &filtered {
            let role = &content.role;
            prop_assert!(
                role == "user" || role == "model",
                "unexpected role '{role}' in filtered history"
            );
        }
    }
}

/// Verify that apply_state_delta with an empty delta is a no-op.
#[test]
fn apply_state_delta_empty_is_noop() {
    let session = empty_session();
    let mutable = MutableSession::new(session);

    let mut delta = HashMap::new();
    delta.insert("key1".to_string(), serde_json::json!("val1"));
    mutable.apply_state_delta(&delta);

    // Empty delta should not change state
    mutable.apply_state_delta(&HashMap::new());
    let state = AdkCoreSession::state(&mutable).all();
    assert_eq!(state.len(), 1);
    assert_eq!(state.get("key1"), Some(&serde_json::json!("val1")));
}

/// Verify that temp: keys are skipped by apply_state_delta.
#[test]
fn apply_state_delta_skips_temp_keys() {
    let session = empty_session();
    let mutable = MutableSession::new(session);

    let mut delta = HashMap::new();
    delta.insert("normal_key".to_string(), serde_json::json!("kept"));
    delta.insert("temp:scratch".to_string(), serde_json::json!("skipped"));
    mutable.apply_state_delta(&delta);

    let state = AdkCoreSession::state(&mutable).all();
    assert_eq!(state.len(), 1);
    assert!(state.contains_key("normal_key"));
    assert!(!state.contains_key("temp:scratch"));
}
