//! Mistral AI provider implementation for ADK.
//!
//! Provides access to Mistral AI cloud models via the
//! OpenAI-compatible API. Requires the `mistral` feature flag.
//!
//! # Quick Start
//!
//! ```rust,ignore
//! use adk_model::mistral::{MistralClient, MistralConfig};
//!
//! let config = MistralConfig::new("your-api-key", "mistral-small-latest");
//! let client = MistralClient::new(config)?;
//! ```
//!
//! # Environment Variable
//!
//! Set `MISTRAL_API_KEY` with your Mistral AI API key.

mod client;
mod config;

pub use client::MistralClient;
pub use config::{MISTRAL_API_BASE, MistralConfig};
