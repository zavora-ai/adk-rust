//! Branch isolation for `ParallelAgent` sub-agents.
//!
//! Concurrent branches must not read each other's output. `ParallelAgent` places
//! each sub-agent on its own branch (`{parent}.{parallel}.{sub_agent}`) and
//! stamps produced events with it; a branch-scoped history read then excludes
//! siblings while keeping ancestors visible. This mirrors ADK Python's
//! `_is_event_belongs_to_branch` and ADK Go's `eventBelongsToBranch`.

use std::collections::HashMap;
use std::sync::Arc;

use adk_core::Session as AdkCoreSession;
use adk_core::{Content, Part, event_belongs_to_branch};
use adk_runner::MutableSession;
use adk_session::{Event, Events, Session, State};

// ── Mock session (mirrors preservation_session_test.rs) ────────────────

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

/// A session view holding `events`.
fn session_with(events: Vec<Event>) -> MutableSession {
    MutableSession::new(Arc::new(MockSession {
        state: MockState(HashMap::new()),
        events: MockEvents(events),
    }))
}

fn model_event(author: &str, branch: &str, text: &str) -> Event {
    let mut event = Event::new("inv-1");
    event.author = author.to_string();
    event.branch = branch.to_string();
    event.llm_response.content = Some(Content {
        role: "model".to_string(),
        parts: vec![Part::Text { text: text.to_string() }],
    });
    event
}

fn user_event(text: &str) -> Event {
    let mut event = Event::new("inv-1");
    event.author = "user".to_string();
    // The runner records the user turn before any fan-out, so it carries no branch.
    event.llm_response.content = Some(Content {
        role: "user".to_string(),
        parts: vec![Part::Text { text: text.to_string() }],
    });
    event
}

fn texts(history: &[Content]) -> Vec<String> {
    history.iter().map(|c| c.parts.iter().filter_map(|p| p.text()).collect::<String>()).collect()
}

#[test]
fn a_branch_does_not_see_its_sibling() {
    let session = session_with(vec![
        user_event("question"),
        model_event("analyst_a", "parallel.analyst_a", "answer from A"),
        model_event("analyst_b", "parallel.analyst_b", "answer from B"),
    ]);

    let seen_by_a = texts(&session.conversation_history_scoped(None, "parallel.analyst_a"));
    assert!(seen_by_a.contains(&"question".to_string()), "the user turn stays visible");
    assert!(seen_by_a.contains(&"answer from A".to_string()), "own output stays visible");
    assert!(
        !seen_by_a.contains(&"answer from B".to_string()),
        "a sibling branch must not be visible, got: {seen_by_a:?}"
    );

    let seen_by_b = texts(&session.conversation_history_scoped(None, "parallel.analyst_b"));
    assert!(seen_by_b.contains(&"answer from B".to_string()));
    assert!(!seen_by_b.contains(&"answer from A".to_string()));
}

#[test]
fn ancestor_turns_remain_visible() {
    let session = session_with(vec![
        user_event("question"),
        // Produced by the parent before the fan-out.
        model_event("supervisor", "parallel", "framing from the supervisor"),
        model_event("analyst_a", "parallel.analyst_a", "answer from A"),
    ]);

    let seen = texts(&session.conversation_history_scoped(None, "parallel.analyst_a"));
    assert!(
        seen.contains(&"framing from the supervisor".to_string()),
        "an ancestor branch must stay visible, got: {seen:?}"
    );
}

#[test]
fn unbranched_reads_are_unaffected() {
    // An agent outside any fan-out passes an empty branch and must still see
    // everything, so existing behaviour is preserved.
    let session = session_with(vec![
        user_event("question"),
        model_event("analyst_a", "parallel.analyst_a", "answer from A"),
        model_event("analyst_b", "parallel.analyst_b", "answer from B"),
    ]);

    let seen = texts(&session.conversation_history_scoped(None, ""));
    assert_eq!(seen.len(), 3, "an unscoped read sees every event, got: {seen:?}");
    // The pre-existing entry points are unscoped too.
    assert_eq!(session.conversation_history().len(), 3);
}

#[test]
fn events_without_a_branch_stay_globally_visible() {
    // Sessions written before branches were stamped must not disappear from
    // history once a branch-scoped read is used.
    let session = session_with(vec![
        user_event("question"),
        model_event("legacy_agent", "", "answer with no branch"),
    ]);

    let seen = texts(&session.conversation_history_scoped(None, "parallel.analyst_a"));
    assert!(
        seen.contains(&"answer with no branch".to_string()),
        "an unbranched event must remain visible, got: {seen:?}"
    );
}

#[test]
fn nested_branches_are_hidden_from_the_parent() {
    let session = session_with(vec![
        user_event("question"),
        model_event("inner", "parallel.a.inner_parallel.x", "deep output"),
    ]);

    let seen = texts(&session.conversation_history_scoped(None, "parallel.a"));
    assert!(
        !seen.contains(&"deep output".to_string()),
        "a descendant branch must not leak upward, got: {seen:?}"
    );
    // ...but the descendant itself sees its ancestors.
    assert!(event_belongs_to_branch("parallel.a.inner_parallel.x", "parallel.a"));
}
