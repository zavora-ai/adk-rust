//! # adk-realtime
//!
//! Real-time bidirectional audio/video streaming for ADK agents.
//!
//! This crate provides a unified interface for building voice-enabled AI agents
//! using real-time streaming APIs from various providers (OpenAI, Gemini, etc.).
//!
//! ## Architecture
//!
//! `adk-realtime` follows the same pattern as OpenAI's Agents SDK, providing both
//! a low-level session interface and a high-level `RealtimeAgent` that implements
//! the standard ADK `Agent` trait.
//!
//! ```text
//!                     ┌─────────────────────────────────────────┐
//!                     │              Agent Trait                │
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
//! ## Features
//!
//! - **RealtimeAgent**: Implements `adk_core::Agent` with callbacks, tools, instructions
//! - **Multiple Providers**: OpenAI Realtime API and Gemini Live API support
//! - **Audio Streaming**: Bidirectional audio with various formats (PCM16, G711)
//! - **Voice Activity Detection**: Server-side VAD for natural conversations
//! - **Tool Calling**: Real-time function execution during voice sessions
//!
//! ## Example - Using RealtimeAgent (Recommended)
//!
//! ```rust,ignore
//! use adk_realtime::{RealtimeAgent, openai::OpenAIRealtimeModel};
//! use adk_runner::Runner;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let model = OpenAIRealtimeModel::new(api_key, "gpt-4o-realtime-preview-2024-12-17");
//!
//!     let agent = RealtimeAgent::builder("voice_assistant")
//!         .model(Arc::new(model))
//!         .instruction("You are a helpful voice assistant.")
//!         .voice("alloy")
//!         .server_vad()
//!         .tool(Arc::new(weather_tool))
//!         .build()?;
//!
//!     // Use with standard ADK runner
//!     let runner = Runner::new(Arc::new(agent));
//!     runner.run(session, content).await?;
//!
//!     Ok(())
//! }
//! ```
//!
//! ## Example - Using Low-Level Session API
//!
//! ```rust,ignore
//! use adk_realtime::{RealtimeModel, RealtimeConfig, ServerEvent};
//! use adk_realtime::openai::OpenAIRealtimeModel;
//!
//! let model = OpenAIRealtimeModel::new(api_key, "gpt-4o-realtime-preview-2024-12-17");
//! let session = model.connect(config).await?;
//!
//! while let Some(event) = session.next_event().await {
//!     match event? {
//!         ServerEvent::AudioDelta { delta, .. } => { /* play audio */ }
//!         ServerEvent::TextDelta { delta, .. } => println!("{}", delta),
//!         _ => {}
//!     }
//! }
//! ```

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

// Re-exports
pub use agent::{RealtimeAgent, RealtimeAgentBuilder};
pub use audio::{AudioEncoding, AudioFormat};
pub use config::{RealtimeConfig, RealtimeConfigBuilder, VadConfig, VadMode};
pub use error::{RealtimeError, Result};
pub use events::{ClientEvent, ServerEvent, ToolCall, ToolResponse};
pub use model::RealtimeModel;
pub use runner::RealtimeRunner;
pub use session::RealtimeSession;
