//! Groq provider implementation for ADK.
//!
//! This module provides support for Groq's ultra-fast LLM inference including:
//! - LLaMA models (llama-3.3-70b-versatile, llama-3.1-8b-instant)
//! - Mixtral (mixtral-8x7b-32768)
//! - Gemma (gemma2-9b-it)
//!
//! # Features
//!
//! - **Ultra-Fast Inference**: Groq's LPU architecture delivers industry-leading speed
//! - **Tool Calling**: Full function/tool calling support
//! - **Streaming**: Real-time streaming responses
//! - **Reasoning**: Optional reasoning mode with `include_reasoning`
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_model::groq::{GroqClient, GroqConfig};
//!
//! // LLaMA 3.3 70B
//! let client = GroqClient::new(GroqConfig::llama70b(
//!     std::env::var("GROQ_API_KEY").unwrap()
//! ))?;
//!
//! // Fast 8B model
//! let fast = GroqClient::new(GroqConfig::llama8b(
//!     std::env::var("GROQ_API_KEY").unwrap()
//! ))?;
//!
//! // Mixtral
//! let mixtral = GroqClient::new(GroqConfig::mixtral(
//!     std::env::var("GROQ_API_KEY").unwrap()
//! ))?;
//! ```

mod client;
mod config;
mod convert;

pub use client::GroqClient;
pub use config::{GroqConfig, GROQ_API_BASE};
