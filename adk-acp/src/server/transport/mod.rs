//! Transport layer for ACP protocol messages.
//!
//! Defines the [`Transport`] trait and provides two implementations:
//! - [`StdioTransport`] — newline-delimited JSON over stdin/stdout
//! - [`HttpTransport`] — HTTP with Server-Sent Events (stub)

pub mod http;
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

pub use http::HttpTransport;
pub use stdio::StdioTransport;
