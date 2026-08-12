//! Integration tests for the plain-JSON `POST /api/run` endpoint

use adk_server::create_app;
use adk_session::{
    CreateRequest, DeleteRequest, Event, GetRequest, ListRequest, Session, SessionService,
};
use async_stream::stream;
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use std::sync::Arc;
use tower::ServiceExt;

const FIXED_EVENT_ID: &str = "run-collect-event";
const FIXED_INVOCATION_ID: &str = "test-invocation";

/// Builds the exact event the mock agent emits, so tests can assert
/// whole-object equality against the response payload.
fn expected_agent_event() -> adk_core::Event {
    let mut event = adk_core::Event::with_id(FIXED_EVENT_ID, FIXED_INVOCATION_ID);
    event.author = "test-agent".to_string();
    event.llm_response.content =
        Some(adk_core::Content::new("model").with_text("Hello from the test agent"));
    event
}

// Mock implementations
struct MockAgentLoader;

#[async_trait]
impl adk_core::AgentLoader for MockAgentLoader {
    async fn load_agent(&self, _app_name: &str) -> adk_core::Result<Arc<dyn adk_core::Agent>> {
        Ok(Arc::new(MockAgent))
    }

    fn list_agents(&self) -> Vec<String> {
        vec!["test-app".to_string()]
    }

    fn root_agent(&self) -> Arc<dyn adk_core::Agent> {
        Arc::new(MockAgent)
    }
}

struct MockAgent;

#[async_trait]
impl adk_core::Agent for MockAgent {
    fn name(&self) -> &str {
        "test-agent"
    }

    fn description(&self) -> &str {
        "Test agent for the plain-JSON run endpoint"
    }

    fn sub_agents(&self) -> &[Arc<dyn adk_core::Agent>] {
        &[]
    }

    async fn run(
        &self,
        _ctx: Arc<dyn adk_core::InvocationContext>,
    ) -> adk_core::Result<adk_core::EventStream> {
        let s = stream! {
            yield Ok(expected_agent_event());
        };
        Ok(Box::pin(s))
    }
}

struct MockSessionService;

#[async_trait]
impl SessionService for MockSessionService {
    async fn create(&self, req: CreateRequest) -> adk_core::Result<Box<dyn Session>> {
        Ok(Box::new(MockSession {
            id: req.session_id.unwrap_or_else(|| "generated-id".to_string()),
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

fn create_test_app() -> axum::Router {
    let config =
        adk_server::ServerConfig::new(Arc::new(MockAgentLoader), Arc::new(MockSessionService));
    create_app(config)
}

async fn post_run(app: axum::Router, body: serde_json::Value) -> axum::response::Response {
    app.oneshot(
        Request::builder()
            .method("POST")
            .uri("/api/run")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap(),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn test_run_returns_json_array_of_events() {
    let app = create_test_app();

    let body = serde_json::json!({
        "appName": "test-app",
        "userId": "user123",
        "sessionId": "session456",
        "newMessage": {
            "role": "user",
            "parts": [
                {"text": "Hello, world!"}
            ]
        }
    });

    let response = post_run(app, body).await;
    assert_eq!(response.status(), StatusCode::OK);

    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(content_type.starts_with("application/json"), "content-type was {content_type}");

    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let events: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    let array = events.as_array().expect("response body must be a JSON array");
    assert_eq!(array.len(), 1);

    // Whole-object equality: the response event must match the mock agent's
    // event exactly, modulo the runner-assigned timestamp.
    let mut expected = serde_json::to_value(expected_agent_event()).unwrap();
    expected["timestamp"] = array[0]["timestamp"].clone();
    assert_eq!(array[0], expected);
}

#[tokio::test]
async fn test_run_without_new_message_is_bad_request() {
    let app = create_test_app();

    let body = serde_json::json!({
        "appName": "test-app",
        "userId": "user123",
        "sessionId": "session456"
    });

    let response = post_run(app, body).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}
