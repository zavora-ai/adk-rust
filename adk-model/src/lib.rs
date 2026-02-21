//! # adk-model
//!
//! LLM model integrations for ADK (Gemini, OpenAI, Anthropic, DeepSeek, Groq, Ollama,
//! Fireworks AI, Together AI, Mistral AI, Perplexity, Cerebras, SambaNova, Amazon Bedrock,
//! Azure AI Inference).
//!
//! ## Overview
//!
//! This crate provides LLM implementations for ADK agents. Currently supports:
//!
//! - [`GeminiModel`] - Google's Gemini models (3 Pro, 2.5 Flash, etc.)
//! - `OpenAIClient` - OpenAI models (GPT-5, GPT-5-mini, o3, etc.) - requires `openai` feature
//! - `AzureOpenAIClient` - Azure OpenAI Service - requires `openai` feature
//! - `XAIClient` - xAI Grok models via OpenAI-compatible API - requires `xai` feature
//! - `AnthropicClient` - Anthropic Claude models (Opus 4.6, Sonnet 4.5, etc.) - requires `anthropic` feature
//! - `DeepSeekClient` - DeepSeek models (deepseek-chat, deepseek-reasoner) - requires `deepseek` feature
//! - `GroqClient` - Groq ultra-fast inference (Llama 4, Llama 3.3, Mixtral) - requires `groq` feature
//! - `OllamaModel` - Local LLMs via Ollama (LLaMA, Mistral, Qwen, etc.) - requires `ollama` feature
//! - `FireworksClient` - Fireworks AI fast open-model inference - requires `fireworks` feature
//! - `TogetherClient` - Together AI hosted open models - requires `together` feature
//! - `MistralClient` - Mistral AI cloud models - requires `mistral` feature
//! - `PerplexityClient` - Perplexity search-augmented LLM - requires `perplexity` feature
//! - `CerebrasClient` - Cerebras ultra-fast inference - requires `cerebras` feature
//! - `SambaNovaClient` - SambaNova fast inference - requires `sambanova` feature
//! - `BedrockClient` - Amazon Bedrock via AWS SDK (IAM auth) - requires `bedrock` feature
//! - `AzureAIClient` - Azure AI Inference endpoints - requires `azure-ai` feature
//! - `OpenAICompatible` - Shared OpenAI-compatible client for custom providers - requires `openai` or `xai`
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
//! ### Fireworks AI
//!
//! ```rust,ignore
//! use adk_model::fireworks::{FireworksClient, FireworksConfig};
//!
//! let model = FireworksClient::new(FireworksConfig::new(
//!     std::env::var("FIREWORKS_API_KEY").unwrap(),
//!     "accounts/fireworks/models/llama-v3p1-8b-instruct",
//! )).unwrap();
//! ```
//!
//! ### Together AI
//!
//! ```rust,ignore
//! use adk_model::together::{TogetherClient, TogetherConfig};
//!
//! let model = TogetherClient::new(TogetherConfig::new(
//!     std::env::var("TOGETHER_API_KEY").unwrap(),
//!     "meta-llama/Llama-3.3-70B-Instruct-Turbo",
//! )).unwrap();
//! ```
//!
//! ### Mistral AI
//!
//! ```rust,ignore
//! use adk_model::mistral::{MistralClient, MistralConfig};
//!
//! let model = MistralClient::new(MistralConfig::new(
//!     std::env::var("MISTRAL_API_KEY").unwrap(),
//!     "mistral-small-latest",
//! )).unwrap();
//! ```
//!
//! ### Perplexity
//!
//! ```rust,ignore
//! use adk_model::perplexity::{PerplexityClient, PerplexityConfig};
//!
//! let model = PerplexityClient::new(PerplexityConfig::new(
//!     std::env::var("PERPLEXITY_API_KEY").unwrap(),
//!     "sonar",
//! )).unwrap();
//! ```
//!
//! ### Cerebras
//!
//! ```rust,ignore
//! use adk_model::cerebras::{CerebrasClient, CerebrasConfig};
//!
//! let model = CerebrasClient::new(CerebrasConfig::new(
//!     std::env::var("CEREBRAS_API_KEY").unwrap(),
//!     "llama-3.3-70b",
//! )).unwrap();
//! ```
//!
//! ### SambaNova
//!
//! ```rust,ignore
//! use adk_model::sambanova::{SambaNovaClient, SambaNovaConfig};
//!
//! let model = SambaNovaClient::new(SambaNovaConfig::new(
//!     std::env::var("SAMBANOVA_API_KEY").unwrap(),
//!     "Meta-Llama-3.3-70B-Instruct",
//! )).unwrap();
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
//! ### New Providers
//!
//! | Provider | Feature Flag | Default Model | Env Var |
//! |----------|-------------|---------------|---------|
//! | Fireworks AI | `fireworks` | `accounts/fireworks/models/llama-v3p1-8b-instruct` | `FIREWORKS_API_KEY` |
//! | Together AI | `together` | `meta-llama/Llama-3.3-70B-Instruct-Turbo` | `TOGETHER_API_KEY` |
//! | Mistral AI | `mistral` | `mistral-small-latest` | `MISTRAL_API_KEY` |
//! | Perplexity | `perplexity` | `sonar` | `PERPLEXITY_API_KEY` |
//! | Cerebras | `cerebras` | `llama-3.3-70b` | `CEREBRAS_API_KEY` |
//! | SambaNova | `sambanova` | `Meta-Llama-3.3-70B-Instruct` | `SAMBANOVA_API_KEY` |
//! | Amazon Bedrock | `bedrock` | `anthropic.claude-sonnet-4-20250514-v1:0` | AWS IAM credentials |
//! | Azure AI Inference | `azure-ai` | (endpoint-specific) | `AZURE_AI_API_KEY` |
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
pub(crate) mod attachment;
#[cfg(feature = "azure-ai")]
pub mod azure_ai;
#[cfg(feature = "bedrock")]
pub mod bedrock;
#[cfg(feature = "cerebras")]
pub mod cerebras;
#[cfg(feature = "deepseek")]
pub mod deepseek;
#[cfg(feature = "fireworks")]
pub mod fireworks;
#[cfg(feature = "gemini")]
pub mod gemini;
#[cfg(feature = "groq")]
pub mod groq;
#[cfg(feature = "mistral")]
pub mod mistral;
pub mod mock;
#[cfg(feature = "ollama")]
pub mod ollama;
#[cfg(feature = "openai")]
pub mod openai;
#[cfg(feature = "openai")]
pub mod openai_compatible;
#[cfg(feature = "perplexity")]
pub mod perplexity;
pub mod retry;
#[cfg(feature = "sambanova")]
pub mod sambanova;
#[cfg(feature = "together")]
pub mod together;
#[cfg(feature = "xai")]
pub mod xai;

