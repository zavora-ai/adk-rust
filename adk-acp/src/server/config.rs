//! Configuration for the ACP Server.
//!
//! Use [`AcpServerConfigBuilder`] to construct a validated [`AcpServerConfig`].
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_acp::server::{AcpServerConfig, AcpServerConfigBuilder, TransportConfig};
//! use std::sync::Arc;
//! use std::time::Duration;
//!
//! let config = AcpServerConfigBuilder::new()
//!     .agent(my_agent)
//!     .session_service(my_session_service)
//!     .agent_name("my-agent")
//!     .streaming(true)
//!     .build()?;
//! ```

use std::sync::Arc;
use std::time::Duration;

use adk_core::Agent;
use adk_session::SessionService;

use super::error::AcpServerError;

/// Transport selection for the ACP Server.
#[derive(Clone, Debug, Default)]
pub enum TransportConfig {
    /// Stdio transport (newline-delimited JSON on stdin/stdout).
    #[default]
    Stdio,
    /// HTTP transport with SSE streaming.
    Http {
        /// Address to bind to (e.g., "127.0.0.1").
        bind_address: String,
        /// Port to listen on.
        port: u16,
    },
}

/// Configuration for the ACP Server.
///
/// Created via [`AcpServerConfigBuilder`]. Contains all settings needed
/// to start an ACP server exposing an ADK agent.
#[derive(Clone)]
pub struct AcpServerConfig {
    /// The ADK agent to expose via ACP.
    pub agent: Arc<dyn Agent>,
    /// Session service for persistence.
    pub session_service: Arc<dyn SessionService>,
    /// Agent name advertised in capabilities.
    pub agent_name: String,
    /// Agent description advertised in capabilities.
    pub agent_description: String,
    /// Whether the agent supports streaming responses.
    pub streaming: bool,
    /// Whether the agent supports tool use.
    pub tool_use: bool,
    /// List of tool names the agent can use.
    pub tool_names: Vec<String>,
    /// Maximum concurrent sessions allowed.
    pub max_sessions: usize,
    /// Timeout for permission requests from the client.
    pub permission_timeout: Duration,
    /// Graceful shutdown timeout.
    pub shutdown_timeout: Duration,
    /// Transport configuration.
    pub transport: TransportConfig,
}

/// Builder for [`AcpServerConfig`] with validation.
///
/// # Example
///
/// ```rust,ignore
/// let config = AcpServerConfigBuilder::new()
///     .agent(agent)
///     .session_service(session_svc)
///     .agent_name("my-agent")
///     .build()?;
/// ```
pub struct AcpServerConfigBuilder {
    agent: Option<Arc<dyn Agent>>,
    session_service: Option<Arc<dyn SessionService>>,
    agent_name: String,
    agent_description: String,
    streaming: bool,
    tool_use: bool,
    tool_names: Vec<String>,
    max_sessions: usize,
    permission_timeout: Duration,
    shutdown_timeout: Duration,
    transport: TransportConfig,
}

impl Default for AcpServerConfigBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl AcpServerConfigBuilder {
    /// Create a new builder with sensible defaults.
    pub fn new() -> Self {
        Self {
            agent: None,
            session_service: None,
            agent_name: "adk-agent".to_string(),
            agent_description: String::new(),
            streaming: true,
            tool_use: false,
            tool_names: Vec::new(),
            max_sessions: 16,
            permission_timeout: Duration::from_secs(120),
            shutdown_timeout: Duration::from_secs(30),
            transport: TransportConfig::Stdio,
        }
    }

    /// Set the ADK agent to expose via ACP (required).
    pub fn agent(mut self, agent: Arc<dyn Agent>) -> Self {
        self.agent = Some(agent);
        self
    }

    /// Set the session service for persistence (required).
    pub fn session_service(mut self, svc: Arc<dyn SessionService>) -> Self {
        self.session_service = Some(svc);
        self
    }

    /// Set the agent name advertised in capabilities.
    pub fn agent_name(mut self, name: impl Into<String>) -> Self {
        self.agent_name = name.into();
        self
    }

    /// Set the agent description advertised in capabilities.
    pub fn agent_description(mut self, desc: impl Into<String>) -> Self {
        self.agent_description = desc.into();
        self
    }

    /// Set whether the agent supports streaming responses.
    pub fn streaming(mut self, enabled: bool) -> Self {
        self.streaming = enabled;
        self
    }

    /// Set whether the agent supports tool use.
    pub fn tool_use(mut self, enabled: bool) -> Self {
        self.tool_use = enabled;
        self
    }

    /// Set the list of tool names the agent can use.
    pub fn tool_names(mut self, names: Vec<String>) -> Self {
        self.tool_names = names;
        self
    }

    /// Set the maximum number of concurrent sessions.
    pub fn max_sessions(mut self, max: usize) -> Self {
        self.max_sessions = max;
        self
    }

    /// Set the timeout for permission requests from the client.
    pub fn permission_timeout(mut self, timeout: Duration) -> Self {
        self.permission_timeout = timeout;
        self
    }

    /// Set the graceful shutdown timeout.
    pub fn shutdown_timeout(mut self, timeout: Duration) -> Self {
        self.shutdown_timeout = timeout;
        self
    }

    /// Set the transport configuration.
    pub fn transport(mut self, transport: TransportConfig) -> Self {
        self.transport = transport;
        self
    }

    /// Build the configuration, validating all required fields.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - `agent` is not set
    /// - `session_service` is not set
    /// - `max_sessions` is 0
    /// - `permission_timeout` is 0
    /// - `shutdown_timeout` is 0
    pub fn build(self) -> Result<AcpServerConfig, AcpServerError> {
        let agent =
            self.agent.ok_or_else(|| AcpServerError::Internal("agent is required".to_string()))?;

        let session_service = self
            .session_service
            .ok_or_else(|| AcpServerError::Internal("session_service is required".to_string()))?;

        if self.max_sessions == 0 {
            return Err(AcpServerError::Internal(
                "max_sessions must be greater than 0".to_string(),
            ));
        }

        if self.permission_timeout.is_zero() {
            return Err(AcpServerError::Internal(
                "permission_timeout must be greater than 0".to_string(),
            ));
        }

        if self.shutdown_timeout.is_zero() {
            return Err(AcpServerError::Internal(
                "shutdown_timeout must be greater than 0".to_string(),
            ));
        }

        Ok(AcpServerConfig {
            agent,
            session_service,
            agent_name: self.agent_name,
            agent_description: self.agent_description,
            streaming: self.streaming,
            tool_use: self.tool_use,
            tool_names: self.tool_names,
            max_sessions: self.max_sessions,
            permission_timeout: self.permission_timeout,
            shutdown_timeout: self.shutdown_timeout,
            transport: self.transport,
        })
    }
}
