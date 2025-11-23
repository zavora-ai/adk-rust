use adk_core::{Agent, Content, EventStream, InvocationContext, Part, Result};
use adk_runner::{Runner, RunnerConfig};
use adk_session::{Event, Events, GetRequest, Session, SessionService, State};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::sync::Arc;

// Mock Agent
struct MockAgent {
    name: String,
}

#[async_trait]
impl Agent for MockAgent {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        "Mock agent"
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, _ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        Ok(Box::pin(futures::stream::empty()))
    }
}

// Mock Events
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

// Mock State
struct MockState;

impl adk_session::ReadonlyState for MockState {
    fn get(&self, _key: &str) -> Option<serde_json::Value> {
        None
    }

    fn all(&self) -> std::collections::HashMap<String, serde_json::Value> {
        std::collections::HashMap::new()
    }
}

impl adk_session::State for MockState {
    fn get(&self, _key: &str) -> Option<serde_json::Value> {
        None
    }

    fn set(&mut self, _key: String, _value: serde_json::Value) {}

    fn all(&self) -> std::collections::HashMap<String, serde_json::Value> {
        std::collections::HashMap::new()
    }
}

// Mock Session
struct MockSession {
    id: String,
    app_name: String,
    user_id: String,
    events: MockEvents,
    state: MockState,
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
        &self.state
    }

    fn events(&self) -> &dyn Events {
        &self.events
    }

    fn last_update_time(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

// Mock SessionService
struct MockSessionService;

#[async_trait]
impl SessionService for MockSessionService {
    async fn create(&self, _req: adk_session::CreateRequest) -> Result<Box<dyn Session>> {
        unimplemented!()
    }

    async fn get(&self, req: GetRequest) -> Result<Box<dyn Session>> {
        Ok(Box::new(MockSession {
            id: req.session_id,
            app_name: req.app_name,
            user_id: req.user_id,
            events: MockEvents { events: vec![] },
            state: MockState,
        }))
    }

    async fn list(&self, _req: adk_session::ListRequest) -> Result<Vec<Box<dyn Session>>> {
        Ok(vec![])
    }

    async fn delete(&self, _req: adk_session::DeleteRequest) -> Result<()> {
        Ok(())
    }

    async fn append_event(&self, _session_id: &str, _event: Event) -> Result<()> {
        Ok(())
    }
}

#[test]
fn test_runner_creation() {
    let agent = Arc::new(MockAgent {
        name: "test_agent".to_string(),
    });

    let session_service = Arc::new(MockSessionService);

    let runner = Runner::new(RunnerConfig {
        app_name: "test_app".to_string(),
        agent,
        session_service,
        artifact_service: None,
        memory_service: None,
    });

    assert!(runner.is_ok());
}

#[tokio::test]
async fn test_runner_run() {
    let agent = Arc::new(MockAgent {
        name: "test_agent".to_string(),
    });

    let session_service = Arc::new(MockSessionService);

    let runner = Runner::new(RunnerConfig {
        app_name: "test_app".to_string(),
        agent,
        session_service,
        artifact_service: None,
        memory_service: None,
    })
    .unwrap();

    let content = Content {
        role: "user".to_string(),
        parts: vec![Part::Text {
            text: "Hello".to_string(),
        }],
    };

    let result = runner
        .run("user123".to_string(), "session456".to_string(), content)
        .await;

    assert!(result.is_ok());
}
