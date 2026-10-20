//! # adk-model
//!
//! LLM provider clients for ADK-Rust. Every client implements [`adk_core::Llm`], so agents and
//! runners accept any of them.
//!
//! ## Providers
//!
//! | Client | Provider | Feature |
//! |--------|----------|---------|
//! | [`GeminiModel`] | Google Gemini (AI Studio, Vertex AI with `gemini-vertex`) | `gemini` (default) |
//! | `OpenAIClient` | OpenAI Chat Completions | `openai` |
//! | `openai::OpenAIResponsesClient` | OpenAI Responses API | `openai` |
//! | `AzureOpenAIClient` | Azure OpenAI | `openai` |
//! | `OpenAICompatible` | Any OpenAI-compatible endpoint, with presets for xAI, Fireworks AI, Together AI, Mistral AI, Perplexity, Cerebras, SambaNova, OrcaRouter, MiniMax, Zhipu AI, Baidu, Cohere, ByteDance, and Gemini | `openai` |
//! | `AnthropicClient` | Anthropic Claude | `anthropic` |
//! | `DeepSeekClient` | DeepSeek | `deepseek` |
//! | `GroqClient` | Groq | `groq` |
//! | `OllamaModel` | Ollama (local) | `ollama` |
//! | `OpenRouterClient` | OpenRouter, with its native chat, responses, routing, discovery, and credits APIs | `openrouter` |
//! | `OpenCodeClient` | OpenCode Go and Zen | `opencode` |
//! | `BedrockClient` | Amazon Bedrock | `bedrock` |
//! | `AzureAIClient` | Azure AI Inference | `azure-ai` |
//! | [`ScriptedLlm`] | Test double that answers each call with the next scripted reply and records every request | always |
//! | [`MockLlm`] | Test double that replays the same responses on every call | always |
//!
//! [`catalog`] holds the recommended model per provider and the lifecycle of known model IDs;
//! [`pricing::PricingCatalog`] prices usage for every provider.
//!
//! ## Quick start
//!
//! ```rust,no_run
//! use adk_core::{Content, Llm, LlmRequest};
//! use adk_model::GeminiModel;
//! use futures::StreamExt;
//!
//! # #[tokio::main]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let model = GeminiModel::new(std::env::var("GOOGLE_API_KEY")?, "gemini-3.7-flash")?;
//! let request = LlmRequest::new(
//!     "gemini-3.7-flash",
//!     vec![Content::new("user").with_text("Name one planet in a short sentence.")],
//! );
//!
//! let mut responses = model.generate_content(request, false).await?;
//! while let Some(response) = responses.next().await {
//!     if let Some(content) = response?.content {
//!         for part in &content.parts {
//!             if let Some(text) = part.text() {
//!                 print!("{text}");
//!             }
//!         }
//!     }
//! }
//! # Ok(())
//! # }
//! ```
//!
//! ## Other providers
//!
//! Each client is built from its config and used the same way:
//!
//! ```rust,no_run
//! # #[cfg(all(feature = "openai", feature = "anthropic", feature = "deepseek"))]
//! # fn main() -> Result<(), adk_core::AdkError> {
//! use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
//! use adk_model::deepseek::DeepSeekClient;
//! use adk_model::openai::{OpenAIClient, OpenAIConfig, OpenAIResponsesClient, OpenAIResponsesConfig};
//! use adk_model::{OpenAICompatible, OpenAICompatibleConfig};
//!
//! let chat = OpenAIClient::new(OpenAIConfig::new("sk-...", "gpt-5.6-terra"))?;
//! let responses = OpenAIResponsesClient::new(OpenAIResponsesConfig::new("sk-...", "gpt-5.6-terra"))?;
//! let claude = AnthropicClient::new(AnthropicConfig::new("sk-ant-...", "claude-sonnet-5"))?;
//! let deepseek = DeepSeekClient::v4_flash("sk-...")?;
//! let grok = OpenAICompatible::new(OpenAICompatibleConfig::xai("xai-...", "grok-4.6"))?;
//! # let _ = (chat, responses, claude, deepseek, grok);
//! # Ok(())
//! # }
//! # #[cfg(not(all(feature = "openai", feature = "anthropic", feature = "deepseek")))]
//! # fn main() {}
//! ```
//!
//! ## Test doubles
//!
//! ```rust
//! use adk_core::{Content, Llm, LlmRequest};
//! use adk_model::{ScriptedLlm, ScriptedResponse};
//! use futures::StreamExt;
//!
//! # #[tokio::main]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let model = ScriptedLlm::new("scripted", vec![ScriptedResponse::text("Mars.")]);
//! let request = LlmRequest::new("scripted", vec![Content::new("user").with_text("Name a planet.")]);
//!
//! let response = model.generate_content(request, false).await?.next().await.ok_or("no response")??;
//! assert_eq!(response.content.unwrap().parts[0].text(), Some("Mars."));
//! assert_eq!(model.requests().len(), 1);
//! # Ok(())
//! # }
//! ```
//!
//! ## OpenRouter scope boundary
//!
//! `OpenRouterClient`'s native methods cover model discovery and credits, exact chat and
//! responses payloads, routing and fallback, plugins, and built-in tools. Its [`adk_core::Llm`]
//! adapter covers streaming, reasoning parts, tool calling, and multimodal requests, takes
//! OpenRouter tuning through `openrouter::OpenRouterRequestOptions`, and maps responses into
//! the generic [`adk_core::LlmResponse`] shape.

