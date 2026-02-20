//! # adk-model
//!
//! LLM model integrations for ADK (Gemini, OpenAI, Anthropic, DeepSeek, Groq, Ollama).
//!
//! ## Overview
//!
//! This crate provides LLM implementations for ADK agents. Currently supports:
//!
//! - [`GeminiModel`] - Google's Gemini models (3 Pro, 2.5 Flash, etc.)
//! - `OpenAIClient` - OpenAI models (GPT-5, GPT-5-mini, o3, etc.) - requires `openai` feature
//! - `AzureOpenAIClient` - Azure OpenAI Service - requires `openai` feature
//! - `AnthropicClient` - Anthropic Claude models (Opus 4.6, Sonnet 4.5, etc.) - requires `anthropic` feature
//! - `DeepSeekClient` - DeepSeek models (deepseek-chat, deepseek-reasoner) - requires `deepseek` feature
//! - `GroqClient` - Groq ultra-fast inference (Llama 4, Llama 3.3, Mixtral) - requires `groq` feature
//! - `OllamaModel` - Local LLMs via Ollama (LLaMA, Mistral, Qwen, etc.) - requires `ollama` feature
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
//!     "gpt-5-mini",
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
//!     "claude-sonnet-4-5-20250929",
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
//! | `gemini-3-pro-preview` | Most intelligent, complex agentic workflows (1M context) |
//! | `gemini-3-flash-preview` | Frontier intelligence at Flash speed (1M context) |
//! | `gemini-2.5-pro` | Advanced reasoning and multimodal (1M context) |
//! | `gemini-2.5-flash` | Balanced speed and capability, recommended (1M context) |
//! | `gemini-2.5-flash-lite` | Ultra-fast for high-volume tasks (1M context) |
//!
//! ### OpenAI
//! | Model | Description |
//! |-------|-------------|
//! | `gpt-5` | Strongest coding and agentic model with adaptive reasoning |
//! | `gpt-5-mini` | Efficient variant for most tasks |
//! | `o3` | Advanced reasoning model for complex problem solving |
//! | `o4-mini` | Efficient reasoning model (200K context) |
//! | `gpt-4.1` | General purpose model with 1M context |
//!
//! ### Anthropic
//! | Model | Description |
//! |-------|-------------|
//! | `claude-opus-4-5-20251101` | Most capable for complex autonomous tasks |
//! | `claude-sonnet-4-5-20250929` | Best balance of intelligence, speed, and cost |
//! | `claude-haiku-4-5-20251001` | Ultra-efficient for high-volume workloads |
//! | `claude-opus-4-20250514` | Hybrid model with extended thinking |
//! | `claude-sonnet-4-20250514` | Balanced model with extended thinking |
//!
//! ### DeepSeek
//! | Model | Description |
//! |-------|-------------|
//! | `deepseek-chat` | V3.2 non-thinking mode for fast general-purpose tasks |
//! | `deepseek-reasoner` | V3.2 thinking mode with chain-of-thought reasoning |
//!
//! ### Groq
//! | Model | Description |
//! |-------|-------------|
//! | `meta-llama/llama-4-scout-17b-16e-instruct` | Llama 4 Scout via Groq LPU |
//! | `llama-3.3-70b-versatile` | Versatile large model |
//! | `llama-3.1-8b-instant` | Ultra-fast at 560 T/s |
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
pub(crate) mod attachment;
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
pub mod retry;

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
pub use retry::RetryConfig;
pub use retry::ServerRetryHint;
