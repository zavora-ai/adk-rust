use std::{sync::Arc, time::Duration};

use crate::auth_bridge::RequestContextExtractor;
use adk_core::{CacheCapable, ContextCacheConfig, EventsCompactionConfig};

#[cfg(feature = "yaml-agent")]
use std::path::PathBuf;

/// Security configuration for the ADK server.
///
/// The defaults admit only same-origin browser traffic for loopback host names and IP
/// addresses. Each field below lists what to set when the server is reached another way.
///
/// # Example
///
/// ```rust
/// use adk_server::SecurityConfig;
///
/// let security = SecurityConfig {
///     allowed_origins: vec!["https://app.example.com".to_string()],
///     allowed_hosts: vec!["agents.example.com".to_string()],
///     ..SecurityConfig::default()
/// };
/// assert!(security.allows_host("agents.example.com:443"));
/// assert!(!security.allows_host("rebind.attacker.example"));
/// ```
#[derive(Clone, Debug)]
pub struct SecurityConfig {
    /// Browser origins allowed to call the server from another origin (CORS).
    ///
    /// Empty — the default — allows no cross-origin access: preflights get no
    /// `Access-Control-Allow-Origin`, and a request whose `Origin` names another origin is
    /// refused with `403`. Entries are exact origins (`https://app.example.com`), an origin
    /// with any port (`http://localhost:*`), or `*` for every origin.
    pub allowed_origins: Vec<String>,
    /// Host names the server answers, in addition to `localhost`, `*.localhost`, and IP
    /// addresses, which are always answered.
    ///
    /// A request whose `Host` names any other host is refused with `403`, so a page on a
    /// DNS-rebinding domain cannot reach the server through the browser. Entries are exact
    /// names (`agents.example.com`), a domain and its subdomains (`.example.com`), or `*`
    /// for every host. The host of an A2A base URL and of the backend URL configured on
    /// [`ServerConfig`] is added automatically. List the public host names here when the
    /// server listens beyond loopback.
    pub allowed_hosts: Vec<String>,
    /// Maximum request body size in bytes (default: 10MB)
    pub max_body_size: usize,
    /// Request timeout duration (default: 30 seconds). Agent runs take
    /// [`run_timeout`](Self::run_timeout) instead.
    pub request_timeout: Duration,
    /// How long a request that runs an agent may take before it is answered `408`: `POST
    /// /api/run`, `/api/run/{app}/{user}/{session}`, `/api/run_sse`, `/a2a`, and `/a2a/stream`
    /// (default: [`DEFAULT_RUN_TIMEOUT`], 15 minutes). A model turn with tool calls routinely
    /// outlasts [`request_timeout`](Self::request_timeout), so agent runs have their own
    /// limit. A streamed run is bounded until its response starts.
    pub run_timeout: Duration,
    /// Whether error responses carry every error's own message (default: `false`;
    /// [`SecurityConfig::development`] sets it).
    ///
    /// Off, a response carries an error's own message only when the server wrote it about the
    /// request. All three must hold:
    ///
    /// - the category rejects the request: `invalid_input`, `unauthorized`, `forbidden`,
    ///   `not_found`, `unsupported`, `conflict`, or `resource_exhausted`;
    /// - the component handles the request inside the server: `server`, `auth`, `session`,
    ///   `agent`, or `guardrail`;
    /// - the error carries no upstream status code and no provider.
    ///
    /// Every other message — a model provider's response, a tool's service, a store or
    /// driver failure — is replaced by a generic message for the error's category, such as
    /// `Internal server error` or `Authentication failed`, and the full error is logged with
    /// `tracing` inside the request's `http.request` span, whose `request.id` is the
    /// response's `x-request-id`. The rule applies to the run endpoints' Problem JSON (its
    /// `code`, `component`, `category`, `requestId`, `retryAfter`, and `upstreamStatusCode`
    /// are kept), the `error_message` of their SSE error event and the AG-UI `RUN_ERROR`
    /// `message`, and the `error` of a `503` from `POST /api/admin/freeze` or
    /// `/api/admin/unfreeze`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_server::SecurityConfig;
    ///
    /// assert!(!SecurityConfig::default().expose_error_details);
    /// assert!(SecurityConfig::development().expose_error_details);
    /// ```
    pub expose_error_details: bool,
    /// Whether to expose admin-only debug endpoints when auth is configured.
    pub expose_admin_debug: bool,
    /// The scope that makes an authenticated caller an administrator (default:
    /// [`ADMIN_SCOPE`], `"admin"`).
    ///
    /// `POST /api/admin/freeze` and `POST /api/admin/unfreeze`, mounted by
    /// [`ServerBuilder::enable_governance_endpoints`](crate::ServerBuilder::enable_governance_endpoints),
    /// admit only a caller whose [`RequestContext::scopes`](adk_core::RequestContext::scopes)
    /// include it: a caller without it is refused with `403`, and a request with no caller —
    /// a server without a `RequestContextExtractor` — with `401`.
    pub admin_scope: String,
}

