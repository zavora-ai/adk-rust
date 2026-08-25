//! OpenAI provider implementation for ADK.
//!
//! This module provides support for OpenAI, Azure OpenAI, and OpenAI-compatible APIs.
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_model::openai::{OpenAIClient, OpenAIConfig};
//!
//! let client = OpenAIClient::new(OpenAIConfig {
//!     api_key: std::env::var("OPENAI_API_KEY").unwrap(),
//!     model: "gpt-4o-mini".to_string(),
//!     ..Default::default()
//! })?;
//! ```

mod background;
mod client;
mod compaction;
mod config;
#[cfg(feature = "openai-conversations")]
pub mod conversations;
pub(crate) mod convert;
pub mod file_input;
pub mod pricing;
mod responses_client;
mod responses_convert;
pub mod schema_adapter;
#[cfg(feature = "openai-ws")]
pub mod ws_transport;

pub use crate::openai_compatible::{OpenAICompatible, OpenAICompatibleConfig};
pub use client::{AzureOpenAIClient, OpenAIClient};
pub use config::{
    AzureConfig, OpenAIConfig, OpenAIReasoningEffort, OpenAIResponsesConfig, PromptCacheRetention,
    ReasoningEffort, ReasoningSummary, ResponsesTransport, ServiceTier,
};
#[cfg(feature = "openai-conversations")]
pub use conversations::ConversationsClient;
pub use responses_client::OpenAIResponsesClient;
pub use schema_adapter::{OpenAiSchemaAdapter, OpenAiStrictSchemaAdapter};
#[cfg(feature = "openai-ws")]
pub use ws_transport::WsTransport;
