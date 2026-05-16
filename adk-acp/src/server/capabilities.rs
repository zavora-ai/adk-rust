//! Agent capabilities builder for the ACP Server.
//!
//! Builds the capabilities declaration from [`AcpServerConfig`] to advertise
//! what the agent supports during initialization.

use serde::{Deserialize, Serialize};

use super::config::AcpServerConfig;

/// Declared capabilities of the ACP agent.
///
/// Sent to clients during initialization so they can adapt their UI
/// to the agent's supported features.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCapabilities {
    /// Agent name.
    pub name: String,
    /// Agent description.
    pub description: String,
    /// Whether the agent supports streaming responses.
    pub streaming: bool,
    /// Whether the agent supports tool use.
    pub tool_use: bool,
    /// List of available tool names (populated when `tool_use` is true).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tool_names: Vec<String>,
    /// Supported content types.
    pub supported_content_types: Vec<String>,
}

/// Builds the ACP capabilities declaration from server configuration.
///
/// # Example
///
/// ```rust,ignore
/// let capabilities = CapabilitiesBuilder::build(&config);
/// assert_eq!(capabilities.name, "my-agent");
/// assert!(capabilities.streaming);
/// ```
pub struct CapabilitiesBuilder;

impl CapabilitiesBuilder {
    /// Build capabilities from the server config.
    ///
    /// Sets agent name, description, streaming flag, tool use flag,
    /// tool names (when tool_use is enabled), and supported content types.
    pub fn build(config: &AcpServerConfig) -> AgentCapabilities {
        let tool_names = if config.tool_use { config.tool_names.clone() } else { Vec::new() };

        AgentCapabilities {
            name: config.agent_name.clone(),
            description: config.agent_description.clone(),
            streaming: config.streaming,
            tool_use: config.tool_use,
            tool_names,
            supported_content_types: vec!["text".to_string()],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::config::{AcpServerConfigBuilder, TransportConfig};
    use crate::server::test_helpers::mock_agent_and_session;

    #[test]
    fn test_capabilities_reflect_config() {
        let (agent, session_svc) = mock_agent_and_session();
        let config = AcpServerConfigBuilder::new()
            .agent(agent)
            .session_service(session_svc)
            .agent_name("test-agent")
            .agent_description("A test agent")
            .streaming(true)
            .tool_use(true)
            .tool_names(vec!["tool_a".to_string(), "tool_b".to_string()])
            .transport(TransportConfig::Stdio)
            .build()
            .unwrap();

        let caps = CapabilitiesBuilder::build(&config);

        assert_eq!(caps.name, "test-agent");
        assert_eq!(caps.description, "A test agent");
        assert!(caps.streaming);
        assert!(caps.tool_use);
        assert_eq!(caps.tool_names, vec!["tool_a", "tool_b"]);
        assert_eq!(caps.supported_content_types, vec!["text"]);
    }

    #[test]
    fn test_capabilities_no_tools_when_disabled() {
        let (agent, session_svc) = mock_agent_and_session();
        let config = AcpServerConfigBuilder::new()
            .agent(agent)
            .session_service(session_svc)
            .tool_use(false)
            .tool_names(vec!["should_not_appear".to_string()])
            .build()
            .unwrap();

        let caps = CapabilitiesBuilder::build(&config);

        assert!(!caps.tool_use);
        assert!(caps.tool_names.is_empty());
    }
}
