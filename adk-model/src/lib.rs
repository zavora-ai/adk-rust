//! # adk-model
#![allow(clippy::result_large_err)]
//!
//! LLM model integrations for ADK (Gemini, OpenAI, OpenRouter, Anthropic, DeepSeek, Groq,
//! Ollama, Fireworks AI, Together AI, Mistral AI, Perplexity, Cerebras, SambaNova, Amazon
//! Bedrock, Azure AI Inference).
//!
//! ## Overview
//!
//! This crate provides LLM implementations for ADK agents. Currently supports:
//!
//! - [`GeminiModel`] - Google's Gemini models (3 Pro, 2.5 Flash, etc.)
//! - `OpenAIClient` - OpenAI models (GPT-5, GPT-5-mini, o3, etc.) — requires `openai` feature
//! - `AzureOpenAIClient` - Azure OpenAI Service — requires `openai` feature
//! - `OpenAICompatible` - Any OpenAI-compatible API (xAI, Fireworks, Together, Mistral, Perplexity, Cerebras, SambaNova, or custom) — requires `openai` feature, use `OpenAICompatibleConfig` presets
//! - `AnthropicClient` - Anthropic Claude models — requires `anthropic` feature
//! - `DeepSeekClient` - DeepSeek models — requires `deepseek` feature
//! - `GroqClient` - Groq ultra-fast inference — requires `groq` feature
//! - `OpenRouterClient` - OpenRouter-native chat, responses, and discovery APIs — requires `openrouter` feature
//! - `OllamaModel` - Local LLMs via Ollama — requires `ollama` feature
//! - `BedrockClient` - Amazon Bedrock via AWS SDK — requires `bedrock` feature
//! - `AzureAIClient` - Azure AI Inference endpoints — requires `azure-ai` feature
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
//! ### OpenRouter
//!
//! ```rust,ignore
//! use adk_core::{Content, Llm, LlmRequest};
//! use adk_model::openrouter::{OpenRouterClient, OpenRouterConfig};
//! use futures::StreamExt;
//!
//! let client = OpenRouterClient::new(
//!     OpenRouterConfig::new(
//!         std::env::var("OPENROUTER_API_KEY").unwrap(),
//!         "openai/gpt-4.1-mini",
//!     )
//!     .with_http_referer("https://github.com/zavora-ai/adk-rust")
//!     .with_title("ADK-Rust"),
//! ).unwrap();
//!
//! let request = LlmRequest::new(
//!     "openai/gpt-4.1-mini",
//!     vec![Content::new("user").with_text("Reply in one short sentence.")],
//! );
//!
//! # tokio_test::block_on(async {
//! let mut stream = client.generate_content(request, true).await.unwrap();
//! while let Some(item) = stream.next().await {
//!     let _response = item.unwrap();
//! }
//! # });
//! ```
//!
//! Native OpenRouter APIs such as model discovery, credits, routing configuration, plugins, and
//! exact Responses payload access remain available on [`OpenRouterClient`] itself.
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
//! ### OpenAI-Compatible Providers (Fireworks, Together, Mistral, Perplexity, Cerebras, SambaNova, xAI)
//!
//! All OpenAI-compatible providers use `OpenAICompatible` with provider presets:
//!
//! ```rust,ignore
//! use adk_model::openai_compatible::{OpenAICompatible, OpenAICompatibleConfig};
//!
//! // Fireworks AI
//! let model = OpenAICompatible::new(OpenAICompatibleConfig::fireworks(
//!     std::env::var("FIREWORKS_API_KEY").unwrap(),
//!     "accounts/fireworks/models/llama-v3p1-8b-instruct",
//! )).unwrap();
//!
//! // Together AI
//! let model = OpenAICompatible::new(OpenAICompatibleConfig::together(
//!     std::env::var("TOGETHER_API_KEY").unwrap(),
//!     "meta-llama/Llama-3.3-70B-Instruct-Turbo",
//! )).unwrap();
//!
//! // Or any custom OpenAI-compatible endpoint
//! let model = OpenAICompatible::new(
//!     OpenAICompatibleConfig::new("your-api-key", "your-model")
//!         .with_base_url("https://your-endpoint.com/v1")
//!         .with_provider_name("my-provider"),
//! ).unwrap();
//! ```
//!
//! ### Amazon Bedrock
//!
//! ```rust,ignore
//! use adk_model::bedrock::{BedrockClient, BedrockConfig};
//!
//! // Uses AWS IAM credentials from the environment (no API key needed)
//! let config = BedrockConfig::new("us-east-1", "anthropic.claude-sonnet-4-20250514-v1:0");
//! let model = BedrockClient::new(config).await.unwrap();
//! ```
//!
//! ### Azure AI Inference
//!
//! ```rust,ignore
//! use adk_model::azure_ai::{AzureAIClient, AzureAIConfig};
//!
//! let model = AzureAIClient::new(AzureAIConfig::new(
//!     "https://my-endpoint.eastus.inference.ai.azure.com",
//!     std::env::var("AZURE_AI_API_KEY").unwrap(),
//!     "meta-llama-3.1-8b-instruct",
//! )).unwrap();
//! ```
//!
//! ### Ollama (Local)
//!
//! ```rust,ignore
//! use adk_model::ollama::{OllamaModel, OllamaConfig};
//!
//! // Default: localhost:11434
//! let model = OllamaModel::new(OllamaConfig::new("llama3.2")).unwrap();
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
//! ### OpenAI-Compatible Providers (via `openai` feature)
//!
//! Use `OpenAICompatibleConfig` presets — one client, one feature flag:
//!
//! | Provider | Preset | Env Var |
//! |----------|--------|---------|
//! | Fireworks AI | `OpenAICompatibleConfig::fireworks()` | `FIREWORKS_API_KEY` |
//! | Together AI | `OpenAICompatibleConfig::together()` | `TOGETHER_API_KEY` |
//! | Mistral AI | `OpenAICompatibleConfig::mistral()` | `MISTRAL_API_KEY` |
//! | Perplexity | `OpenAICompatibleConfig::perplexity()` | `PERPLEXITY_API_KEY` |
//! | Cerebras | `OpenAICompatibleConfig::cerebras()` | `CEREBRAS_API_KEY` |
//! | SambaNova | `OpenAICompatibleConfig::sambanova()` | `SAMBANOVA_API_KEY` |
//! | xAI (Grok) | `OpenAICompatibleConfig::xai()` | `XAI_API_KEY` |
//!
//! ### Other Providers
//!
//! | Provider | Feature Flag | Env Var |
//! |----------|-------------|---------|
//! | Amazon Bedrock | `bedrock` | AWS IAM credentials |
//! | Azure AI Inference | `azure-ai` | `AZURE_AI_API_KEY` |
//! | OpenRouter | `openrouter` | `OPENROUTER_API_KEY` |
//!
//! ## Features
//!
//! - Async streaming with backpressure
//! - Tool/function calling support
//! - Multimodal input (text, images, audio, video, PDF)
//! - Generation configuration (temperature, top_p, etc.)
//! - OpenAI-compatible APIs (Ollama, vLLM, etc.)
//!
//! ## OpenRouter Scope Boundary
//!
//! [`OpenRouterClient`] exposes the provider's native surfaces:
//!
//! - chat completions
//! - responses
//! - model discovery and credits
//! - provider routing and fallback
//! - OpenRouter plugins and built-in tools
//! - exact provider metadata and annotations
//!
//! The generic [`Llm`] adapter exists for agent compatibility and covers text generation,
//! streaming, reasoning parts, tool/function calling, and OpenRouter request options through
//! [`openrouter::OpenRouterRequestOptions`].
//!
//! Use the native OpenRouter APIs whenever you need exact request or response parity, discovery,
//! or provider-specific features that do not fit the generic [`LlmRequest`] / [`LlmResponse`]
//! shape without information loss.
//!
//! For end-to-end agentic validation, see the local `examples/openrouter` crate and the
//! `adk-model/examples/openrouter_*` binaries.

