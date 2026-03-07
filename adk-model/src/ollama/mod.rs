//! Ollama local LLM provider implementation for ADK.
//!
//! This module provides support for running local LLMs via Ollama.
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_model::ollama::{OllamaModel, OllamaConfig};
//!
//! let model = OllamaModel::new(OllamaConfig {
//!     model: "llama3.2"),
//!     ..Default::default()
//! })?;
//! ```

mod client;
mod config;
mod convert;

pub use client::OllamaModel;
pub use config::OllamaConfig;
