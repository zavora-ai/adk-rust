//! SambaNova provider implementation for ADK.
//!
//! Provides access to SambaNova fast inference via the
//! OpenAI-compatible API. Requires the `sambanova` feature flag.
//!
//! # Quick Start
//!
//! ```rust,ignore
//! use adk_model::sambanova::{SambaNovaClient, SambaNovaConfig};
//!
//! let config = SambaNovaConfig::new("your-api-key", "Meta-Llama-3.3-70B-Instruct");
//! let client = SambaNovaClient::new(config)?;
//! ```
//!
//! # Environment Variable
//!
//! Set `SAMBANOVA_API_KEY` with your SambaNova API key.

mod client;
mod config;

pub use client::SambaNovaClient;
pub use config::{SAMBANOVA_API_BASE, SambaNovaConfig};