/// The default [`SecurityConfig::admin_scope`]: the scope that makes a caller an
/// administrator of the governance endpoints.
///
/// # Example
///
/// ```rust
/// use adk_server::{ADMIN_SCOPE, SecurityConfig};
///
/// assert_eq!(SecurityConfig::default().admin_scope, ADMIN_SCOPE);
/// ```
pub const ADMIN_SCOPE: &str = "admin";

/// The [`SecurityConfig::run_timeout`] of every preset: 15 minutes, long enough for an agent
/// turn with several model and tool calls.
pub const DEFAULT_RUN_TIMEOUT: Duration = Duration::from_secs(15 * 60);

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            allowed_origins: Vec::new(),
            allowed_hosts: Vec::new(),
            max_body_size: 10 * 1024 * 1024, // 10MB
            request_timeout: Duration::from_secs(30),
            run_timeout: DEFAULT_RUN_TIMEOUT,
            expose_error_details: false,
            expose_admin_debug: false,
            admin_scope: ADMIN_SCOPE.to_string(),
        }
    }
}

/// Origins [`SecurityConfig::development`] admits: loopback on any port.
const LOOPBACK_ORIGINS: [&str; 6] = [
    "http://localhost:*",
    "https://localhost:*",
    "http://127.0.0.1:*",
    "https://127.0.0.1:*",
    "http://[::1]:*",
    "https://[::1]:*",
];

impl SecurityConfig {
    /// A development configuration: detailed errors, admin debug endpoints, and
    /// cross-origin access from loopback origins on any port, so a frontend dev server on
    /// `http://localhost:5173` can call the API.
    pub fn development() -> Self {
        Self {
            allowed_origins: LOOPBACK_ORIGINS.iter().map(|origin| (*origin).to_string()).collect(),
            allowed_hosts: Vec::new(),
            max_body_size: 10 * 1024 * 1024,
            request_timeout: Duration::from_secs(60),
            run_timeout: DEFAULT_RUN_TIMEOUT,
            expose_error_details: true,
            expose_admin_debug: true,
            admin_scope: ADMIN_SCOPE.to_string(),
        }
    }

    /// A production configuration admitting cross-origin calls from `allowed_origins` only.
    pub fn production(allowed_origins: Vec<String>) -> Self {
        Self {
            allowed_origins,
            allowed_hosts: Vec::new(),
            max_body_size: 10 * 1024 * 1024,
            request_timeout: Duration::from_secs(30),
            run_timeout: DEFAULT_RUN_TIMEOUT,
            expose_error_details: false,
            expose_admin_debug: false,
            admin_scope: ADMIN_SCOPE.to_string(),
        }
    }

    /// Sets the host names the server answers besides loopback names and IP addresses,
    /// replacing any list set before; see [`allowed_hosts`](Self::allowed_hosts).
    ///
    /// [`ServerConfig::with_allowed_hosts`] sets the same list the same way.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_server::SecurityConfig;
    ///
    /// let security = SecurityConfig::default()
    ///     .with_allowed_hosts(["staging.example.com"])
    ///     .with_allowed_hosts(["agents.example.com", "api.example.com"]);
    /// assert!(security.allows_host("agents.example.com"));
    /// assert!(!security.allows_host("staging.example.com"));
    /// ```
    pub fn with_allowed_hosts<I, S>(mut self, hosts: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.allowed_hosts = hosts.into_iter().map(Into::into).collect();
        self
    }

    /// Whether a request whose `Host` header (or URI authority) is `host` is answered.
    ///
    /// `localhost`, its subdomains, and IP addresses are always answered; any other name
    /// must match [`allowed_hosts`](Self::allowed_hosts). The port is ignored.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_server::SecurityConfig;
    ///
    /// let security = SecurityConfig::default();
    /// assert!(security.allows_host("127.0.0.1:8080"));
    /// assert!(security.allows_host("localhost:8080"));
    /// assert!(!security.allows_host("rebind.attacker.example:8080"));
    /// ```
    pub fn allows_host(&self, host: &str) -> bool {
        host_allowed(host, &self.allowed_hosts)
    }

    /// Whether a browser on `origin` may call the server from another origin.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_server::SecurityConfig;
    ///
    /// assert!(!SecurityConfig::default().allows_origin("https://evil.example"));
    /// assert!(SecurityConfig::development().allows_origin("http://localhost:5173"));
    /// ```
    pub fn allows_origin(&self, origin: &str) -> bool {
        self.allowed_origins.iter().any(|entry| origin_matches(entry, origin))
    }
}

