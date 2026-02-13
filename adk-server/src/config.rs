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
    pub span_exporter: Option<Arc<adk_telemetry::AdkSpanExporter>>,
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
            span_exporter: None,
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

    pub fn with_span_exporter(
        mut self,
        span_exporter: Arc<adk_telemetry::AdkSpanExporter>,
    ) -> Self {
        self.span_exporter = Some(span_exporter);
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

#[cfg(test)]
mod tests {
    use super::*;
    use adk_agent::CustomAgent;
    use adk_artifact::InMemoryArtifactService;
    use adk_core::SingleAgentLoader;
    use adk_session::InMemorySessionService;
    use adk_telemetry::AdkSpanExporter;
    use futures;
    use std::sync::Arc;
    use std::time::Duration;

    const DEFAULT_MAX_BODY_SIZE: usize = 10 * 1024 * 1024;
    const PROD_TIMEOUT: u64 = 30;
    const DEV_TIMEOUT: u64 = 60;

    fn create_mocks() -> (Arc<SingleAgentLoader>, Arc<InMemorySessionService>) {
        let agent = CustomAgent::builder("test-agent")
            .handler(|_| async { Ok(Box::pin(futures::stream::empty()) as adk_core::EventStream) })
            .build()
            .unwrap();
        let agent_loader = Arc::new(SingleAgentLoader::new(Arc::new(agent)));
        let session_service = Arc::new(InMemorySessionService::new());
        (agent_loader, session_service)
    }

    #[test]
    fn test_security_config_constructors() {
        let default = SecurityConfig::default();
        assert_eq!(default.allowed_origins.len(), 0);
        assert_eq!(default.max_body_size, DEFAULT_MAX_BODY_SIZE);
        assert_eq!(default.request_timeout, Duration::from_secs(PROD_TIMEOUT));
        assert!(!default.expose_error_details);

        let dev = SecurityConfig::development();
        assert_eq!(dev.allowed_origins.len(), 0);
        assert_eq!(dev.max_body_size, DEFAULT_MAX_BODY_SIZE);
        assert_eq!(dev.request_timeout, Duration::from_secs(DEV_TIMEOUT));
        assert!(dev.expose_error_details);

        let prod = SecurityConfig::production(vec!["https://example.com".to_string()]);
        assert_eq!(prod.allowed_origins, vec!["https://example.com"]);
        assert_eq!(prod.max_body_size, DEFAULT_MAX_BODY_SIZE);
        assert_eq!(prod.request_timeout, Duration::from_secs(PROD_TIMEOUT));
        assert!(!prod.expose_error_details);
    }

    #[test]
    fn test_server_config_new() {
        let (agent_loader, session_service) = create_mocks();
        let config = ServerConfig::new(agent_loader.clone(), session_service.clone());

        assert!(config.artifact_service.is_none());
        assert!(config.span_exporter.is_none());
        assert!(config.backend_url.is_none());
    }

    #[test]
    fn test_server_config_builder() {
        let (agent_loader, session_service) = create_mocks();
        let artifact_service = Arc::new(InMemoryArtifactService::new());
        let span_exporter = Arc::new(AdkSpanExporter::new());
        let security = SecurityConfig::development();

        let config = ServerConfig::new(agent_loader, session_service)
            .with_artifact_service(artifact_service.clone())
            .with_backend_url("http://backend")
            .with_security(security.clone())
            .with_span_exporter(span_exporter.clone());

        assert!(config.artifact_service.is_some());
        assert_eq!(config.backend_url, Some("http://backend".to_string()));
        assert!(config.security.expose_error_details);
        assert!(config.span_exporter.is_some());

        let config_opt = config.with_artifact_service_opt(None);
        assert!(config_opt.artifact_service.is_none());
    }

    #[test]
    fn test_server_config_security_passthrough() {
        let (agent_loader, session_service) = create_mocks();
        let config = ServerConfig::new(agent_loader, session_service)
            .with_allowed_origins(vec!["test".into()])
            .with_max_body_size(100)
            .with_request_timeout(Duration::from_secs(10))
            .with_error_details(true);

        assert_eq!(config.security.allowed_origins, vec!["test"]);
        assert_eq!(config.security.max_body_size, 100);
        assert_eq!(config.security.request_timeout, Duration::from_secs(10));
        assert!(config.security.expose_error_details);
    }
}