#![allow(clippy::result_large_err)]
#![deny(missing_docs)]

#[cfg(feature = "anthropic")]
pub mod anthropic;
pub(crate) mod attachment;
#[cfg(feature = "azure-ai")]
pub mod azure_ai;
#[cfg(feature = "bedrock")]
pub mod bedrock;
/// Curated provider model catalog, lifecycle metadata, and recommended defaults.
pub mod catalog;
#[cfg(feature = "deepseek")]
pub mod deepseek;
#[cfg(any(
    feature = "anthropic",
    feature = "azure-ai",
    feature = "deepseek",
    feature = "gemini",
    feature = "groq",
    feature = "ollama",
    feature = "openai",
    feature = "openrouter"
))]
mod error_metadata;
/// Gemini model provider (Google AI Studio and Vertex AI).
#[cfg(feature = "gemini")]
pub mod gemini;
#[cfg(any(
    feature = "openai",
    feature = "anthropic",
    feature = "bedrock",
    feature = "groq",
    feature = "deepseek",
    feature = "azure-ai",
    feature = "gemini-interactions",
    feature = "openrouter",
    feature = "ollama"
))]
mod generation_config;
#[cfg(feature = "groq")]
pub mod groq;
/// Connect and idle timeouts for the HTTP transport of model clients.
pub mod http_timeouts;
/// Mock LLM for testing without real API calls.
pub mod mock;
#[cfg(feature = "ollama")]
pub mod ollama;
#[cfg(feature = "openai")]
pub mod openai;
#[cfg(feature = "openai")]
pub mod openai_compatible;
#[cfg(feature = "opencode")]
pub mod opencode;
#[cfg(feature = "openrouter")]
pub mod openrouter;
/// Conversion outcomes for content parts sent to a provider.
pub mod part_conversion;
/// Token prices for every provider and cost calculation from usage.
pub mod pricing;

/// Canonical provider identifiers and metadata.
pub mod provider;
/// Retry logic with exponential backoff for transient provider errors.
pub mod retry;
/// Scripted LLM test double with per-call replies and request capture.
pub mod scripted;
#[cfg(any(
    feature = "openai",
    feature = "deepseek",
    feature = "azure-ai",
    feature = "groq",
    feature = "ollama"
))]
mod sse;
#[cfg(any(
    feature = "openai",
    feature = "deepseek",
    feature = "azure-ai",
    feature = "groq",
    feature = "ollama",
    feature = "openrouter"
))]
mod stream_error;
#[cfg(any(
    feature = "openai",
    feature = "anthropic",
    feature = "deepseek",
    feature = "groq",
    feature = "bedrock",
    feature = "azure-ai",
    feature = "openrouter"
))]
pub(crate) mod tool_args;
pub mod tool_call_parser;
#[cfg(any(
    feature = "openai",
    feature = "ollama",
    feature = "deepseek",
    feature = "groq",
    feature = "bedrock",
    feature = "azure-ai",
    feature = "openrouter"
))]
pub(crate) mod tool_result;
#[cfg(any(feature = "openai", feature = "ollama", feature = "deepseek", feature = "groq"))]
pub(crate) mod usage_json;
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
pub use http_timeouts::HttpTimeouts;
pub use mock::MockLlm;
#[cfg(feature = "ollama")]
pub use ollama::{OllamaConfig, OllamaModel};
#[cfg(feature = "openai")]
pub use openai::{AzureConfig, AzureOpenAIClient, OpenAIClient, OpenAIConfig, ReasoningEffort};
#[cfg(feature = "openai")]
pub use openai_compatible::{OpenAICompatible, OpenAICompatibleConfig, ReasoningReplayField};
#[cfg(feature = "opencode")]
pub use opencode::{OpenCodeApi, OpenCodeClient, OpenCodeConfig, OpenCodeService};
#[cfg(feature = "openrouter")]
pub use openrouter::{OpenRouterApiMode, OpenRouterClient, OpenRouterConfig};
pub use pricing::PricingCatalog;
pub use provider::ModelProvider;
pub use retry::RetryConfig;
pub use retry::ServerRetryHint;
pub use scripted::{SCRIPT_EXHAUSTED_CODE, ScriptedLlm, ScriptedResponse};
