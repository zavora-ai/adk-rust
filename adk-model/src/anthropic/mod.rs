//! Anthropic/Claude provider implementation for ADK.
//!
//! This module provides full API parity with Anthropic's Claude models,
//! including system prompt routing, multimodal content (images and PDFs),
//! streaming with thinking deltas, structured errors, rate-limit-aware retry,
//! prompt caching, extended thinking, token counting, and model discovery.
//!
//! # Quick Start
//!
//! ```rust,ignore
//! use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
//!
//! let client = AnthropicClient::new(AnthropicConfig::new(
//!     std::env::var("ANTHROPIC_API_KEY").unwrap(),
//!     "claude-sonnet-4-5-20250929",
//! ))?;
//! ```
//!
//! # Extended Thinking
//!
//! Enable chain-of-thought reasoning with a token budget:
//!
//! ```rust,ignore
//! use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
//!
//! let config = AnthropicConfig::new("sk-ant-xxx", "claude-sonnet-4-5-20250929")
//!     .with_thinking(8192);
//! let client = AnthropicClient::new(config)?;
//! ```
//!
//! # Prompt Caching
//!
//! Reduce latency and cost for repeated prefixes:
//!
//! ```rust,ignore
//! let config = AnthropicConfig::new("sk-ant-xxx", "claude-sonnet-4-5-20250929")
//!     .with_prompt_caching(true);
//! ```
//!
//! # Token Counting
//!
//! Count input tokens without generating a response:
//!
//! ```rust,ignore
//! let count = client.count_tokens(&request).await?;
//! println!("Input tokens: {}", count.input_tokens);
//! ```
//!
//! # Model Discovery
//!
//! List available models or get details for a specific model:
//!
//! ```rust,ignore
//! let models = client.list_models().await?;
//! let info = client.get_model("claude-sonnet-4-5-20250929").await?;
//! ```
//!
//! # Rate Limit Information
//!
//! Inspect rate-limit state after each request:
//!
//! ```rust,ignore
//! let info = client.latest_rate_limit_info().await;
//! if let Some(remaining) = info.requests_remaining {
//!     println!("Requests remaining: {remaining}");
//! }
//! ```
//!
//! # Error Handling
//!
//! Structured errors preserve the Anthropic error type, message, status code,
//! and request ID for debugging:
//!
//! ```rust
//! use adk_model::anthropic::AnthropicApiError;
//!
//! let err = AnthropicApiError {
//!     error_type: "rate_limit_error".to_string(),
//!     message: "Too many requests".to_string(),
//!     status_code: 429,
//!     request_id: Some("req_abc123".to_string()),
//! };
//! assert!(err.to_string().contains("429"));
//! ```

mod client;
mod config;
mod convert;
mod error;
mod models;
mod rate_limit;
mod token_count;

pub use client::AnthropicClient;
pub use config::{AnthropicConfig, ThinkingConfig};
pub use error::{AnthropicApiError, ConversionError};
pub use models::ModelInfo;
pub use rate_limit::RateLimitInfo;
pub use token_count::TokenCount;
