use adk_core::{
    inject_session_state, AdkError, Agent, Artifacts, CallbackContext, Content, InvocationContext,
    Memory, ReadonlyContext, RunConfig, Session, State,
};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

// --- Mocks ---

struct MockState {
    data: HashMap<String, Value>,
}

impl MockState {
    fn new() -> Self {
        let mut data = HashMap::new();
        data.insert("user_name".to_string(), json!("Alice"));
        data.insert("role".to_string(), json!("admin"));
        data.insert("user:pref".to_string(), json!("dark_mode"));
        Self { data }
    }
}

impl State for MockState {
    fn get(&self, key: &str) -> Option<Value> {
        self.data.get(key).cloned()
    }

    fn set(&mut self, _key: String, _value: Value) {
        unimplemented!()
    }

    fn all(&self) -> HashMap<String, Value> {
        self.data.clone()
    }
}

struct MockSession {
    state: MockState,
}

impl MockSession {
    fn new() -> Self {
        Self { state: MockState::new() }
    }
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
    fn conversation_history(&self) -> Vec<Content> {
        Vec::new()
    }
}

struct MockArtifacts;

#[async_trait]
impl Artifacts for MockArtifacts {
    async fn save(&self, _name: &str, _data: &adk_core::Part) -> adk_core::Result<i64> {
        unimplemented!()
    }

    async fn load(&self, name: &str) -> adk_core::Result<adk_core::Part> {
        if name == "welcome.txt" {
            Ok(adk_core::Part::Text { text: "Welcome to ADK!".to_string() })
        } else {
            Err(AdkError::Agent("Artifact not found".to_string()))
        }
    }

    async fn list(&self) -> adk_core::Result<Vec<String>> {
        Ok(vec!["welcome.txt".to_string()])
    }
}

struct MockContext {
    session: MockSession,
    artifacts: Option<Arc<dyn Artifacts>>,
}

impl MockContext {
    fn new() -> Self {
        Self { session: MockSession::new(), artifacts: None }
    }

    fn with_artifacts(mut self) -> Self {
        self.artifacts = Some(Arc::new(MockArtifacts));
        self
    }
}

#[async_trait]
impl ReadonlyContext for MockContext {
    fn invocation_id(&self) -> &str {
        "inv-1"
    }
    fn agent_name(&self) -> &str {
        "test-agent"
    }
    fn user_id(&self) -> &str {
        "user-1"
    }
    fn app_name(&self) -> &str {
        "test-app"
    }
    fn session_id(&self) -> &str {
        "session-1"
    }
    fn branch(&self) -> &str {
        "main"
    }
    fn user_content(&self) -> &Content {
        unimplemented!()
    }
}

#[async_trait]
impl CallbackContext for MockContext {
    fn artifacts(&self) -> Option<Arc<dyn Artifacts>> {
        self.artifacts.clone()
    }
}

#[async_trait]
impl InvocationContext for MockContext {
    fn agent(&self) -> Arc<dyn Agent> {
        unimplemented!()
    }
    fn memory(&self) -> Option<Arc<dyn Memory>> {
        None
    }
    fn session(&self) -> &dyn Session {
        &self.session
    }
    fn run_config(&self) -> &RunConfig {
        unimplemented!()
    }
    fn end_invocation(&self) {}
    fn ended(&self) -> bool {
        false
    }
}

// --- Tests ---

#[tokio::test]
async fn test_simple_substitution() {
    let ctx = MockContext::new();
    let template = "Hello {user_name}, welcome back!";
    let result = inject_session_state(&ctx, template).await.unwrap();
    assert_eq!(result, "Hello Alice, welcome back!");
}

#[tokio::test]
async fn test_multiple_substitutions() {
    let ctx = MockContext::new();
    let template = "User {user_name} has role {role}.";
    let result = inject_session_state(&ctx, template).await.unwrap();
    assert_eq!(result, "User Alice has role admin.");
}

#[tokio::test]
async fn test_optional_substitution_exists() {
    let ctx = MockContext::new();
    let template = "Role: {role?}";
    let result = inject_session_state(&ctx, template).await.unwrap();
    assert_eq!(result, "Role: admin");
}

#[tokio::test]
async fn test_optional_substitution_missing() {
    let ctx = MockContext::new();
    let template = "Group: {group?}";
    let result = inject_session_state(&ctx, template).await.unwrap();
    assert_eq!(result, "Group: ");
}

#[tokio::test]
async fn test_missing_variable_error() {
    let ctx = MockContext::new();
    let template = "Group: {group}";
    let result = inject_session_state(&ctx, template).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_prefixed_variable() {
    let ctx = MockContext::new();
    let template = "Pref: {user:pref}";
    let result = inject_session_state(&ctx, template).await.unwrap();
    assert_eq!(result, "Pref: dark_mode");
}

#[tokio::test]
async fn test_artifact_injection() {
    let ctx = MockContext::new().with_artifacts();
    let template = "Content: {artifact.welcome.txt}";
    let result = inject_session_state(&ctx, template).await.unwrap();
    assert_eq!(result, "Content: Welcome to ADK!");
}

#[tokio::test]
async fn test_artifact_injection_missing_error() {
    let ctx = MockContext::new().with_artifacts();
    let template = "Content: {artifact.missing.txt}";
    let result = inject_session_state(&ctx, template).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_artifact_injection_missing_optional() {
    let ctx = MockContext::new().with_artifacts();
    let template = "Content: {artifact.missing.txt?}";
    let result = inject_session_state(&ctx, template).await.unwrap();
    assert_eq!(result, "Content: ");
}

#[tokio::test]
async fn test_no_artifacts_service_error() {
    let ctx = MockContext::new(); // No artifacts service
    let template = "Content: {artifact.welcome.txt}";
    let result = inject_session_state(&ctx, template).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_complex_mix() {
    let ctx = MockContext::new().with_artifacts();
    let template = "{user_name} read '{artifact.welcome.txt}' (Theme: {user:pref?})";
    let result = inject_session_state(&ctx, template).await.unwrap();
    assert_eq!(result, "Alice read 'Welcome to ADK!' (Theme: dark_mode)");
}
