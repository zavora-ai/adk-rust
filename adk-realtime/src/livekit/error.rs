use thiserror::Error;

/// Error type for LiveKit bridging operations.
#[derive(Debug, Error)]
pub enum LiveKitError {
    #[error("LiveKit configuration error: {0}")]
    ConfigError(String),
    #[error(transparent)]
    TokenGenerationError(#[from] livekit_api::access_token::AccessTokenError),
    #[error(transparent)]
    ConnectionError(#[from] livekit::prelude::RoomError),
}