#[cfg(feature = "anthropic")]
pub mod anthropic;
pub(crate) mod attachment;
#[cfg(feature = "azure-ai")]
pub mod azure_ai;
#[cfg(feature = "bedrock")]
pub mod bedrock;
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
#[cfg(feature = "openai")]
pub mod openai_compatible;
#[cfg(feature = "openrouter")]
pub mod openrouter;
pub mod provider;
pub mod retry;
#[cfg(any(
    feature = "openai",
    feature = "ollama",
    feature = "deepseek",
    feature = "groq",
    feature = "bedrock",
    feature = "azure-ai"
))]
pub(crate) mod tool_result;
pub mod usage_tracking;

#[cfg(feature = "anthropic")]
pub use anthropic::AnthropicClient;
#[cfg(feature = "azure-ai")]
pub use azure_ai::{AzureAIClient, AzureAIConfig};
#[cfg(feature = "bedrock")]
pub use bedrock::{BedrockClient, BedrockConfig};
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
pub use openai::{AzureConfig, AzureOpenAIClient, OpenAIClient, OpenAIConfig, ReasoningEffort};
#[cfg(feature = "openai")]
pub use openai_compatible::{OpenAICompatible, OpenAICompatibleConfig};
#[cfg(feature = "openrouter")]
pub use openrouter::{OpenRouterApiMode, OpenRouterClient, OpenRouterConfig};
pub use provider::ModelProvider;
pub use retry::RetryConfig;
pub use retry::ServerRetryHint;
