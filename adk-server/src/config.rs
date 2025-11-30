use std::sync::Arc;

/// Configuration for the ADK server.
#[derive(Clone)]
pub struct ServerConfig {
    pub agent_loader: Arc<dyn adk_core::AgentLoader>,
    pub session_service: Arc<dyn adk_session::SessionService>,
    pub artifact_service: Option<Arc<dyn adk_artifact::ArtifactService>>,
    pub backend_url: Option<String>,
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
            backend_url: None,
        }
    }

    pub fn with_artifact_service(mut self, artifact_service: Arc<dyn adk_artifact::ArtifactService>) -> Self {
        self.artifact_service = Some(artifact_service);
        self
    }

    pub fn with_backend_url(mut self, backend_url: impl Into<String>) -> Self {
        self.backend_url = Some(backend_url.into());
        self
    }
}