/// The host part of a `Host` header or authority: no port, no IPv6 brackets, no trailing
/// dot, lowercase.
pub(crate) fn host_name(authority: &str) -> String {
    let authority = authority.trim();
    let host = if let Some(rest) = authority.strip_prefix('[') {
        rest.split(']').next().unwrap_or_default()
    } else {
        match authority.rsplit_once(':') {
            // A bare IPv6 address has more than one colon and no port.
            Some((host, port))
                if !host.contains(':') && port.bytes().all(|byte| byte.is_ascii_digit()) =>
            {
                host
            }
            _ => authority,
        }
    };
    host.trim_end_matches('.').to_ascii_lowercase()
}

fn host_allowed(authority: &str, allowed: &[String]) -> bool {
    let host = host_name(authority);
    if host.parse::<std::net::IpAddr>().is_ok()
        || host == "localhost"
        || host.ends_with(".localhost")
    {
        return true;
    }
    allowed.iter().any(|entry| {
        let entry = entry.trim();
        if entry == "*" {
            return true;
        }
        match entry.strip_prefix("*.").or_else(|| entry.strip_prefix('.')) {
            Some(domain) => {
                let domain = host_name(domain);
                host == domain || host.ends_with(&format!(".{domain}"))
            }
            None => host == host_name(entry),
        }
    })
}

