//! Transport layer for ACP protocol messages.
//!
//! Defines the [`Transport`] trait and provides [`StdioTransport`] for the
//! official ACP JSON-RPC stream over stdin/stdout.
//!
//! ACP v1's stable local transport is stdio. Remote HTTP/WebSocket transport
//! remains under specification work and is not advertised here.

pub mod stdio;

use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use super::error::AcpServerError;
use super::handler::AcpSessionHandler;

/// A transport layer for ACP protocol messages.
///
/// Implementations handle the wire format and connection management,
/// routing incoming messages to the [`AcpSessionHandler`] and sending
/// responses back to the client.
#[async_trait]
pub trait Transport: Send + Sync {
    /// Start listening for incoming connections/messages.
    ///
    /// Returns when the transport is shut down (via the cancellation token)
    /// or encounters a fatal error.
    async fn serve(
        &self,
        handler: Arc<AcpSessionHandler>,
        shutdown: CancellationToken,
    ) -> Result<(), AcpServerError>;
}

pub use stdio::StdioTransport;
