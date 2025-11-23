use std::sync::Arc;

/// Configuration for the ADK server.
#[derive(Clone)]
pub struct ServerConfig {
    pub agent_loader: Arc<dyn adk_core::AgentLoader>,
    pub session_service: Arc<dyn adk_session::SessionService>,
    pub artifact_service: Option<Arc<dyn adk_core::Artifacts>>,
}

impl ServerConfig {
    pub fn new(
        agent_loader: Arc<dyn adk_core::AgentLoader>,
        session_service: Arc<dyn adk_session::SessionService>,
    ) -> Self {
        Self {
            agent_loader,
            session_service,
            artifact_service: None,
        }
    }

    pub fn with_artifact_service(mut self, artifact_service: Arc<dyn adk_core::Artifacts>) -> Self {
        self.artifact_service = Some(artifact_service);
        self
    }
}
