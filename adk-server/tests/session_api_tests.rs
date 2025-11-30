use adk_server::create_app;
use adk_session::{CreateRequest, DeleteRequest, Event, GetRequest, ListRequest, Session, SessionService};
use async_trait::async_trait;
use async_stream::stream;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use std::sync::Arc;
use tower::ServiceExt;

struct MockAgentLoader;

#[async_trait]
impl adk_core::AgentLoader for MockAgentLoader {
    async fn load_agent(&self, _app_name: &str) -> adk_core::Result<Arc<dyn adk_core::Agent>> {
        Err(adk_core::AdkError::Agent("not implemented".to_string()))
    }
    
    fn list_agents(&self) -> Vec<String> {
        vec![]
    }
    
    fn root_agent(&self) -> Arc<dyn adk_core::Agent> {
        // This is a mock - we'll never actually use this in these tests
        // Create a minimal agent that satisfies the trait
        Arc::new(MockAgent)
    }
}

// Minimal mock agent for testing
struct MockAgent;

#[async_trait]
impl adk_core::Agent for MockAgent {
    fn name(&self) -> &str {
        "mock"
    }
    
    fn description(&self) -> &str {
        "Mock agent for testing"
    }
    
    fn sub_agents(&self) -> &[Arc<dyn adk_core::Agent>] {
        &[]
    }
    
    async fn run(&self, _ctx: Arc<dyn adk_core::InvocationContext>) -> adk_core::Result<adk_core::EventStream> {
        let s = stream! {
            // Yield nothing, but satisfy type inference
            if false {
                yield Ok(adk_core::Event::new("mock"));
            }
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

#[tokio::test]
async fn test_create_session() {
    let config = adk_server::ServerConfig::new(
        Arc::new(MockAgentLoader),
        Arc::new(MockSessionService),
    );
    let app = create_app(config);

    let body = serde_json::json!({
        "appName": "test-app",
        "userId": "user123",
        "sessionId": "session456"
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/sessions")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_get_session() {
    let config = adk_server::ServerConfig::new(
        Arc::new(MockAgentLoader),
        Arc::new(MockSessionService),
    );
    let app = create_app(config);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/sessions/test-app/user123/session456")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_delete_session() {
    let config = adk_server::ServerConfig::new(
        Arc::new(MockAgentLoader),
        Arc::new(MockSessionService),
    );
    let app = create_app(config);

    let response = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/sessions/test-app/user123/session456")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}
