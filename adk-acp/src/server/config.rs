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
//!     .build()?;
//! ```

use std::sync::Arc;
use std::time::Duration;

use adk_core::Agent;
use adk_session::SessionService;

use super::auth::AcpAuthenticator;
use super::error::AcpServerError;
use super::modes::SessionControls;
use super::permission::StandingPermissions;
use super::providers::AcpProviders;

/// Which client-provided system prompts the server applies, advertised as the
/// `sessionCapabilities.systemPrompt` capability.
///
/// The server applies a system prompt to every model request of the session's
/// turns through a run invocation hook: `append` adds it after the agent's own
/// system instructions, and `override` replaces them. Agents that make no model
/// requests are unaffected.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum SystemPromptSupport {
    /// Advertise no system prompt support and reject `systemPrompt` on session
    /// requests.
    Disabled,
    /// Accept `append` only.
    Append,
    /// Accept `append` and `override` (the default).
    #[default]
    AppendAndOverride,
}

/// Transport selection for the ACP Server.
#[derive(Clone, Debug, Default)]
pub enum TransportConfig {
    /// Stdio transport (newline-delimited JSON on stdin/stdout).
    #[default]
    Stdio,
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
    /// Stable ADK-Rust user identifier used for sessions on this ACP connection.
    pub user_id: String,
    /// Maximum concurrent sessions allowed.
    pub max_sessions: usize,
    /// Graceful shutdown timeout.
    pub shutdown_timeout: Duration,
    /// Transport configuration.
    pub transport: TransportConfig,
    /// Optional provider of session modes and configuration options.
    ///
    /// When `None` (the default), the server advertises no modes and no
    /// configuration options, preserving capability accuracy for agents that
    /// expose no interactive session controls.
    pub session_controls: Option<Arc<dyn SessionControls>>,
    /// Optional store for "always" answers to permission requests (spec 3.3 §10 A1).
    ///
    /// When `None` (the default), every permission request offers allow once and deny
    /// only.
    pub standing_permissions: Option<Arc<dyn StandingPermissions>>,
    /// Optional authentication for ACP connections.
    ///
    /// When `None` (the default), the server advertises no `authMethods` and
    /// requires no authentication.
    pub authenticator: Option<Arc<dyn AcpAuthenticator>>,
    /// Which client-provided system prompts the server applies.
    pub system_prompts: SystemPromptSupport,
    /// Optional hook through which clients list and configure the agent's
    /// model providers (`providers/*`).
    ///
    /// When `None` (the default), the server does not advertise the
    /// `providers` capability.
    pub providers: Option<Arc<dyn AcpProviders>>,
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
    user_id: String,
    max_sessions: usize,
    shutdown_timeout: Duration,
    transport: TransportConfig,
    session_controls: Option<Arc<dyn SessionControls>>,
    standing_permissions: Option<Arc<dyn StandingPermissions>>,
    authenticator: Option<Arc<dyn AcpAuthenticator>>,
    system_prompts: SystemPromptSupport,
    providers: Option<Arc<dyn AcpProviders>>,
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
            user_id: "acp-client".to_string(),
            max_sessions: 16,
            shutdown_timeout: Duration::from_secs(30),
            transport: TransportConfig::Stdio,
            session_controls: None,
            standing_permissions: None,
            authenticator: None,
            system_prompts: SystemPromptSupport::default(),
            providers: None,
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

    /// Set the stable ADK-Rust user identifier used by ACP sessions.
    pub fn user_id(mut self, user_id: impl Into<String>) -> Self {
        self.user_id = user_id.into();
        self
    }

    /// Set the maximum number of concurrent sessions.
    pub fn max_sessions(mut self, max: usize) -> Self {
        self.max_sessions = max;
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

    /// Set the provider of session modes and configuration options.
    ///
    /// When unset, the server advertises no modes and no configuration options.
    pub fn session_controls(mut self, controls: Arc<dyn SessionControls>) -> Self {
        self.session_controls = Some(controls);
        self
    }

    /// Offer "always allow" and "always deny" on permission requests for the calls
    /// `standing` may remember, and remember those answers through it as standing rules
    /// (spec 3.3 §10 A1).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// // A harness remembers answers as its own standing rules.
    /// let harness = Arc::new(HarnessAgent::builder("harness").model(model).build()?);
    /// let config = AcpServerConfigBuilder::new()
    ///     .agent(harness.clone())
    ///     .session_service(sessions)
    ///     .standing_permissions(harness)
    ///     .build()?;
    /// ```
    pub fn standing_permissions(mut self, standing: Arc<dyn StandingPermissions>) -> Self {
        self.standing_permissions = Some(standing);
        self
    }

    /// Advertise `authenticator`'s methods and answer `authenticate`, `logout`
    /// and `auth/status` through it.
    ///
    /// When the authenticator requires authentication, session requests on a
    /// connection are refused with `auth_required` until the client
    /// authenticates.
    pub fn authenticator(mut self, authenticator: Arc<dyn AcpAuthenticator>) -> Self {
        self.authenticator = Some(authenticator);
        self
    }

    /// Choose which client-provided system prompts the server applies.
    pub fn system_prompts(mut self, support: SystemPromptSupport) -> Self {
        self.system_prompts = support;
        self
    }

    /// Let clients list and configure the agent's model providers through
    /// `providers`, and advertise the `providers` capability.
    ///
    /// [`ModelProviders`](super::ModelProviders) is a ready-made hook over
    /// [`SwitchableModel`](super::SwitchableModel)s.
    pub fn providers(mut self, providers: Arc<dyn AcpProviders>) -> Self {
        self.providers = Some(providers);
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
            user_id: self.user_id,
            max_sessions: self.max_sessions,
            shutdown_timeout: self.shutdown_timeout,
            transport: self.transport,
            session_controls: self.session_controls,
            standing_permissions: self.standing_permissions,
            authenticator: self.authenticator,
            system_prompts: self.system_prompts,
            providers: self.providers,
        })
    }
}
