//! # adk-model
//!
//! LLM model integrations for ADK (Gemini, OpenAI, Anthropic, etc.).
//!
//! ## Overview
//!
//! This crate provides LLM implementations for ADK agents. Currently supports:
//!
//! - [`GeminiModel`] - Google's Gemini models (2.0 Flash, Pro, etc.)
//! - [`OpenAIClient`] - OpenAI models (GPT-4o, GPT-4o-mini, etc.)
//! - [`AzureOpenAIClient`] - Azure OpenAI Service
//! - [`AnthropicClient`] - Anthropic Claude models (Claude 4, Claude 3.5, etc.)
//! - [`MockLlm`] - Mock LLM for testing
//!
//! ## Quick Start
//!
//! ### Gemini
//!
//! ```rust,no_run
//! use adk_model::GeminiModel;
//! use std::sync::Arc;
//!
//! let api_key = std::env::var("GOOGLE_API_KEY").unwrap();
//! let model = GeminiModel::new(&api_key, "gemini-2.5-flash").unwrap();
//! ```
//!
//! ### OpenAI
//!
//! ```rust,ignore
//! use adk_model::openai::{OpenAIClient, OpenAIConfig};
//!
//! let model = OpenAIClient::new(OpenAIConfig::new(
//!     std::env::var("OPENAI_API_KEY").unwrap(),
//!     "gpt-4o-mini",
//! )).unwrap();
//! ```
//!
//! ### Anthropic
//!
//! ```rust,ignore
//! use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
//!
//! let model = AnthropicClient::new(AnthropicConfig::new(
//!     std::env::var("ANTHROPIC_API_KEY").unwrap(),
//!     "claude-sonnet-4-20250514",
//! )).unwrap();
//! ```
//!
//! ## Supported Models
//!
//! ### Gemini
//! | Model | Description |
//! |-------|-------------|
//! | `gemini-2.5-flash` | Fast, efficient model (recommended) |
//! | `gemini-1.5-pro` | Most capable model |
//! | `gemini-1.5-flash` | Balanced speed/capability |
//!
//! ### OpenAI
//! | Model | Description |
//! |-------|-------------|
//! | `gpt-4o` | Most capable model |
//! | `gpt-4o-mini` | Fast, cost-effective |
//! | `gpt-4-turbo` | Previous generation flagship |
//!
//! ### Anthropic
//! | Model | Description |
//! |-------|-------------|
//! | `claude-sonnet-4-20250514` | Latest Claude 4 Sonnet |
//! | `claude-3-5-sonnet-20241022` | Claude 3.5 Sonnet |
//! | `claude-3-opus-20240229` | Most capable Claude 3 |
//!
//! ## Features
//!
//! - Async streaming with backpressure
//! - Tool/function calling support
//! - Multimodal input (text, images, audio, video, PDF)
//! - Generation configuration (temperature, top_p, etc.)
//! - OpenAI-compatible APIs (Ollama, vLLM, etc.)

#[cfg(feature = "anthropic")]
pub mod anthropic;
#[cfg(feature = "gemini")]
pub mod gemini;
pub mod mock;
#[cfg(feature = "openai")]
pub mod openai;

#[cfg(feature = "anthropic")]
pub use anthropic::AnthropicClient;
#[cfg(feature = "gemini")]
pub use gemini::GeminiModel;
pub use mock::MockLlm;
#[cfg(feature = "openai")]
pub use openai::{AzureConfig, AzureOpenAIClient, OpenAIClient, OpenAIConfig};