#[cfg(feature = "anthropic")]
pub use anthropic::AnthropicClient;
#[cfg(feature = "azure-ai")]
pub use azure_ai::{AzureAIClient, AzureAIConfig};
#[cfg(feature = "bedrock")]
pub use bedrock::{BedrockClient, BedrockConfig};
#[cfg(feature = "cerebras")]
pub use cerebras::{CerebrasClient, CerebrasConfig};
#[cfg(feature = "deepseek")]
pub use deepseek::{DeepSeekClient, DeepSeekConfig};
#[cfg(feature = "fireworks")]
pub use fireworks::{FireworksClient, FireworksConfig};
#[cfg(feature = "gemini")]
pub use gemini::GeminiModel;
#[cfg(feature = "groq")]
pub use groq::{GroqClient, GroqConfig};
#[cfg(feature = "mistral")]
pub use mistral::{MistralClient, MistralConfig};
pub use mock::MockLlm;
#[cfg(feature = "ollama")]
pub use ollama::{OllamaConfig, OllamaModel};
#[cfg(feature = "openai")]
pub use openai::{AzureConfig, AzureOpenAIClient, OpenAIClient, OpenAIConfig};
#[cfg(feature = "openai")]
pub use openai_compatible::{OpenAICompatible, OpenAICompatibleConfig};
#[cfg(feature = "perplexity")]
pub use perplexity::{PerplexityClient, PerplexityConfig};
pub use retry::RetryConfig;
pub use retry::ServerRetryHint;
#[cfg(feature = "sambanova")]
pub use sambanova::{SambaNovaClient, SambaNovaConfig};
#[cfg(feature = "together")]
pub use together::{TogetherClient, TogetherConfig};
#[cfg(feature = "xai")]
pub use xai::{XAIClient, XAIConfig};
