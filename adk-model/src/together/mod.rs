//! Together AI provider implementation for ADK.
//!
//! Provides access to Together AI hosted open models via the
//! OpenAI-compatible API. Requires the `together` feature flag.
//!
//! # Quick Start
//!
//! ```rust,ignore
//! use adk_model::together::{TogetherClient, TogetherConfig};
//!
//! let config = TogetherConfig::new("your-api-key", "meta-llama/Llama-3.3-70B-Instruct-Turbo");
//! let client = TogetherClient::new(config)?;
//! ```
//!
//! # Environment Variable
//!
//! Set `TOGETHER_API_KEY` with your Together AI API key.

mod client;
mod config;

pub use client::TogetherClient;
pub use config::{TOGETHER_API_BASE, TogetherConfig};
