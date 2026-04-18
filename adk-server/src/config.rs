use std::{sync::Arc, time::Duration};

use crate::auth_bridge::RequestContextExtractor;
use adk_core::{CacheCapable, ContextCacheConfig, EventsCompactionConfig};

#[cfg(feature = "yaml-agent")]
use std::path::PathBuf;

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
    /// Whether to expose admin-only debug endpoints when auth is configured.
    pub expose_admin_debug: bool,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            allowed_origins: Vec::new(), // Empty = permissive (for dev), should be configured for prod
            max_body_size: 10 * 1024 * 1024, // 10MB
            request_timeout: Duration::from_secs(30),
            expose_error_details: false,
            expose_admin_debug: false,
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
            expose_admin_debug: true,
        }
    }

    /// Create a production configuration with specific allowed origins
    pub fn production(allowed_origins: Vec<String>) -> Self {
        Self {
            allowed_origins,
            max_body_size: 10 * 1024 * 1024,
            request_timeout: Duration::from_secs(30),
            expose_error_details: false,
            expose_admin_debug: false,
        }
    }
}

/// Configuration for the ADK server.
#[derive(Clone)]
pub struct ServerConfig {
    pub agent_loader: Arc<dyn adk_core::AgentLoader>,
    pub session_service: Arc<dyn adk_session::SessionService>,
    pub artifact_service: Option<Arc<dyn adk_artifact::ArtifactService>>,
    pub memory_service: Option<Arc<dyn adk_core::Memory>>,
    pub compaction_config: Option<EventsCompactionConfig>,
    pub context_cache_config: Option<ContextCacheConfig>,
    pub cache_capable: Option<Arc<dyn CacheCapable>>,
    pub span_exporter: Option<Arc<adk_telemetry::AdkSpanExporter>>,
    pub backend_url: Option<String>,
    pub security: SecurityConfig,
    pub request_context_extractor: Option<Arc<dyn RequestContextExtractor>>,
    /// Directories containing YAML agent definitions to watch for hot reload.
    /// Only used when the `yaml-agent` feature is enabled.
    #[cfg(feature = "yaml-agent")]
    pub yaml_agent_dirs: Vec<PathBuf>,
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
            memory_service: None,
            compaction_config: None,
            context_cache_config: None,
            cache_capable: None,
            span_exporter: None,
            backend_url: None,
            security: SecurityConfig::default(),
            request_context_extractor: None,
            #[cfg(feature = "yaml-agent")]
            yaml_agent_dirs: Vec::new(),
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

    /// Configure a memory service for semantic search across sessions.
    ///
    /// When set, the runner injects memory into the invocation context,
    /// allowing agents to search previous conversation content via
    /// `ToolContext::search_memory()`.
    pub fn with_memory_service(mut self, memory_service: Arc<dyn adk_core::Memory>) -> Self {
        self.memory_service = Some(memory_service);
        self
    }

    /// Configure automatic context compaction for long-running sessions.
    pub fn with_compaction(mut self, compaction_config: EventsCompactionConfig) -> Self {
        self.compaction_config = Some(compaction_config);
        self
    }

