/// Gemini model client implementation.
pub mod client;
/// Conversion layer between ADK and Interactions API types (Beta).
#[cfg(feature = "gemini-interactions")]
pub mod interactions_convert;
/// Interactions API target allowlist (Beta).
#[cfg(feature = "gemini-interactions")]
pub mod interactions_target;
/// Request options carried through the provider-neutral types: media resolution and video
/// sampling.
pub mod options;
/// Streaming response handling for Gemini.
pub mod streaming;

pub use crate::retry::RetryConfig;
pub use client::{GeminiFile, GeminiFileState, GeminiModel, vertex_env_requested};
pub use options::{
    GEMINI_EXTENSION_NAMESPACE, GeminiRequestOptions, MediaResolution, VideoMetadata,
    video_part_annotations,
};

// Re-export the Interactions transport surface (Beta) so consumers can configure
// the toggle through `adk_model::gemini::{...}` without reaching into submodules.
// Gated behind `gemini-interactions`.
#[cfg(feature = "gemini-interactions")]
pub use client::{BackgroundMode, GeminiTransport, InteractionOptions};
#[cfg(feature = "gemini-interactions")]
pub use interactions_target::InteractionTarget;

// Re-export thinking config types from adk-gemini so users don't need
// a direct dependency on adk-gemini to configure thinking.
pub use adk_gemini::{ThinkingConfig, ThinkingLevel};

// Re-export the Interactions API (Beta) so consumers can reach Google's new
// stateful, step-based API surface through adk-model without a direct
// adk-gemini dependency. Gated behind `gemini-interactions`.
#[cfg(feature = "gemini-interactions")]
pub use adk_gemini::interactions;
