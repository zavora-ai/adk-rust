//! A `Runner` shared across tenants must not confuse their runs or their writes.
//!
//! Two defects motivated these tests:
//!
//! 1. Active runs were tracked in a `HashMap<String, CancellationToken>` keyed by
//!    the raw session ID. A session ID is only unique within an app and user, so
//!    `(app-a, user-a, shared-id)` and `(app-b, user-b, shared-id)` collided in one
//!    map, and two concurrent runs for one identity overwrote each other. The drop
//!    guard then removed the key unconditionally, so a finishing run could
//!    deregister a different run that was still going.
//! 2. The Runner resolved a session with the full `(app, user, session)` triple and
//!    then persisted events through `append_event(session_id, event)`, discarding
//!    the identity at the write boundary. A backend whose natural key is composite
//!    could not bind an event to its tenant.

use std::sync::Arc;
use std::sync::Mutex;

use adk_core::{Agent, Content, EventStream, InvocationContext, Part, Result, SessionId, UserId};
use adk_runner::Runner;
use adk_session::{AppendEventRequest, Event, Events, GetRequest, Session, SessionService, State};
use async_trait::async_trait;
use futures::StreamExt;

// ── Mocks ─────────────────────────────────────────────────────────────

struct MockAgent;

#[async_trait]
impl Agent for MockAgent {
    fn name(&self) -> &str {
        "test-agent"
    }
    fn description(&self) -> &str {
        "mock"
    }
    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }
    async fn run(&self, _ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        Ok(Box::pin(futures::stream::empty()))
    }
}

struct MockEvents;

impl Events for MockEvents {
    fn all(&self) -> Vec<Event> {
        vec![]
    }
    fn len(&self) -> usize {
        0
    }
    fn at(&self, _index: usize) -> Option<&Event> {
        None
    }
}

struct MockState;

impl adk_session::ReadonlyState for MockState {
    fn get(&self, _key: &str) -> Option<serde_json::Value> {
        None
    }
    fn all(&self) -> std::collections::HashMap<String, serde_json::Value> {
        std::collections::HashMap::new()
    }
}

impl State for MockState {
    fn get(&self, _key: &str) -> Option<serde_json::Value> {
        None
    }
    fn set(&mut self, _key: String, _value: serde_json::Value) {}
    fn all(&self) -> std::collections::HashMap<String, serde_json::Value> {
        std::collections::HashMap::new()
    }
}

struct MockSession {
    id: String,
    app_name: String,
    user_id: String,
}

impl Session for MockSession {
    fn id(&self) -> &str {
        &self.id
    }
    fn app_name(&self) -> &str {
        &self.app_name
    }
    fn user_id(&self) -> &str {
        &self.user_id
    }
    fn state(&self) -> &dyn State {
        &MockState
    }
    fn events(&self) -> &dyn Events {
        &MockEvents
    }
    fn last_update_time(&self) -> chrono::DateTime<chrono::Utc> {
        chrono::Utc::now()
    }
}

/// A backend whose natural key is the full identity triple. It refuses a write
/// that arrives with only a session ID, exactly as a composite-key store must.
struct StrictCompositeService {
    app_name: String,
    user_id: String,
    /// Identities recorded by identity-aware writes, in order.
    identity_writes: Mutex<Vec<(String, String, String)>>,
    /// Count of writes that arrived with a raw session ID only.
    raw_writes: Mutex<usize>,
}

impl StrictCompositeService {
    fn new(app_name: &str, user_id: &str) -> Self {
        Self {
            app_name: app_name.to_string(),
            user_id: user_id.to_string(),
            identity_writes: Mutex::new(Vec::new()),
            raw_writes: Mutex::new(0),
        }
    }
}

#[async_trait]
impl SessionService for StrictCompositeService {
    async fn create(&self, _req: adk_session::CreateRequest) -> Result<Box<dyn Session>> {
        unimplemented!("not exercised")
    }

    async fn get(&self, req: GetRequest) -> Result<Box<dyn Session>> {
        Ok(Box::new(MockSession {
            id: req.session_id,
            app_name: self.app_name.clone(),
            user_id: self.user_id.clone(),
        }))
    }

    async fn list(&self, _req: adk_session::ListRequest) -> Result<Vec<Box<dyn Session>>> {
        Ok(vec![])
    }

    async fn delete(&self, _req: adk_session::DeleteRequest) -> Result<()> {
        Ok(())
    }

    async fn append_event(&self, _session_id: &str, _event: Event) -> Result<()> {
        *self.raw_writes.lock().unwrap() += 1;
        Err(adk_core::AdkError::session(
            "this backend keys events by (app_name, user_id, session_id); \
             a raw session ID cannot identify a session",
        ))
    }

