//! OpenAI provider implementation for ADK.
//!
//! This module provides support for OpenAI, Azure OpenAI, and OpenAI-compatible APIs.
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_model::openai::{OpenAIClient, OpenAIConfig};
//!
//! let client = OpenAIClient::new(OpenAIConfig {
//!     api_key: std::env::var("OPENAI_API_KEY").unwrap(),
//!     model: "gpt-4o-mini".to_string(),
//!     ..Default::default()
//! })?;
//! ```

mod client;
mod config;
pub(crate) mod convert;

pub use crate::openai_compatible::{OpenAICompatible, OpenAICompatibleConfig};
pub use client::{AzureOpenAIClient, OpenAIClient};
pub use config::{AzureConfig, OpenAIConfig};
