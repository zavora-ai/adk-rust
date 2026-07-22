//! Builds the official ACP v1 capability declaration for the ADK-Rust agent.

pub use agent_client_protocol::schema::v1::AgentCapabilities;
use agent_client_protocol::schema::v1::{
    SessionAdditionalDirectoriesCapabilities, SessionCapabilities, SessionCloseCapabilities,
    SessionDeleteCapabilities, SessionListCapabilities, SessionResumeCapabilities,
};

use super::config::AcpServerConfig;

/// Constructs capabilities that exactly match the registered server handlers.
pub struct CapabilitiesBuilder;

impl CapabilitiesBuilder {
    /// Build the stable ACP v1 capability set implemented by the server.
    pub fn build(_config: &AcpServerConfig) -> AgentCapabilities {
        AgentCapabilities::new().session_capabilities(
            SessionCapabilities::new()
                .list(SessionListCapabilities::new())
                .delete(SessionDeleteCapabilities::new())
                .additional_directories(SessionAdditionalDirectoriesCapabilities::new())
                .resume(SessionResumeCapabilities::new())
                .close(SessionCloseCapabilities::new()),
        )
    }
}
