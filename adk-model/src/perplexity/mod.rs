//! Perplexity provider implementation for ADK.
//!
//! Provides access to Perplexity's search-augmented LLM via the
//! OpenAI-compatible API. Requires the `perplexity` feature flag.
//!
//! # Quick Start
//!
//! ```rust,ignore
//! use adk_model::perplexity::{PerplexityClient, PerplexityConfig};
//!
//! let config = PerplexityConfig::new("your-api-key", "sonar");
//! let client = PerplexityClient::new(config)?;
//! ```
//!
//! # Environment Variable
//!
//! Set `PERPLEXITY_API_KEY` with your Perplexity API key.

mod client;
mod config;

pub use client::PerplexityClient;
pub use config::{PERPLEXITY_API_BASE, PerplexityConfig};
