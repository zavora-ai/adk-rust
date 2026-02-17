//! Anthropic/Claude provider implementation for ADK.
//!
//! This module provides support for Anthropic's Claude models.
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
//!
//! let client = AnthropicClient::new(AnthropicConfig::new(
//!     std::env::var("ANTHROPIC_API_KEY").unwrap(),
//!     "claude-sonnet-4.5",
//! ))?;
//! ```

mod client;
mod config;
mod convert;

pub use client::AnthropicClient;
pub use config::{AnthropicConfig, ThinkingConfig};
