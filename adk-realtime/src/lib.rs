//! # adk-realtime
//!
//! Real-time bidirectional audio streaming for ADK agents.
//!
//! The crate offers a low-level session API ([`RealtimeModel`], [`RealtimeSession`]), an event
//! loop with tool dispatch ([`RealtimeRunner`]), and [`RealtimeAgent`], which implements
//! `adk_core::Agent` so a voice agent runs under the standard ADK runner.
//!
//! ```text
//!                     ┌─────────────────────────────────────────┐
//!                     │              Agent trait                │
//!                     │  (name, description, run, sub_agents)   │
//!                     └────────────────┬────────────────────────┘
//!                                      │
//!              ┌───────────────────────┼───────────────────────┐
//!              │                       │                       │
//!     ┌────────▼────────┐    ┌─────────▼─────────┐   ┌─────────▼─────────┐
//!     │    LlmAgent     │    │  RealtimeAgent    │   │  SequentialAgent  │
//!     │  (text-based)   │    │  (voice-based)    │   │   (workflow)      │
//!     └─────────────────┘    └───────────────────┘   └───────────────────┘
//! ```
//!
//! ## Providers
//!
//! | Feature | Provider |
//! |---------|----------|
//! | `openai` | OpenAI Realtime API over WebSocket |
//! | `openai-webrtc` | OpenAI Realtime API over WebRTC with Opus |
//! | `gemini` | Gemini Live API |
//! | `vertex-live` | Vertex AI Live API with Application Default Credentials |
//! | `livekit` | LiveKit WebRTC bridge |
//! | `heygen-avatar`, `did-avatar` | Video avatar providers |
//! | `integration` | `IntegratedRealtimeRunner` with sessions, memory, and plugins |
//!
//! No provider is enabled by default. Every tool call goes through
//! `adk_core::authorize_tool_call`: the kill switch, the `ToolPolicy`, and confirmation.
//!
//! ## Example: low-level session
//!
//! ```rust,no_run
//! # #[cfg(feature = "openai")]
//! # #[tokio::main]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use adk_realtime::openai::OpenAIRealtimeModel;
//! use adk_realtime::{RealtimeConfig, RealtimeModel, ServerEvent};
//!
//! let model = OpenAIRealtimeModel::new(std::env::var("OPENAI_API_KEY")?, "gpt-realtime-2.1");
//! let config = RealtimeConfig::default()
//!     .with_instruction("Answer in one sentence.")
//!     .with_modalities(vec!["text".to_string()]);
//!
//! let session = model.connect(config).await?;
//! session.send_text("Name one planet.").await?;
//! session.create_response().await?;
//! while let Some(event) = session.next_event().await {
//!     match event? {
//!         ServerEvent::TextDelta { delta, .. } => print!("{delta}"),
//!         ServerEvent::ResponseDone { .. } => break,
//!         _ => {}
//!     }
//! }
//! # Ok(())
//! # }
//! # #[cfg(not(feature = "openai"))]
//! # fn main() {}
//! ```
//!
//! ## Example: RealtimeAgent
//!
//! ```rust,no_run
//! # #[cfg(feature = "openai")]
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use adk_realtime::RealtimeAgent;
//! use adk_realtime::openai::OpenAIRealtimeModel;
//! use std::sync::Arc;
//!
//! let model = Arc::new(OpenAIRealtimeModel::new("sk-...", "gpt-realtime-2.1"));
//! let agent = RealtimeAgent::builder("voice_assistant")
//!     .model(model)
//!     .instruction("You are a helpful voice assistant.")
//!     .voice("alloy")
//!     .server_vad()
//!     .build()?;
//! # let _ = agent;
//! # Ok(())
//! # }
//! # #[cfg(not(feature = "openai"))]
//! # fn main() {}
//! ```

#![allow(clippy::result_large_err)]

pub mod agent;
pub mod audio;
pub mod config;
pub mod error;
pub mod events;
pub mod model;
pub mod runner;
pub mod session;

// Provider implementations
#[cfg(feature = "openai")]
pub mod openai;

#[cfg(feature = "gemini")]
pub mod gemini;

#[cfg(feature = "livekit")]
pub mod livekit;

#[cfg(feature = "video-avatar")]
pub mod avatar;

#[cfg(feature = "integration")]
pub mod integration;

#[cfg(feature = "integration")]
pub use integration::{
    AggregatedEvent, CompletedToolCall, DefaultToolContextFactory, IntegratedRealtimeRunner,
    IntegratedRealtimeRunnerBuilder, IntegrationConfig, RealtimeToolContext, SessionIdentity,
    ToolBridgeAdapter, TranscriptAggregator,
};

// Re-exports
pub use agent::{RealtimeAgent, RealtimeAgentBuilder};
pub use audio::{AudioEncoding, AudioFormat};
pub use config::{
    InterruptionDetection, RealtimeConfig, RealtimeConfigBuilder, VadConfig, VadMode,
};
pub use error::{RealtimeError, Result};
pub use events::{ClientEvent, ServerEvent, ToolCall, ToolResponse};
pub use model::{BoxedModel, RealtimeModel};
pub use runner::RealtimeRunner;
pub use session::{BoxedSession, DisconnectReason, RealtimeSession, RealtimeSessionExt};
