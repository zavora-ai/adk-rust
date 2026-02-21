//! Fireworks AI provider implementation for ADK.
//!
//! Provides access to Fireworks AI fast open-model inference via the
//! OpenAI-compatible API. Requires the `fireworks` feature flag.
//!
//! # Quick Start
//!
//! ```rust,ignore
//! use adk_model::fireworks::{FireworksClient, FireworksConfig};
//!
//! let config = FireworksConfig::new("your-api-key", "accounts/fireworks/models/llama-v3p1-8b-instruct");
//! let client = FireworksClient::new(config)?;
//! ```
//!
//! # Environment Variable
//!
//! Set `FIREWORKS_API_KEY` with your Fireworks AI API key.

mod client;
mod config;

pub use client::FireworksClient;
pub use config::{FIREWORKS_API_BASE, FireworksConfig};