fn origin_matches(entry: &str, origin: &str) -> bool {
    let entry = entry.trim().trim_end_matches('/');
    if entry == "*" || entry.eq_ignore_ascii_case(origin) {
        return true;
    }
    let Some(prefix) = entry.strip_suffix(":*") else {
        return false;
    };
    let Some(head) = origin.get(..prefix.len()) else {
        return false;
    };
    if !head.eq_ignore_ascii_case(prefix) {
        return false;
    }
    match &origin[prefix.len()..] {
        "" => true,
        rest => rest
            .strip_prefix(':')
            .is_some_and(|port| !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit())),
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
    /// Optional interceptor chain for A2A request/response middleware.
    ///
    /// When set, the chain is invoked before and after A2A executor processing.
    #[cfg(feature = "a2a-interceptors")]
    pub interceptor_chain: Option<Arc<crate::a2a::interceptor::InterceptorChain>>,
    /// Directories containing YAML agent definitions to watch for hot reload.
    /// Only used when the `yaml-agent` feature is enabled.
    #[cfg(feature = "yaml-agent")]
    pub yaml_agent_dirs: Vec<PathBuf>,
    /// Kill switch shared by every runner the server builds.
    ///
    /// Freezing it — directly or through `POST /api/admin/freeze` when
    /// [`ServerBuilder::enable_governance_endpoints`](crate::ServerBuilder::enable_governance_endpoints)
    /// is set — fails new runs and stops running ones before their next model or tool call.
    pub governance: adk_core::GovernanceControl,
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
            #[cfg(feature = "a2a-interceptors")]
            interceptor_chain: None,
            #[cfg(feature = "yaml-agent")]
            yaml_agent_dirs: Vec::new(),
            governance: adk_core::GovernanceControl::new(),
        }
    }

    /// Shares `control` with every runner the server builds.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let control = adk_core::GovernanceControl::new();
    /// let config = ServerConfig::new(loader, sessions).with_governance(control.clone());
    /// control.freeze("incident 4012");
    /// ```
    pub fn with_governance(mut self, control: adk_core::GovernanceControl) -> Self {
        self.governance = control;
        self
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

    /// Sets the browser origins allowed to call the server cross-origin; see
    /// [`SecurityConfig::allowed_origins`].
    pub fn with_allowed_origins(mut self, origins: Vec<String>) -> Self {
        self.security.allowed_origins = origins;
        self
    }

    /// Sets the host names the server answers besides loopback names and IP addresses,
    /// replacing any list set before, including one in a [`SecurityConfig`] passed to
    /// [`with_security`](Self::with_security); see [`SecurityConfig::allowed_hosts`].
    ///
    /// [`SecurityConfig::with_allowed_hosts`] sets the same list the same way.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let config = ServerConfig::new(loader, sessions)
    ///     .with_allowed_hosts(["agents.example.com", "api.example.com"]);
    /// ```
    pub fn with_allowed_hosts<I, S>(mut self, hosts: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.security = self.security.with_allowed_hosts(hosts);
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

    /// Sets how long a request that runs an agent may take; see
    /// [`SecurityConfig::run_timeout`].
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let config = ServerConfig::new(agent_loader, session_service)
    ///     .with_run_timeout(std::time::Duration::from_secs(30 * 60));
    /// ```
    pub fn with_run_timeout(mut self, timeout: Duration) -> Self {
        self.security.run_timeout = timeout;
        self
    }

    /// Sets whether error responses carry every error's own message; see
    /// [`SecurityConfig::expose_error_details`]. Enable it for development only: an upstream
    /// or store message can name hosts, tables, accounts, or request content.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let config = ServerConfig::new(agent_loader, session_service).with_error_details(true);
    /// ```
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

    /// Configure an interceptor chain for A2A request/response middleware.
    ///
    /// When set, the chain's `run_before` is called before the A2A executor
    /// processes a request, and `run_after` is called after it completes.
    /// Interceptors can reject, short-circuit, or modify requests and responses.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_server::a2a::interceptor::InterceptorChain;
    /// use std::sync::Arc;
    ///
    /// let chain = InterceptorChain::new()
    ///     .add(my_auth_interceptor)
    ///     .add(my_rate_limiter);
    ///
    /// let config = ServerConfig::new(loader, session)
    ///     .with_interceptor_chain(Arc::new(chain));
    /// ```
    #[cfg(feature = "a2a-interceptors")]
    pub fn with_interceptor_chain(
        mut self,
        chain: Arc<crate::a2a::interceptor::InterceptorChain>,
    ) -> Self {
        self.interceptor_chain = Some(chain);
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
    fn host_name_strips_port_brackets_and_trailing_dot() {
        assert_eq!(host_name("Agents.Example.com:8443"), "agents.example.com");
        assert_eq!(host_name("[::1]:8080"), "::1");
        assert_eq!(host_name("::1"), "::1");
        assert_eq!(host_name("localhost."), "localhost");
        assert_eq!(host_name("127.0.0.1:80"), "127.0.0.1");
    }

    #[test]
    fn default_security_answers_loopback_names_and_ip_addresses_only() {
        let security = SecurityConfig::default();
        for host in ["localhost:8080", "app.localhost", "127.0.0.1:8080", "[::1]:8080", "10.0.0.5"]
        {
            assert!(security.allows_host(host), "{host} should be answered");
        }
        for host in ["rebind.attacker.example:8080", "localhost.attacker.example", "example.com"] {
            assert!(!security.allows_host(host), "{host} should be refused");
        }
    }

    #[test]
    fn allowed_hosts_match_exact_names_domains_and_wildcard() {
        let exact = SecurityConfig::default().with_allowed_hosts(["agents.example.com"]);
        assert!(exact.allows_host("AGENTS.example.com:443"));
        assert!(!exact.allows_host("api.agents.example.com"));

        let domain = SecurityConfig::default().with_allowed_hosts([".example.com"]);
        assert!(domain.allows_host("example.com"));
        assert!(domain.allows_host("api.example.com"));
        assert!(!domain.allows_host("example.com.attacker.example"));
        assert!(!domain.allows_host("notexample.com"));

        let any = SecurityConfig::default().with_allowed_hosts(["*"]);
        assert!(any.allows_host("anything.example"));
    }

    #[test]
    fn allowed_hosts_setters_replace_the_list() {
        let security = SecurityConfig::default()
            .with_allowed_hosts(["old.example.com"])
            .with_allowed_hosts(["agents.example.com"]);
        assert_eq!(security.allowed_hosts, ["agents.example.com"]);
    }

    #[test]
    fn default_security_allows_no_cross_origin_access() {
        let security = SecurityConfig::default();
        assert!(!security.allows_origin("https://evil.example"));
        assert!(!security.allows_origin("http://localhost:5173"));
    }

    #[test]
    fn origin_entries_match_exact_any_port_and_wildcard() {
        let security = SecurityConfig::production(vec![
            "https://app.example.com".to_string(),
            "http://localhost:*".to_string(),
        ]);
        assert!(security.allows_origin("https://app.example.com"));
        assert!(!security.allows_origin("https://app.example.com.attacker.example"));
        assert!(security.allows_origin("http://localhost:5173"));
        assert!(security.allows_origin("http://localhost"));
        assert!(!security.allows_origin("http://localhost.attacker.example"));
        assert!(!security.allows_origin("http://localhost:"));
        assert!(!security.allows_origin("https://localhost:5173"));

        assert!(SecurityConfig::production(vec!["*".to_string()]).allows_origin("null"));
    }

    #[test]
    fn development_security_allows_loopback_origins_on_any_port() {
        let security = SecurityConfig::development();
        assert!(security.allows_origin("http://localhost:5173"));
        assert!(security.allows_origin("http://127.0.0.1:3000"));
        assert!(security.allows_origin("http://[::1]:8080"));
        assert!(!security.allows_origin("https://evil.example"));
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
