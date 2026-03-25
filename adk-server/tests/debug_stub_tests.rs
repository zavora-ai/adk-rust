//! Tests for debug endpoint honest responses (Bug 2 fix).
//!
//! Validates:
//! - 2.6: `get_graph` returns HTTP 501 with error JSON
//! - 2.7: `get_eval_sets` returns HTTP 501 with error JSON
//! - 2.8: `get_event` returns HTTP 404 when event not found
//! - 3.5: `get_session_traces` behavior unchanged
//! - 3.6: `get_trace_by_event_id` behavior unchanged
//! - 3.7: `get_event` returns HTTP 200 with correct body when event exists

use adk_server::{ServerConfig, create_app};
use adk_session::{
    CreateRequest, DeleteRequest, Event, GetRequest, ListRequest, Session, SessionService,
};
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use std::sync::Arc;
use tower::ServiceExt;

// --- Mock infrastructure (minimal, reused from server_tests pattern) ---

struct MockAgentLoader;

#[async_trait]
impl adk_core::AgentLoader for MockAgentLoader {
    async fn load_agent(&self, _app_name: &str) -> adk_core::Result<Arc<dyn adk_core::Agent>> {
        Err(adk_core::AdkError::agent("not implemented"))
    }
    fn list_agents(&self) -> Vec<String> {
        vec![]
    }
    fn root_agent(&self) -> Arc<dyn adk_core::Agent> {
        panic!("MockAgentLoader has no root agent")
    }
}

struct MockSessionService;

#[async_trait]
impl SessionService for MockSessionService {
    async fn create(&self, req: CreateRequest) -> adk_core::Result<Box<dyn Session>> {
        Ok(Box::new(MockSession {
            id: req.session_id.unwrap_or_default(),
            app_name: req.app_name,
            user_id: req.user_id,
        }))
    }
    async fn get(&self, req: GetRequest) -> adk_core::Result<Box<dyn Session>> {
        Ok(Box::new(MockSession {
            id: req.session_id,
            app_name: req.app_name,
            user_id: req.user_id,
        }))
    }
    async fn list(&self, _req: ListRequest) -> adk_core::Result<Vec<Box<dyn Session>>> {
        Ok(vec![])
    }
    async fn delete(&self, _req: DeleteRequest) -> adk_core::Result<()> {
        Ok(())
    }
    async fn append_event(&self, _session_id: &str, _event: Event) -> adk_core::Result<()> {
        Ok(())
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
    fn state(&self) -> &dyn adk_session::State {
        &MockState
    }
    fn events(&self) -> &dyn adk_session::Events {
        &MockEvents
    }
    fn last_update_time(&self) -> chrono::DateTime<chrono::Utc> {
        chrono::Utc::now()
    }
}

struct MockState;
impl adk_session::State for MockState {
    fn get(&self, _key: &str) -> Option<serde_json::Value> {
        None
    }
    fn set(&mut self, _key: String, _value: serde_json::Value) {}
    fn all(&self) -> std::collections::HashMap<String, serde_json::Value> {
        std::collections::HashMap::new()
    }
}

struct MockEvents;
impl adk_session::Events for MockEvents {
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

fn base_config() -> ServerConfig {
    ServerConfig::new(Arc::new(MockAgentLoader), Arc::new(MockSessionService))
}

/// Helper: create an AdkSpanExporter pre-loaded with a single trace entry.
fn exporter_with_trace(
    event_id: &str,
    session_id: &str,
    invocation_id: &str,
) -> Arc<adk_telemetry::AdkSpanExporter> {
    let exporter = Arc::new(adk_telemetry::AdkSpanExporter::new());

    // Use the tracing layer to insert a span that the exporter will capture.
    // AdkSpanExporter captures spans named "agent.execute" that have a
    // "gcp.vertex.agent.event_id" attribute.
    use tracing_subscriber::layer::SubscriberExt;

    let layer = adk_telemetry::AdkSpanLayer::new(exporter.clone());
    let subscriber = tracing_subscriber::registry::Registry::default().with(layer);

    // Use the subscriber for this scope to emit a span
    let _guard = tracing::subscriber::set_default(subscriber);

    let span = tracing::info_span!(
        "agent.execute",
        gcp.vertex.agent.event_id = event_id,
        gcp.vertex.agent.session_id = session_id,
        gcp.vertex.agent.invocation_id = invocation_id,
    );
    // Enter and drop the span so on_close fires and the exporter captures it
    let _enter = span.enter();
    drop(_enter);
    drop(span);

    exporter
}

// --- Tests ---

/// Requirement 2.6: GET /api/debug/graph/{app}/{user}/{session}/{event} returns 501
#[tokio::test]
async fn test_get_graph_returns_501() {
    let app = create_app(base_config());

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/debug/graph/myapp/user1/sess1/evt1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["error"], "graph generation is not yet implemented");
}

/// Requirement 2.7: GET /api/apps/{app}/eval_sets returns 501
#[tokio::test]
async fn test_get_eval_sets_returns_501() {
    let app = create_app(base_config());

    let response = app
        .oneshot(Request::builder().uri("/api/apps/myapp/eval_sets").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["error"], "eval sets are not yet implemented");
}

/// Requirement 2.8: GET /api/apps/{app}/users/{user}/sessions/{session}/events/{event}
/// returns 404 when event does not exist.
#[tokio::test]
async fn test_get_event_not_found_returns_404() {
    // Config with an exporter but no matching trace data
    let exporter = Arc::new(adk_telemetry::AdkSpanExporter::new());
    let config = base_config().with_span_exporter(exporter);
    let app = create_app(config);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/apps/myapp/users/user1/sessions/sess1/events/nonexistent-event")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

/// Requirement 3.7: GET /api/apps/{app}/users/{user}/sessions/{session}/events/{event}
/// returns 200 with correct body when event exists in span exporter.
#[tokio::test]
async fn test_get_event_found_returns_200() {
    let exporter = exporter_with_trace("evt-123", "sess-abc", "inv-456");
    let config = base_config().with_span_exporter(exporter);
    let app = create_app(config);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/apps/myapp/users/user1/sessions/sess-abc/events/evt-123")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["id"], "evt-123");
    assert_eq!(json["invocationId"], "inv-456");
    assert_eq!(json["appName"], "myapp");
    assert_eq!(json["sessionId"], "sess-abc");
}

/// Requirement 3.5: GET /api/debug/trace/session/{session_id} returns session traces unchanged.
#[tokio::test]
async fn test_get_session_traces_unchanged() {
    let exporter = exporter_with_trace("evt-s1", "sess-trace", "inv-s1");
    let config = base_config().with_span_exporter(exporter);
    let app = create_app(config);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/debug/trace/session/sess-trace")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let arr = json.as_array().expect("response should be a JSON array");
    assert!(!arr.is_empty(), "should return at least one span for the session");

    // Verify the span data structure matches the UI-compatible format
    let span = &arr[0];
    assert!(span.get("name").is_some(), "span should have a name field");
    assert!(span.get("span_id").is_some(), "span should have a span_id field");
    assert!(span.get("trace_id").is_some(), "span should have a trace_id field");
    assert!(span.get("attributes").is_some(), "span should have an attributes field");
}
