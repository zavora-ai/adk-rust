use crate::Result;
use async_trait::async_trait;
use std::sync::Arc;

/// Trait for loading agents by application name.
#[async_trait]
pub trait AgentLoader: Send + Sync {
    /// Load an agent for the given application name.
    async fn load_agent(&self, app_name: &str) -> Result<Arc<dyn crate::Agent>>;
}

/// Single agent loader that returns the same agent for all app names.
pub struct SingleAgentLoader {
    agent: Arc<dyn crate::Agent>,
}

impl SingleAgentLoader {
    pub fn new(agent: Arc<dyn crate::Agent>) -> Self {
        Self { agent }
    }
}

#[async_trait]
impl AgentLoader for SingleAgentLoader {
    async fn load_agent(&self, _app_name: &str) -> Result<Arc<dyn crate::Agent>> {
        Ok(self.agent.clone())
    }
}