    async fn append_event_for_identity(&self, req: AppendEventRequest) -> Result<()> {
        self.identity_writes.lock().unwrap().push((
            req.identity.app_name.as_ref().to_string(),
            req.identity.user_id.as_ref().to_string(),
            req.identity.session_id.as_ref().to_string(),
        ));
        Ok(())
    }
}

fn runner_for(service: Arc<StrictCompositeService>, app_name: &str) -> Runner {
    Runner::builder()
        .app_name(app_name)
        .agent(Arc::new(MockAgent) as Arc<dyn Agent>)
        .session_service(service as Arc<dyn SessionService>)
        .build()
        .unwrap()
}

// ── Tests ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn every_persistence_write_carries_the_full_identity() {
    let service = Arc::new(StrictCompositeService::new("app-a", "user-a"));
    let runner = runner_for(service.clone(), "app-a");

    let mut stream = runner
        .run(
            UserId::new("user-a").unwrap(),
            SessionId::new("session-1").unwrap(),
            Content { role: "user".to_string(), parts: vec![Part::Text { text: "hi".into() }] },
        )
        .await
        .unwrap();
    while let Some(event) = stream.next().await {
        event.expect("the run must not fail: a composite-key backend rejects raw-ID writes");
    }

    assert_eq!(
        *service.raw_writes.lock().unwrap(),
        0,
        "the Runner still wrote through the raw session-ID API, which a composite-key \
         backend cannot bind to a tenant"
    );
    let writes = service.identity_writes.lock().unwrap().clone();
    assert!(!writes.is_empty(), "the user event must be persisted");
    for write in &writes {
        assert_eq!(
            write,
            &("app-a".to_string(), "user-a".to_string(), "session-1".to_string()),
            "an event was persisted under the wrong identity"
        );
    }
}

#[tokio::test]
async fn runs_sharing_a_session_id_across_identities_do_not_collide() {
    // One Runner per app, as a multi-tenant deployment would have, but the same
    // raw session ID. Registration must not overwrite across identities.
    let service_a = Arc::new(StrictCompositeService::new("app-a", "user-a"));
    let runner_a = runner_for(service_a, "app-a");

    let stream_a = runner_a
        .run(
            UserId::new("user-a").unwrap(),
            SessionId::new("shared-id").unwrap(),
            Content { role: "user".to_string(), parts: vec![Part::Text { text: "a".into() }] },
        )
        .await
        .unwrap();

    // While that run is registered, an interrupt for a different identity with the
    // same session ID must not claim it.
    assert!(
        !runner_a.interrupt_identity("app-b", "user-b", "shared-id"),
        "an interrupt for another tenant's identity matched this run"
    );
    assert!(
        runner_a.interrupt_identity("app-a", "user-a", "shared-id"),
        "the run's own identity must be interruptible"
    );

    drop(stream_a);
}

#[tokio::test]
async fn two_runs_for_one_identity_are_tracked_separately() {
    let service = Arc::new(StrictCompositeService::new("app-a", "user-a"));
    let runner = runner_for(service, "app-a");

    let content = |t: &str| Content {
        role: "user".to_string(),
        parts: vec![Part::Text { text: t.to_string() }],
    };

    let first = runner
        .run(UserId::new("user-a").unwrap(), SessionId::new("session-1").unwrap(), content("first"))
        .await
        .unwrap();
    let second = runner
        .run(
            UserId::new("user-a").unwrap(),
            SessionId::new("session-1").unwrap(),
            content("second"),
        )
        .await
        .unwrap();

    assert_eq!(
        runner.active_runs().len(),
        2,
        "two overlapping runs for one identity must both be registered; \
         keying by session ID alone hid the first"
    );

    // Reverse completion order: dropping the second must not deregister the first.
    drop(second);
    assert_eq!(
        runner.active_runs().len(),
        1,
        "cleanup removed a different run than the one that finished"
    );
    assert_eq!(runner.active_session_ids(), vec!["session-1".to_string()]);

    drop(first);
    assert!(runner.active_runs().is_empty(), "the last run must deregister itself");
    assert!(runner.active_session_ids().is_empty());
}

#[tokio::test]
async fn interrupting_a_session_cancels_all_of_its_runs() {
    let service = Arc::new(StrictCompositeService::new("app-a", "user-a"));
    let runner = runner_for(service, "app-a");

    let content =
        Content { role: "user".to_string(), parts: vec![Part::Text { text: "work".to_string() }] };
    let first = runner
        .run(UserId::new("user-a").unwrap(), SessionId::new("session-1").unwrap(), content.clone())
        .await
        .unwrap();
    let second = runner
        .run(UserId::new("user-a").unwrap(), SessionId::new("session-1").unwrap(), content)
        .await
        .unwrap();

    // Session-wide semantics: the documented `interrupt(session_id)` cancels every
    // run for that session rather than whichever one happened to register last.
    assert!(runner.interrupt("session-1"));
    assert!(!runner.interrupt("session-absent"));

    drop(first);
    drop(second);
}
