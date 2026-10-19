//! Groq provider implementation for ADK.
//!
//! This module provides support for Groq's ultra-fast LLM inference, with presets for
//! Groq's production models:
//! - `openai/gpt-oss-120b` ([`GroqConfig::gpt_oss_120b`], the default)
//! - `openai/gpt-oss-20b` ([`GroqConfig::gpt_oss_20b`], faster and lower cost)
//!
//! Any other model Groq serves works through [`GroqConfig::new`].
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
//! // GPT-OSS 120B, the production default
//! let client = GroqClient::new(GroqConfig::gpt_oss_120b(
//!     std::env::var("GROQ_API_KEY").unwrap()
//! ))?;
//!
//! // GPT-OSS 20B, faster and lower cost
//! let fast = GroqClient::new(GroqConfig::gpt_oss_20b(
//!     std::env::var("GROQ_API_KEY").unwrap()
//! ))?;
//! ```

mod client;
mod config;
mod convert;

pub use client::GroqClient;
pub use config::{GROQ_API_BASE, GroqConfig};
