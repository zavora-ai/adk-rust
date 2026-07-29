//! Convenience API for quickly starting an A2A-capable server.
//!
//! This module provides [`A2aServer`], a high-level wrapper that hides the
//! complexity of wiring up A2A routes, agent cards, and session services.
//!
//! # Quick Start
//!
//! ```rust,ignore
//! use adk_server::a2a::convenience::A2aServer;
//! use std::sync::Arc;
//!
//! let agent: Arc<dyn adk_core::Agent> = /* your agent */;
//! let app = A2aServer::quick_start(agent);
//! // Serve with axum::serve(listener, app).await
//! ```
//!
//! # Builder
//!
//! ```rust,ignore
//! use adk_server::a2a::convenience::A2aServer;
//! use std::sync::Arc;
//!
//! let server = A2aServer::builder()
//!     .agent(my_agent)
//!     .bind_addr("0.0.0.0:9090")
//!     .agent_card_name("My Agent")
//!     .streaming(true)
//!     .build()?;
//!
//! server.serve().await?;
//! ```

use std::sync::Arc;

use adk_core::{AdkError, Agent, ErrorCategory, ErrorComponent, SingleAgentLoader};
use adk_session::{InMemorySessionService, SessionService};
use axum::Router;

use crate::a2a::types::{AgentCapabilities, AgentCard};
use crate::config::ServerConfig;
use crate::rest::create_app_with_a2a;

/// Convenience wrapper for quickly starting an A2A-capable server.
///
/// Provides two entry points:
/// - [`A2aServer::quick_start`] for zero-config usage
/// - [`A2aServer::builder`] for custom configuration
///
/// # Example
///
/// ```rust,ignore
/// use adk_server::a2a::convenience::A2aServer;
/// use std::sync::Arc;
///
/// let app = A2aServer::quick_start(my_agent);
/// let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
/// axum::serve(listener, app).await?;
/// ```
pub struct A2aServer;

impl A2aServer {
    /// Create an A2A-ready Axum app with sensible defaults.
    ///
    /// Uses an in-memory session service, binds to `http://localhost:8080`,
    /// enables streaming, and auto-generates the agent card from agent metadata.
    ///
    /// # Arguments
    ///
    /// * `agent` - The agent to expose via A2A protocol
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_server::a2a::convenience::A2aServer;
    /// use std::sync::Arc;
    ///
    /// let app = A2aServer::quick_start(my_agent);
    /// let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
    /// axum::serve(listener, app).await?;
    /// ```
    pub fn quick_start(agent: Arc<dyn Agent>) -> Router {
        let base_url = "http://localhost:8080";
        let session_service: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
        let agent_loader = Arc::new(SingleAgentLoader::new(agent));

        let config = ServerConfig::new(agent_loader, session_service);
        create_app_with_a2a(config, Some(base_url))
    }

    /// Create a builder for custom A2A server configuration.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_server::a2a::convenience::A2aServer;
    /// use std::sync::Arc;
    ///
    /// let server = A2aServer::builder()
    ///     .agent(my_agent)
    ///     .bind_addr("0.0.0.0:9090")
    ///     .streaming(false)
    ///     .build()?;
    ///
    /// server.serve().await?;
    /// ```
    pub fn builder() -> A2aServerBuilder {
        A2aServerBuilder::default()
    }
}

/// Builder for configuring an A2A server with custom settings.
///
/// Allows customization of the agent card metadata, session service,
/// bind address, and capabilities before building the server app.
///
/// # Defaults
///
/// - Session service: [`InMemorySessionService`]
/// - Bind address: `127.0.0.1:8080` (loopback; call `bind_addr` to expose it)
/// - Agent card name: from `agent.name()`
/// - Agent card description: from `agent.description()`
/// - Agent card version: `"1.0.0"`
/// - Agent card URL: `http://localhost:8080`
/// - Streaming: enabled
/// - Push notifications: disabled
pub struct A2aServerBuilder {
    agent: Option<Arc<dyn Agent>>,
    session_service: Option<Arc<dyn SessionService>>,
    bind_addr: String,
    agent_card_name: Option<String>,
    agent_card_description: Option<String>,
    agent_card_version: Option<String>,
    agent_card_url: Option<String>,
    streaming_enabled: bool,
    push_notifications_enabled: bool,
}

