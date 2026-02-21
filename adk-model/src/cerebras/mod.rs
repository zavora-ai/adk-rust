//! Cerebras provider implementation for ADK.
//!
//! Provides access to Cerebras ultra-fast inference via the
//! OpenAI-compatible API. Requires the `cerebras` feature flag.
//!
//! # Quick Start
//!
//! ```rust,ignore
//! use adk_model::cerebras::{CerebrasClient, CerebrasConfig};
//!
//! let config = CerebrasConfig::new("your-api-key", "llama-3.3-70b");
//! let client = CerebrasClient::new(config)?;
//! ```
//!
//! # Environment Variable
//!
//! Set `CEREBRAS_API_KEY` with your Cerebras API key.

mod client;
mod config;

pub use client::CerebrasClient;
pub use config::{CEREBRAS_API_BASE, CerebrasConfig};
