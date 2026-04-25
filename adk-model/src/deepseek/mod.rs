//! DeepSeek provider implementation for ADK.
//!
//! Supports DeepSeek V4 models and legacy models:
//!
//! | Model | Description | Thinking |
//! |-------|-------------|----------|
//! | `deepseek-v4-pro` | Strongest reasoning, thinking enabled by default | Yes |
//! | `deepseek-v4-flash` | Fast, cost-efficient | Optional |
//! | `deepseek-chat` | Legacy general-purpose chat | No |
//! | `deepseek-reasoner` | Legacy reasoning with thinking | Yes |
//!
//! # Features
//!
//! - **Thinking Mode**: Chain-of-thought reasoning with `reasoning_content`
//! - **Reasoning Effort**: Control thinking depth (`high` or `max`)
//! - **Tool Calling**: Full function/tool calling, including in thinking mode
//! - **Strict Tool Mode** (beta): Model strictly follows JSON schema for tool args
//! - **Streaming**: Real-time streaming responses with reasoning chunks
//! - **Prefix Caching**: Automatic disk-based KV cache (server-side, zero config)
//! - **JSON Output**: Structured JSON responses via `response_format`
//! - **Anthropic API**: Compatible endpoint at `https://api.deepseek.com/anthropic`
//!
//! # V4 Quick Start
//!
//! ```rust,ignore
//! use adk_model::deepseek::{DeepSeekClient, DeepSeekConfig, ReasoningEffort};
//!
//! // V4 Pro with max reasoning effort
//! let pro = DeepSeekClient::new(
//!     DeepSeekConfig::v4_pro("api-key")
//!         .with_reasoning_effort(ReasoningEffort::Max)
//! )?;
//!
//! // V4 Flash (fast, no thinking by default)
//! let flash = DeepSeekClient::v4_flash("api-key")?;
//!
//! // V4 Pro with strict tool mode (beta)
//! let strict = DeepSeekClient::new(
//!     DeepSeekConfig::v4_pro("api-key")
//!         .with_strict_tools()
//! )?;
//! ```
//!
//! # Legacy (backward compatible)
//!
//! ```rust,ignore
//! let chat = DeepSeekClient::chat("api-key")?;
//! let reasoner = DeepSeekClient::reasoner("api-key")?;
//! ```
//!
//! # Thinking Mode
//!
//! V4 models default to thinking enabled. The model outputs chain-of-thought
//! reasoning (`reasoning_content`) before the final answer. Control it with:
//!
//! - `ThinkingMode::Enabled` / `ThinkingMode::Disabled` — explicit toggle
//! - `ReasoningEffort::High` / `ReasoningEffort::Max` — thinking depth
//!
//! In thinking mode, `temperature`, `top_p`, `presence_penalty`, and
//! `frequency_penalty` are silently ignored by the API.
//!
//! # Anthropic API Compatibility
//!
//! DeepSeek V4 also exposes an Anthropic-compatible endpoint. You can use
//! `adk_model::AnthropicClient` pointed at `https://api.deepseek.com/anthropic`
//! with a DeepSeek API key:
//!
//! ```rust,ignore
//! use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
//!
//! let config = AnthropicConfig::new("deepseek-api-key", "deepseek-v4-pro")
//!     .with_base_url("https://api.deepseek.com/anthropic");
//! let client = AnthropicClient::new(config)?;
//! ```

mod client;
mod config;
mod convert;

pub use client::DeepSeekClient;
pub use config::{
    DEEPSEEK_ANTHROPIC_API_BASE, DEEPSEEK_API_BASE, DEEPSEEK_BETA_API_BASE, DeepSeekConfig,
    ReasoningEffort, ThinkingMode,
};
