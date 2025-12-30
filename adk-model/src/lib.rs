//! # adk-model
//!
//! LLM model integrations for ADK (Gemini, OpenAI, Anthropic, DeepSeek, etc.).
//!
//! ## Overview
//!
//! This crate provides LLM implementations for ADK agents. Currently supports:
//!
//! - [`GeminiModel`] - Google's Gemini models (2.0 Flash, Pro, etc.)
//! - `OpenAIClient` - OpenAI models (GPT-4o, GPT-4o-mini, etc.) - requires `openai` feature
//! - `AzureOpenAIClient` - Azure OpenAI Service - requires `openai` feature
//! - `AnthropicClient` - Anthropic Claude models (Claude 4, Claude 3.5, etc.) - requires `anthropic` feature
//! - `DeepSeekClient` - DeepSeek models (deepseek-chat, deepseek-reasoner) - requires `deepseek` feature
//! - `OllamaModel` - Local LLMs via Ollama (LLaMA, Mistral, Qwen, etc.) - requires `ollama` feature
//! - `GroqClient` - Groq ultra-fast inference (LLaMA, Mixtral, Gemma) - requires `groq` feature
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
//! ### DeepSeek
//!
//! ```rust,ignore
//! use adk_model::deepseek::{DeepSeekClient, DeepSeekConfig};
//!
//! // Chat model
//! let chat = DeepSeekClient::chat(std::env::var("DEEPSEEK_API_KEY").unwrap()).unwrap();
//!
//! // Reasoner with thinking mode
//! let reasoner = DeepSeekClient::reasoner(std::env::var("DEEPSEEK_API_KEY").unwrap()).unwrap();
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
//! ### DeepSeek
//! | Model | Description |
//! |-------|-------------|
//! | `deepseek-chat` | Fast, capable chat model |
//! | `deepseek-reasoner` | Reasoning model with thinking mode |
//!
//! ## Features
//!
//! - Async streaming with backpressure
//! - Tool/function calling support
//! - Multimodal input (text, images, audio, video, PDF)
//! - Generation configuration (temperature, top_p, etc.)
//! - OpenAI-compatible APIs (Ollama, vLLM, etc.)
//!
//! ### Ollama (Local)
//!
//! ```rust,ignore
//! use adk_model::ollama::{OllamaModel, OllamaConfig};
//!
//! // Default: localhost:11434
//! let model = OllamaModel::new(OllamaConfig::new("llama3.2")).unwrap();
//! ```

#[cfg(feature = "anthropic")]
pub mod anthropic;
#[cfg(feature = "deepseek")]
pub mod deepseek;
#[cfg(feature = "gemini")]
pub mod gemini;
#[cfg(feature = "groq")]
pub mod groq;
pub mod mock;
#[cfg(feature = "ollama")]
pub mod ollama;
#[cfg(feature = "openai")]
pub mod openai;

#[cfg(feature = "anthropic")]
pub use anthropic::AnthropicClient;
#[cfg(feature = "deepseek")]
pub use deepseek::{DeepSeekClient, DeepSeekConfig};
#[cfg(feature = "gemini")]
pub use gemini::GeminiModel;
#[cfg(feature = "groq")]
pub use groq::{GroqClient, GroqConfig};
pub use mock::MockLlm;
#[cfg(feature = "ollama")]
pub use ollama::{OllamaConfig, OllamaModel};
#[cfg(feature = "openai")]
pub use openai::{AzureConfig, AzureOpenAIClient, OpenAIClient, OpenAIConfig};