impl Default for A2aServerBuilder {
    fn default() -> Self {
        Self {
            agent: None,
            session_service: None,
            // Loopback, not every interface. An A2A server executes agent and tool work, so
            // exposing it beyond this host is a decision the operator should make explicitly
            // — the previous default published it to the network on `build()`.
            bind_addr: "127.0.0.1:8080".to_string(),
            agent_card_name: None,
            agent_card_description: None,
            agent_card_version: None,
            agent_card_url: None,
            streaming_enabled: true,
            push_notifications_enabled: false,
        }
    }
}

impl A2aServerBuilder {
    /// Set the agent to expose via A2A protocol.
    ///
    /// This is required — the builder will return an error if no agent is set.
    pub fn agent(mut self, agent: Arc<dyn Agent>) -> Self {
        self.agent = Some(agent);
        self
    }

    /// Set a custom session service implementation.
    ///
    /// Defaults to [`InMemorySessionService`] if not specified.
    pub fn session_service(mut self, service: Arc<dyn SessionService>) -> Self {
        self.session_service = Some(service);
        self
    }

    /// Set the bind address for the server.
    ///
    /// Defaults to `"0.0.0.0:8080"`.
    pub fn bind_addr(mut self, addr: impl Into<String>) -> Self {
        self.bind_addr = addr.into();
        self
    }

    /// Override the agent card name.
    ///
    /// Defaults to `agent.name()`.
    pub fn agent_card_name(mut self, name: impl Into<String>) -> Self {
        self.agent_card_name = Some(name.into());
        self
    }

    /// Override the agent card description.
    ///
    /// Defaults to `agent.description()`.
    pub fn agent_card_description(mut self, desc: impl Into<String>) -> Self {
        self.agent_card_description = Some(desc.into());
        self
    }

    /// Override the agent card version.
    ///
    /// Defaults to `"1.0.0"`.
    pub fn agent_card_version(mut self, version: impl Into<String>) -> Self {
        self.agent_card_version = Some(version.into());
        self
    }

    /// Override the agent card URL.
    ///
    /// Defaults to `"http://localhost:8080"`.
    pub fn agent_card_url(mut self, url: impl Into<String>) -> Self {
        self.agent_card_url = Some(url.into());
        self
    }

    /// Enable or disable streaming support.
    ///
    /// Defaults to `true`.
    pub fn streaming(mut self, enabled: bool) -> Self {
        self.streaming_enabled = enabled;
        self
    }

    /// Enable or disable push notifications.
    ///
    /// Defaults to `false`.
    pub fn push_notifications(mut self, enabled: bool) -> Self {
        self.push_notifications_enabled = enabled;
        self
    }

    /// Build the configured A2A server application.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - No agent was provided (call `.agent(my_agent)`)
    /// - The agent has an empty name
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let server = A2aServer::builder()
    ///     .agent(my_agent)
    ///     .build()?;
    /// ```
    pub fn build(self) -> Result<A2aServerApp, AdkError> {
        let agent = self.agent.ok_or_else(|| {
            AdkError::new(
                ErrorComponent::Server,
                ErrorCategory::InvalidInput,
                "server.a2a.missing_agent",
                "A2aServer requires an agent. Call .agent(my_agent) on the builder.",
            )
        })?;

        if agent.name().is_empty() {
            return Err(AdkError::new(
                ErrorComponent::Server,
                ErrorCategory::InvalidInput,
                "server.a2a.missing_agent_name",
                "A2A server requires an agent with a non-empty name. Set a name via LlmAgentBuilder::new(\"my-agent\")",
            ));
        }

        let session_service: Arc<dyn SessionService> =
            self.session_service.unwrap_or_else(|| Arc::new(InMemorySessionService::new()));

        let base_url = self
            .agent_card_url
            .unwrap_or_else(|| format!("http://localhost:{}", extract_port(&self.bind_addr)));

        let agent_loader = Arc::new(SingleAgentLoader::new(agent.clone()));
        let config = ServerConfig::new(agent_loader, session_service);

        // Build a custom agent card if any overrides were specified
        let _agent_card = build_custom_agent_card(
            agent.as_ref(),
            &base_url,
            self.agent_card_name,
            self.agent_card_description,
            self.agent_card_version,
            self.streaming_enabled,
            self.push_notifications_enabled,
        );

        let router = create_app_with_a2a(config, Some(&base_url));

        Ok(A2aServerApp { router, bind_addr: self.bind_addr })
    }
}

