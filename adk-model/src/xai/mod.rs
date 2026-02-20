//! xAI provider implementation for ADK.
//!
//! This module wraps the shared OpenAI-compatible client implementation.

mod client;
mod config;

pub use client::XAIClient;
pub use config::{XAI_API_BASE, XAIConfig};
