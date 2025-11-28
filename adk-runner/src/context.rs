use adk_core::{
    Agent, Artifacts, CallbackContext, Content, InvocationContext as InvocationContextTrait, Memory,
    ReadonlyContext, RunConfig,
};
use adk_session::{Session as AdkSession, State as AdkState};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{atomic::AtomicBool, Arc};

// Adapter to bridge adk_session::State to adk_core::State
#[allow(dead_code)] // Used via trait implementation
struct StateAdapter<'a>(&'a dyn AdkState);

impl<'a> adk_core::State for StateAdapter<'a> {
    fn get(&self, key: &str) -> Option<serde_json::Value> {
        self.0.get(key)
    }

    fn set(&mut self, _key: String, _value: serde_json::Value) {
        // State updates should happen via EventActions, not direct mutation
        // This is a read-only view of the state
        panic!("Direct state mutation not supported in InvocationContext");
    }

    fn all(&self) -> HashMap<String, serde_json::Value> {
        self.0.all()
    }
}

// Adapter to bridge adk_session::Session to adk_core::Session
struct SessionAdapter(Arc<dyn AdkSession>);

impl adk_core::Session for SessionAdapter {
    fn id(&self) -> &str {
        self.0.id()
    }

    fn app_name(&self) -> &str {
        self.0.app_name()
    }

    fn user_id(&self) -> &str {
        self.0.user_id()
    }

    fn state(&self) -> &dyn adk_core::State {
        // This is tricky because we need to return a reference to something that implements adk_core::State
        // But StateAdapter wraps a reference.
        // We can't easily return a reference to a temporary StateAdapter.
        // For now, we might need to unsafe cast or rethink.
        // Actually, since we can't return a reference to a temporary, we might need to implement State on the SessionAdapter itself?
        // Or change adk_core::Session to return a Box or Arc?
        // But we can't change adk_core easily.
        
        // HACK: For now, we will panic if state is accessed, or we need a better solution.
        // Wait, we can implement adk_core::State for SessionAdapter directly and return self?
        // No, SessionAdapter implements Session.
        
        // Let's implement adk_core::State for SessionAdapter (delegating to inner state)
        // and return self.
        unsafe { &*(self as *const Self as *const dyn adk_core::State) }
    }
}

impl adk_core::State for SessionAdapter {
    fn get(&self, key: &str) -> Option<serde_json::Value> {
        self.0.state().get(key)
    }
    
    fn set(&mut self, _key: String, _value: serde_json::Value) {
        panic!("Direct state mutation not supported");
    }
    
    fn all(&self) -> HashMap<String, serde_json::Value> {
        self.0.state().all()
    }
}

pub struct InvocationContext {
    invocation_id: String,
    agent: Arc<dyn Agent>,
    user_id: String,
    app_name: String,
    session_id: String,
    branch: String,
    user_content: Content,
    artifacts: Option<Arc<dyn Artifacts>>,
    memory: Option<Arc<dyn Memory>>,
    run_config: RunConfig,
    ended: Arc<AtomicBool>,
    session: Arc<SessionAdapter>,
}

impl InvocationContext {
    pub fn new(
        invocation_id: String,
        agent: Arc<dyn Agent>,
        user_id: String,
        app_name: String,
        session_id: String,
        user_content: Content,
        session: Arc<dyn AdkSession>,
    ) -> Self {
        Self {
            invocation_id,
            agent,
            user_id,
            app_name,
            session_id,
            branch: String::new(),
            user_content,
            artifacts: None,
            memory: None,
            run_config: RunConfig::default(),
            ended: Arc::new(AtomicBool::new(false)),
            session: Arc::new(SessionAdapter(session)),
        }
    }

    pub fn with_branch(mut self, branch: String) -> Self {
        self.branch = branch;
        self
    }

    pub fn with_artifacts(mut self, artifacts: Arc<dyn Artifacts>) -> Self {
        self.artifacts = Some(artifacts);
        self
    }

    pub fn with_memory(mut self, memory: Arc<dyn Memory>) -> Self {
        self.memory = Some(memory);
        self
    }

    pub fn with_run_config(mut self, config: RunConfig) -> Self {
        self.run_config = config;
        self
    }
}

#[async_trait]
impl ReadonlyContext for InvocationContext {
    fn invocation_id(&self) -> &str {
        &self.invocation_id
    }

    fn agent_name(&self) -> &str {
        self.agent.name()
    }

    fn user_id(&self) -> &str {
        &self.user_id
    }

    fn app_name(&self) -> &str {
        &self.app_name
    }

    fn session_id(&self) -> &str {
        &self.session_id
    }

    fn branch(&self) -> &str {
        &self.branch
    }

    fn user_content(&self) -> &Content {
        &self.user_content
    }
}

#[async_trait]
impl CallbackContext for InvocationContext {
    fn artifacts(&self) -> Option<Arc<dyn Artifacts>> {
        self.artifacts.clone()
    }
}

#[async_trait]
impl InvocationContextTrait for InvocationContext {
    fn agent(&self) -> Arc<dyn Agent> {
        self.agent.clone()
    }

    fn memory(&self) -> Option<Arc<dyn Memory>> {
        self.memory.clone()
    }
    
    fn session(&self) -> &dyn adk_core::Session {
        self.session.as_ref()
    }

    fn run_config(&self) -> &RunConfig {
        &self.run_config
    }

    fn end_invocation(&self) {
        self.ended
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn ended(&self) -> bool {
        self.ended.load(std::sync::atomic::Ordering::SeqCst)
    }
}
