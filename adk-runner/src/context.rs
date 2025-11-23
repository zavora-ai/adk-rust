use adk_core::{
    Agent, Artifacts, Content, InvocationContext as InvocationContextTrait, Memory, ReadonlyContext,
    RunConfig,
};
use async_trait::async_trait;
use std::sync::{atomic::AtomicBool, Arc};

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
}

impl InvocationContext {
    pub fn new(
        invocation_id: String,
        agent: Arc<dyn Agent>,
        user_id: String,
        app_name: String,
        session_id: String,
        user_content: Content,
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
impl InvocationContextTrait for InvocationContext {
    fn agent(&self) -> Arc<dyn Agent> {
        self.agent.clone()
    }

    fn artifacts(&self) -> Option<Arc<dyn Artifacts>> {
        self.artifacts.clone()
    }

    fn memory(&self) -> Option<Arc<dyn Memory>> {
        self.memory.clone()
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
