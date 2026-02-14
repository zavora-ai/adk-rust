//! Error types for the realtime module.

use thiserror::Error;

/// Result type for realtime operations.
pub type Result<T> = std::result::Result<T, RealtimeError>;

/// Errors that can occur during realtime operations.
#[derive(Error, Debug)]
pub enum RealtimeError {
    /// WebSocket connection error.
    #[error("WebSocket connection error: {0}")]
    ConnectionError(String),

    /// WebSocket message error.
    #[error("WebSocket message error: {0}")]
    MessageError(String),

    /// Authentication error.
    #[error("Authentication error: {0}")]
    AuthError(String),

    /// Session not connected.
    #[error("Session not connected")]
    NotConnected,

    /// Session already closed.
    #[error("Session already closed")]
    SessionClosed,

    /// Invalid configuration.
    #[error("Invalid configuration: {0}")]
    ConfigError(String),

    /// Audio format error.
    #[error("Audio format error: {0}")]
    AudioFormatError(String),

    /// Tool execution error.
    #[error("Tool execution error: {0}")]
    ToolError(String),

    /// Server returned an error.
    #[error("Server error: {code} - {message}")]
    ServerError {
        /// Error code from the server.
        code: String,
        /// Error message from the server.
        message: String,
    },

    /// Timeout waiting for response.
    #[error("Timeout: {0}")]
    Timeout(String),

    /// Serialization/deserialization error.
    #[error("Serialization error: {0}")]
    SerializationError(#[from] serde_json::Error),

    /// Provider-specific error.
    #[error("Provider error: {0}")]
    ProviderError(String),

    /// Generic IO error.
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

impl RealtimeError {
    /// Create a new connection error.
    pub fn connection<S: Into<String>>(msg: S) -> Self {
        Self::ConnectionError(msg.into())
    }

    /// Create a new server error.
    pub fn server<S: Into<String>>(code: S, message: S) -> Self {
        Self::ServerError { code: code.into(), message: message.into() }
    }

    /// Create a new provider error.
    pub fn provider<S: Into<String>>(msg: S) -> Self {
        Self::ProviderError(msg.into())
    }

    /// Create a new configuration error.
    pub fn config<S: Into<String>>(msg: S) -> Self {
        Self::ConfigError(msg.into())
    }

    /// Create a new protocol error.
    pub fn protocol<S: Into<String>>(msg: S) -> Self {
        Self::MessageError(msg.into())
    }

    /// Create a new audio format error.
    pub fn audio<S: Into<String>>(msg: S) -> Self {
        Self::AudioFormatError(msg.into())
    }
}
