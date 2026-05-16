//! ACP Server: expose ADK agents as ACP-compatible agents.
//!
//! The server accepts connections from ACP clients (IDEs like Kiro, VS Code)
//! and routes prompts to an ADK agent via the Runner, streaming responses
//! back as ACP notifications.
//!
//! # Architecture
//!
//! ```text
//! ACP Client (IDE) ──► Transport (Stdio/HTTP) ──► AcpSessionHandler ──► Runner ──► Agent
//!                  ◄── SessionNotifications ◄──── ResponseStreamer ◄──── EventStream
//! ```
//!
//! # Quick Start
//!
//! ```rust,ignore
//! use adk_acp::server::{AcpServer, AcpServerConfigBuilder, TransportConfig};
//! use std::sync::Arc;
//!
//! let config = AcpServerConfigBuilder::new()
//!     .agent(my_agent)
//!     .session_service(my_session_service)
//!     .agent_name("my-agent")
//!     .build()?;
//!
//! let handle = AcpServer::run(config).await?;
//!
//! // Server is running in the background...
//! // To shut down:
//! handle.shutdown();
//! handle.wait().await?;
//! ```

pub mod capabilities;
pub mod config;
pub mod error;
pub mod handler;
pub mod permission;
pub mod streamer;
pub mod transport;

#[cfg(test)]
pub(crate) mod test_helpers;

use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use tracing::info;

pub use capabilities::{AgentCapabilities, CapabilitiesBuilder};
pub use config::{AcpServerConfig, AcpServerConfigBuilder, TransportConfig};
pub use error::{AcpServerError, ErrorResponse};
pub use handler::AcpSessionHandler;
pub use permission::{PermissionBridge, PermissionOutcome};
pub use streamer::{ResponseStreamer, SessionNotification};
pub use transport::{HttpTransport, StdioTransport, Transport};

/// Handle returned by [`AcpServer::run()`] for lifecycle control.
///
/// Use `shutdown()` to request graceful shutdown and `wait()` to block
/// until the server has fully stopped.
pub struct AcpServerHandle {
    shutdown_token: CancellationToken,
    join_handle: tokio::task::JoinHandle<Result<(), AcpServerError>>,
}

impl AcpServerHandle {
    /// Request graceful shutdown of the server.
    ///
    /// The server will stop accepting new sessions and prompts,
    /// wait for in-progress executions to complete (up to the configured
    /// shutdown timeout), then release all resources.
    pub fn shutdown(&self) {
        self.shutdown_token.cancel();
    }

    /// Wait for the server to fully stop.
    ///
    /// Returns the result of the server's background task.
    pub async fn wait(self) -> Result<(), AcpServerError> {
        self.join_handle
            .await
            .map_err(|e| AcpServerError::Internal(format!("server task panicked: {e}")))?
    }
}

/// The ACP Server exposing an ADK agent via the Agent Client Protocol.
///
/// Use [`AcpServer::run()`] to start the server with a given configuration.
/// The server runs in a background Tokio task and processes ACP protocol
/// messages according to the configured transport.
///
/// # Example
///
/// ```rust,ignore
/// use adk_acp::server::{AcpServer, AcpServerConfigBuilder};
/// use std::sync::Arc;
///
/// let config = AcpServerConfigBuilder::new()
///     .agent(my_agent)
///     .session_service(session_svc)
///     .agent_name("my-agent")
///     .build()?;
///
/// let handle = AcpServer::run(config).await?;
/// // ... server is running ...
/// handle.shutdown();
/// handle.wait().await?;
/// ```
pub struct AcpServer;

impl AcpServer {
    /// Start the ACP server with the given configuration.
    ///
    /// Creates the session handler, selects the transport based on config,
    /// spawns the serve loop in a background task, and returns a handle
    /// for lifecycle control.
    ///
    /// # Errors
    ///
    /// Returns an error if the handler cannot be created.
    pub async fn run(config: AcpServerConfig) -> Result<AcpServerHandle, AcpServerError> {
        let shutdown_token = CancellationToken::new();
        let handler = Arc::new(AcpSessionHandler::new(&config, shutdown_token.clone())?);

        let transport: Box<dyn Transport> = match &config.transport {
            TransportConfig::Stdio => Box::new(StdioTransport::new(&config)),
            TransportConfig::Http { bind_address, port } => {
                Box::new(HttpTransport::new(bind_address.clone(), *port))
            }
        };

        let shutdown_timeout = config.shutdown_timeout;
        let serve_shutdown = shutdown_token.clone();
        let handler_for_drain = handler.clone();

        let join_handle = tokio::spawn(async move {
            info!("ACP server starting");

            let result = transport.serve(handler.clone(), serve_shutdown).await;

            // Drain sessions on shutdown
            handler_for_drain.drain_sessions(shutdown_timeout).await;

            info!("ACP server stopped");
            result
        });

        Ok(AcpServerHandle { shutdown_token, join_handle })
    }
}
