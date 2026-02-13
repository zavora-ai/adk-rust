//! Core RealtimeModel trait definition.

use crate::audio::AudioFormat;
use crate::config::RealtimeConfig;
use crate::error::Result;
use crate::session::BoxedSession;
use async_trait::async_trait;

/// A factory for creating real-time sessions.
///
/// Each provider (OpenAI, Gemini, etc.) implements this trait to provide
/// their specific realtime connection logic.
///
/// # Example
///
/// ```rust,ignore
/// use adk_realtime::{RealtimeModel, RealtimeConfig};
/// use adk_realtime::openai::OpenAIRealtimeModel;
///
/// #[tokio::main]
/// async fn main() -> Result<()> {
///     let model = OpenAIRealtimeModel::new(api_key, "gpt-4o-realtime-preview-2024-12-17");
///
///     let config = RealtimeConfig::default()
///         .with_instruction("You are a helpful assistant.")
///         .with_voice("alloy");
///
///     let session = model.connect(config).await?;
///
///     // Use the session...
///
///     session.close().await?;
///     Ok(())
/// }
/// ```
#[async_trait]
pub trait RealtimeModel: Send + Sync {
    /// Get the provider name (e.g., "openai", "gemini").
    fn provider(&self) -> &str;

    /// Get the model identifier.
    fn model_id(&self) -> &str;

    /// Check if this model supports realtime streaming.
    fn supports_realtime(&self) -> bool {
        true
    }

    /// Get supported input audio formats.
    fn supported_input_formats(&self) -> Vec<AudioFormat>;

    /// Get supported output audio formats.
    fn supported_output_formats(&self) -> Vec<AudioFormat>;

    /// Get available voices for this model.
    fn available_voices(&self) -> Vec<&str>;

    /// Connect and create a new realtime session.
    ///
    /// This establishes a WebSocket connection to the provider and
    /// configures the session with the provided settings.
    async fn connect(&self, config: RealtimeConfig) -> Result<BoxedSession>;
}

/// A shared model type for thread-safe access.
pub type BoxedModel = std::sync::Arc<dyn RealtimeModel>;
