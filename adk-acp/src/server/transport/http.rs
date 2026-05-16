//! HTTP transport with Server-Sent Events for streaming.
//!
//! This is a stub implementation. The full Axum-based HTTP transport
//! requires additional dependencies (axum, axum-extra) and is complex
//! to implement. The stdio transport is the priority for IDE connections.
//!
//! # Endpoints (planned)
//!
//! - `POST /acp/initialize` → InitializeResponse
//! - `POST /acp/session/create` → session_id
//! - `POST /acp/session/{id}/prompt` → SSE stream of notifications
//! - `POST /acp/session/{id}/permission` → permission response
//! - `DELETE /acp/session/{id}` → close session

use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use tracing::info;

use super::super::error::AcpServerError;
use super::super::handler::AcpSessionHandler;
use super::Transport;

/// HTTP transport using Server-Sent Events for streaming.
///
/// Binds to a configurable address and port, serving ACP protocol
/// messages over HTTP. Prompt responses are streamed via SSE.
///
/// **Note:** This is currently a stub. Use [`StdioTransport`](super::stdio::StdioTransport)
/// for production IDE connections.
pub struct HttpTransport {
    /// Address to bind to.
    pub bind_address: String,
    /// Port to listen on.
    pub port: u16,
}

impl HttpTransport {
    /// Create a new HTTP transport with the given bind address and port.
    pub fn new(bind_address: impl Into<String>, port: u16) -> Self {
        Self { bind_address: bind_address.into(), port }
    }
}

#[async_trait]
impl Transport for HttpTransport {
    async fn serve(
        &self,
        _handler: Arc<AcpSessionHandler>,
        shutdown: CancellationToken,
    ) -> Result<(), AcpServerError> {
        info!(
            bind = %self.bind_address,
            port = %self.port,
            "HTTP transport started (stub — not fully implemented)"
        );

        // Wait for shutdown signal
        shutdown.cancelled().await;

        info!("HTTP transport shutting down");
        Ok(())
    }
}