/// A configured A2A server application ready to serve.
///
/// Created via [`A2aServerBuilder::build`]. Provides access to the
/// underlying Axum router and a convenience [`serve`](A2aServerApp::serve)
/// method for binding and listening.
pub struct A2aServerApp {
    router: Router,
    bind_addr: String,
}

impl std::fmt::Debug for A2aServerApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("A2aServerApp").field("bind_addr", &self.bind_addr).finish_non_exhaustive()
    }
}

impl A2aServerApp {
    /// Consume the app and return the underlying Axum router.
    ///
    /// Use this when you need to compose the A2A routes with other
    /// Axum routers or middleware.
    pub fn into_router(self) -> Router {
        self.router
    }

    /// Get the configured bind address.
    pub fn bind_addr(&self) -> &str {
        &self.bind_addr
    }

    /// Start serving the A2A application.
    ///
    /// Binds to the configured address and serves until the process is
    /// terminated.
    ///
    /// # Errors
    ///
    /// Returns an error if the port is already in use or binding fails.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let server = A2aServer::builder()
    ///     .agent(my_agent)
    ///     .build()?;
    ///
    /// server.serve().await?;
    /// ```
    pub async fn serve(self) -> Result<(), AdkError> {
        let listener =
            tokio::net::TcpListener::bind(&self.bind_addr).await.map_err(|e| {
                let port = extract_port(&self.bind_addr);
                let alt_port = port + 1;
                AdkError::new(
                    ErrorComponent::Server,
                    ErrorCategory::Unavailable,
                    "server.a2a.port_in_use",
                    format!(
                        "Port {port} is already in use. Try a different port with .bind_addr(\"0.0.0.0:{alt_port}\")"
                    ),
                )
                .with_source(e)
            })?;

        axum::serve(listener, self.router).await.map_err(|e| {
            AdkError::new(
                ErrorComponent::Server,
                ErrorCategory::Internal,
                "server.a2a.serve_failed",
                format!("A2A server encountered an error: {e}"),
            )
            .with_source(e)
        })
    }
}

/// Build a custom agent card with optional overrides.
fn build_custom_agent_card(
    agent: &dyn Agent,
    base_url: &str,
    name_override: Option<String>,
    description_override: Option<String>,
    version_override: Option<String>,
    streaming: bool,
    push_notifications: bool,
) -> AgentCard {
    let name = name_override.unwrap_or_else(|| agent.name().to_string());
    let description = description_override.unwrap_or_else(|| agent.description().to_string());
    let version = version_override.unwrap_or_else(|| "1.0.0".to_string());

    let skills = crate::a2a::agent_card::build_agent_skills(agent);

    AgentCard::builder()
        .name(name)
        .description(description)
        .url(base_url.to_string())
        .version(version)
        .capabilities(AgentCapabilities {
            streaming,
            push_notifications,
            state_transition_history: true,
            extensions: None,
        })
        .skills(skills)
        .build()
        .expect("build_custom_agent_card: name, description, and url must be non-empty")
}