    /// Configure automatic prompt-cache lifecycle management for cache-capable models.
    pub fn with_context_cache(
        mut self,
        context_cache_config: ContextCacheConfig,
        cache_capable: Arc<dyn CacheCapable>,
    ) -> Self {
        self.context_cache_config = Some(context_cache_config);
        self.cache_capable = Some(cache_capable);
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

    /// Configure a request context extractor for auth middleware bridging.
    ///
    /// When set, the server invokes the extractor on each incoming request
    /// to extract authenticated identity (user_id, scopes, metadata) from
    /// HTTP headers. The extracted context flows into `InvocationContext`,
    /// making scopes available via `ToolContext::user_scopes()`.
    pub fn with_request_context(mut self, extractor: Arc<dyn RequestContextExtractor>) -> Self {
        self.request_context_extractor = Some(extractor);
        self
    }

    /// Configure directories containing YAML agent definitions to watch.
    ///
    /// When the `yaml-agent` feature is enabled and directories are configured,
    /// the server starts a [`HotReloadWatcher`](crate::yaml_agent::HotReloadWatcher)
    /// for each directory at startup, automatically loading and hot-reloading
    /// YAML-defined agents.
    #[cfg(feature = "yaml-agent")]
    pub fn with_yaml_agent_dirs(mut self, dirs: Vec<PathBuf>) -> Self {
        self.yaml_agent_dirs = dirs;
        self
    }

    /// Add a single YAML agent directory to watch.
    ///
    /// Convenience method that appends one directory to the list.
    #[cfg(feature = "yaml-agent")]
    pub fn with_yaml_agent_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.yaml_agent_dirs.push(dir.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::{
        Agent, BaseEventsSummarizer, Event, EventStream, InvocationContext, Result as AdkResult,
        SingleAgentLoader,
    };
    use adk_session::InMemorySessionService;
    use async_trait::async_trait;
    use futures::stream;

    struct TestAgent;

    #[async_trait]
    impl Agent for TestAgent {
        fn name(&self) -> &str {
            "server_config_test_agent"
        }

        fn description(&self) -> &str {
            "server config test agent"
        }

        fn sub_agents(&self) -> &[Arc<dyn Agent>] {
            &[]
        }

        async fn run(&self, _ctx: Arc<dyn InvocationContext>) -> AdkResult<EventStream> {
            Ok(Box::pin(stream::empty()))
        }
    }

    struct TestCache;

    struct TestSummarizer;

    #[async_trait]
    impl CacheCapable for TestCache {
        async fn create_cache(
            &self,
            _system_instruction: &str,
            _tools: &std::collections::HashMap<String, serde_json::Value>,
            _ttl_seconds: u32,
        ) -> adk_core::Result<String> {
            Ok("cache".to_string())
        }

        async fn delete_cache(&self, _name: &str) -> adk_core::Result<()> {
            Ok(())
        }
    }

    #[async_trait]
    impl BaseEventsSummarizer for TestSummarizer {
        async fn summarize_events(&self, _events: &[Event]) -> adk_core::Result<Option<Event>> {
            Ok(Some(Event::new("summary")))
        }
    }

    fn test_config() -> ServerConfig {
        let agent_loader = Arc::new(SingleAgentLoader::new(Arc::new(TestAgent)));
        let session_service = Arc::new(InMemorySessionService::new());
        ServerConfig::new(agent_loader, session_service)
    }

    #[test]
    fn with_compaction_sets_optional_config() {
        let compaction_config = EventsCompactionConfig {
            compaction_interval: 10,
            overlap_size: 2,
            summarizer: Arc::new(TestSummarizer),
        };

        let config = test_config().with_compaction(compaction_config.clone());

        assert!(config.compaction_config.is_some());
        assert_eq!(config.compaction_config.as_ref().unwrap().compaction_interval, 10);
        assert_eq!(config.compaction_config.as_ref().unwrap().overlap_size, 2);
    }

    #[test]
    fn with_context_cache_sets_cache_fields() {
        let context_cache_config =
            ContextCacheConfig { min_tokens: 512, ttl_seconds: 300, cache_intervals: 2 };
        let cache_capable = Arc::new(TestCache);

        let config =
            test_config().with_context_cache(context_cache_config.clone(), cache_capable.clone());

        assert_eq!(config.context_cache_config.as_ref().unwrap().min_tokens, 512);
        assert_eq!(config.context_cache_config.as_ref().unwrap().ttl_seconds, 300);
        assert_eq!(config.context_cache_config.as_ref().unwrap().cache_intervals, 2);
        assert!(config.cache_capable.is_some());
        let configured = config.cache_capable.as_ref().unwrap();
        let expected: Arc<dyn CacheCapable> = cache_capable;
        assert!(Arc::ptr_eq(configured, &expected));
    }
}
