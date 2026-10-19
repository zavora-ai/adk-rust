//! # adk-mistralrs
//!
//! Native [mistral.rs](https://github.com/EricLBuehler/mistral.rs) integration for ADK-Rust:
//! local LLM inference inside the process, without a separate server.
//!
//! ```toml
//! [dependencies]
//! adk-mistralrs = "3.0.0"
//! ```
//!
//! ## Features
//!
//! - **In-process inference**: mistral.rs runs inside the application
//! - **ISQ (in-situ quantization)**: quantize weights while loading
//! - **PagedAttention**: memory-efficient attention for long contexts
//! - **Devices**: CPU, CUDA, and Metal, with layer ranges split across GPUs
//! - **Tool calling** through the ADK [`Llm`] interface
//! - **LoRA and X-LoRA adapters** with runtime swapping
//! - **Vision, speech, image generation, and embedding models**
//! - **Multi-model serving** with routing by name
//! - **MCP client** for external tools
//!
//! ## Quick start
//!
//! ```rust,no_run
//! use adk_core::{Content, Llm, LlmRequest};
//! use adk_mistralrs::MistralRsModel;
//! use futures::StreamExt;
//!
//! # #[tokio::main]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let model = MistralRsModel::from_hf("Qwen/Qwen3-4B").await?;
//! let request = LlmRequest::new(
//!     "Qwen/Qwen3-4B",
//!     vec![Content::new("user").with_text("Name one planet.")],
//! );
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
//! ## ISQ quantization
//!
//! ```rust,no_run
//! use adk_mistralrs::{MistralRsConfig, MistralRsModel, ModelSource, QuantizationLevel};
//!
//! # #[tokio::main]
//! # async fn main() -> adk_mistralrs::Result<()> {
//! let config = MistralRsConfig::builder()
//!     .model_source(ModelSource::huggingface("Qwen/Qwen3-4B"))
//!     .isq(QuantizationLevel::Q4K)
//!     .paged_attention(true)
//!     .build();
//! let model = MistralRsModel::new(config).await?;
//! # let _ = model;
//! # Ok(())
//! # }
//! ```
//!
//! ## Module overview
//!
//! - [`MistralRsModel`] — text models implementing the ADK [`Llm`] trait
//! - [`MistralRsConfig`], [`ModelSource`], [`QuantizationLevel`] — configuration
//! - [`MistralRsAdapterModel`] — LoRA and X-LoRA adapters
//! - [`MistralRsVisionModel`] — vision models
//! - [`MistralRsEmbeddingModel`] — embedding models
//! - [`MistralRsSpeechModel`] — speech synthesis
//! - [`MistralRsDiffusionModel`] — image generation
//! - [`MistralRsMultiModel`] — multi-model serving
//! - [`McpClientConfig`] — MCP client configuration
//! - [`convert`] — conversions between ADK and mistral.rs types
//! - [`MistralRsError`] — errors
//!
//! ## Feature flags
//!
//! The default build runs on the CPU. `metal`, `accelerate`, `cuda`, `cudnn`, `flash-attn`,
//! `nccl`, `mkl`, and `ring` pass through to mistral.rs; `reqwest` adds image loading from URLs.

mod adapter;
mod client;
mod config;
pub mod convert;
mod diffusion;
mod embedding;
mod error;
mod mcp;
mod multimodel;
mod realtime;
mod speech;
pub mod tracing_utils;
mod vision;

pub use adapter::*;
pub use client::*;
pub use config::*;
pub use diffusion::*;
pub use embedding::*;
pub use error::*;
pub use mcp::*;
pub use multimodel::*;
pub use realtime::*;
pub use speech::*;
pub use vision::*;

// Re-export commonly used types
pub use adk_core::{Llm, LlmRequest, LlmResponse, LlmResponseStream};