/// Extract the port number from a bind address string.
///
/// Handles formats like `"0.0.0.0:8080"`, `":8080"`, or `"8080"`.
fn extract_port(addr: &str) -> u16 {
    addr.rsplit(':').next().and_then(|p| p.parse().ok()).unwrap_or(8080)
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::{Agent, EventStream, InvocationContext, Result as AdkResult};
    use async_trait::async_trait;
    use futures::stream;

    struct TestAgent {
        name: String,
        description: String,
    }

    impl TestAgent {
        fn new(name: &str, description: &str) -> Self {
            Self { name: name.to_string(), description: description.to_string() }
        }
    }

    #[async_trait]
    impl Agent for TestAgent {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            &self.description
        }

        fn sub_agents(&self) -> &[Arc<dyn Agent>] {
            &[]
        }

        async fn run(&self, _ctx: Arc<dyn InvocationContext>) -> AdkResult<EventStream> {
            Ok(Box::pin(stream::empty()))
        }
    }

    #[test]
    fn test_quick_start_returns_router() {
        let agent: Arc<dyn Agent> = Arc::new(TestAgent::new("test-agent", "A test agent"));
        let _router = A2aServer::quick_start(agent);
    }

    #[test]
    fn test_builder_defaults() {
        let builder = A2aServer::builder();
        // Loopback, deliberately. An A2A server runs agent and tool work, so publishing it
        // to every interface is an explicit `bind_addr` call, not a default.
        assert_eq!(builder.bind_addr, "127.0.0.1:8080");
        assert!(builder.streaming_enabled);
        assert!(!builder.push_notifications_enabled);
        assert!(builder.agent.is_none());
        assert!(builder.session_service.is_none());
        assert!(builder.agent_card_name.is_none());
        assert!(builder.agent_card_description.is_none());
        assert!(builder.agent_card_version.is_none());
        assert!(builder.agent_card_url.is_none());
    }

    #[test]
    fn test_builder_missing_agent_error() {
        let result = A2aServer::builder().build();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.code, "server.a2a.missing_agent");
        assert!(err.message.contains("agent"));
    }

    #[test]
    fn test_builder_empty_agent_name_error() {
        let agent: Arc<dyn Agent> = Arc::new(TestAgent::new("", "A test agent"));
        let result = A2aServer::builder().agent(agent).build();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.code, "server.a2a.missing_agent_name");
        assert!(err.message.contains("non-empty name"));
    }

    #[test]
    fn test_builder_with_valid_agent_succeeds() {
        let agent: Arc<dyn Agent> = Arc::new(TestAgent::new("my-agent", "My agent description"));
        let result = A2aServer::builder().agent(agent).build();
        assert!(result.is_ok());
        let app = result.unwrap();
        assert_eq!(app.bind_addr(), "127.0.0.1:8080", "the default must stay on loopback");
    }

    #[test]
    fn test_builder_custom_bind_addr() {
        let agent: Arc<dyn Agent> = Arc::new(TestAgent::new("my-agent", "My agent"));
        let result = A2aServer::builder().agent(agent).bind_addr("127.0.0.1:9090").build();
        assert!(result.is_ok());
        let app = result.unwrap();
        assert_eq!(app.bind_addr(), "127.0.0.1:9090");
    }

    #[test]
    fn test_builder_into_router() {
        let agent: Arc<dyn Agent> = Arc::new(TestAgent::new("my-agent", "My agent"));
        let app = A2aServer::builder().agent(agent).build().unwrap();
        let _router = app.into_router();
    }

    #[test]
    fn test_extract_port() {
        assert_eq!(extract_port("0.0.0.0:8080"), 8080);
        assert_eq!(extract_port("127.0.0.1:9090"), 9090);
        assert_eq!(extract_port(":3000"), 3000);
        assert_eq!(extract_port("invalid"), 8080);
    }

    #[test]
    fn test_builder_all_options() {
        let agent: Arc<dyn Agent> = Arc::new(TestAgent::new("my-agent", "My agent"));
        let session_service: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());

        let result = A2aServer::builder()
            .agent(agent)
            .session_service(session_service)
            .bind_addr("0.0.0.0:3000")
            .agent_card_name("Custom Name")
            .agent_card_description("Custom description")
            .agent_card_version("2.0.0")
            .agent_card_url("https://my-agent.example.com")
            .streaming(false)
            .push_notifications(true)
            .build();

        assert!(result.is_ok());
        let app = result.unwrap();
        assert_eq!(app.bind_addr(), "0.0.0.0:3000");
    }
}
