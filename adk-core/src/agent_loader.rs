use crate::{AdkError, Result};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;

/// Trait for loading agents by name.
#[async_trait]
pub trait AgentLoader: Send + Sync {
    /// Load an agent by name (or app_name for compatibility).
    async fn load_agent(&self, name: &str) -> Result<Arc<dyn crate::Agent>>;
    
    /// List all available agent names.
    fn list_agents(&self) -> Vec<String>;
    
    /// Get the root (default) agent.
    fn root_agent(&self) -> Arc<dyn crate::Agent>;
}

/// Single agent loader that returns the same agent for all names.
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
    async fn load_agent(&self, name: &str) -> Result<Arc<dyn crate::Agent>> {
        if name.is_empty() || name == self.agent.name() {
            Ok(self.agent.clone())
        } else {
            Err(AdkError::Config(format!(
                "Cannot load agent '{}' - use empty string or '{}'",
                name,
                self.agent.name()
            )))
        }
    }
    
    fn list_agents(&self) -> Vec<String> {
        vec![self.agent.name().to_string()]
    }
    
    fn root_agent(&self) -> Arc<dyn crate::Agent> {
        self.agent.clone()
    }
}

/// Multi-agent loader that manages multiple agents by name.
pub struct MultiAgentLoader {
    agent_map: HashMap<String, Arc<dyn crate::Agent>>,
    root: Arc<dyn crate::Agent>,
}

impl MultiAgentLoader {
    /// Create a new MultiAgentLoader with the given agents.
    /// The first agent becomes the root agent.
    /// Returns an error if duplicate agent names are found.
    pub fn new(agents: Vec<Arc<dyn crate::Agent>>) -> Result<Self> {
        if agents.is_empty() {
            return Err(AdkError::Config(
                "MultiAgentLoader requires at least one agent".to_string(),
            ));
        }

        let mut agent_map = HashMap::new();
        let root = agents[0].clone();

        for agent in agents {
            let name = agent.name().to_string();
            if agent_map.contains_key(&name) {
                return Err(AdkError::Config(format!(
                    "Duplicate agent name: {}",
                    name
                )));
            }
            agent_map.insert(name, agent);
        }

        Ok(Self { agent_map, root })
    }
}

#[async_trait]
impl AgentLoader for MultiAgentLoader {
    async fn load_agent(&self, name: &str) -> Result<Arc<dyn crate::Agent>> {
        if name.is_empty() {
            return Ok(self.root.clone());
        }

        self.agent_map
            .get(name)
            .cloned()
            .ok_or_else(|| {
                AdkError::Config(format!(
                    "Agent '{}' not found. Available agents: {:?}",
                    name,
                    self.list_agents()
                ))
            })
    }
    
    fn list_agents(&self) -> Vec<String> {
        self.agent_map.keys().cloned().collect()
    }
    
    fn root_agent(&self) -> Arc<dyn crate::Agent> {
        self.root.clone()
    }
}
