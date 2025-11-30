use std::{sync::Arc, time::Duration};

/// Security configuration for the ADK server.
#[derive(Clone, Debug)]
pub struct SecurityConfig {
    /// Allowed origins for CORS (empty = allow all, which is NOT recommended for production)
    pub allowed_origins: Vec<String>,
    /// Maximum request body size in bytes (default: 10MB)
    pub max_body_size: usize,
    /// Request timeout duration (default: 30 seconds)
    pub request_timeout: Duration,
    /// Whether to include detailed error messages in responses (default: false for production)
    pub expose_error_details: bool,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            allowed_origins: Vec::new(), // Empty = permissive (for dev), should be configured for prod
            max_body_size: 10 * 1024 * 1024, // 10MB
            request_timeout: Duration::from_secs(30),
            expose_error_details: false,
        }
    }
}

impl SecurityConfig {
    /// Create a development configuration (permissive CORS, detailed errors)
    pub fn development() -> Self {
        Self {
            allowed_origins: Vec::new(),
            max_body_size: 10 * 1024 * 1024,
            request_timeout: Duration::from_secs(60),
            expose_error_details: true,
        }
    }

    /// Create a production configuration with specific allowed origins
    pub fn production(allowed_origins: Vec<String>) -> Self {
        Self {
            allowed_origins,
            max_body_size: 10 * 1024 * 1024,
            request_timeout: Duration::from_secs(30),
            expose_error_details: false,
        }
    }
}

/// Configuration for the ADK server.
#[derive(Clone)]
pub struct ServerConfig {
    pub agent_loader: Arc<dyn adk_core::AgentLoader>,
    pub session_service: Arc<dyn adk_session::SessionService>,
    pub artifact_service: Option<Arc<dyn adk_artifact::ArtifactService>>,
    pub backend_url: Option<String>,
    pub security: SecurityConfig,
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
            security: SecurityConfig::default(),
        }
    }

    pub fn with_artifact_service(
        mut self,
        artifact_service: Arc<dyn adk_artifact::ArtifactService>,
    ) -> Self {
        self.artifact_service = Some(artifact_service);
        self
    }

    pub fn with_artifact_service_opt(
        mut self,
        artifact_service: Option<Arc<dyn adk_artifact::ArtifactService>>,
    ) -> Self {
        self.artifact_service = artifact_service;
        self
    }

    pub fn with_backend_url(mut self, backend_url: impl Into<String>) -> Self {
        self.backend_url = Some(backend_url.into());
        self
    }

    pub fn with_security(mut self, security: SecurityConfig) -> Self {
        self.security = security;
        self
    }

    /// Configure allowed CORS origins
    pub fn with_allowed_origins(mut self, origins: Vec<String>) -> Self {
        self.security.allowed_origins = origins;
        self
    }

    /// Configure maximum request body size
    pub fn with_max_body_size(mut self, size: usize) -> Self {
        self.security.max_body_size = size;
        self
    }

    /// Configure request timeout
    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.security.request_timeout = timeout;
        self
    }

    /// Enable detailed error messages (for development only)
    pub fn with_error_details(mut self, expose: bool) -> Self {
        self.security.expose_error_details = expose;
        self
    }
}
