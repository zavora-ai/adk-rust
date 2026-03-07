//! # adk-mistralrs
//!
//! Native [mistral.rs](https://github.com/EricLBuehler/mistral.rs) integration for ADK-Rust,
//! providing blazingly fast local LLM inference without external dependencies.
//!
//! > **Note:** This crate is NOT published to crates.io because mistral.rs depends on
//! > unpublished git dependencies. Add it via git dependency instead:
//! >
//! > ```toml
//! > [dependencies]
//! > adk-mistralrs = { git = "https://github.com/zavora-ai/adk-rust" }
//! > ```
//!
//! ## Features
//!
//! - **Native Rust Integration**: Direct embedding of mistral.rs, no daemon required
//! - **ISQ (In-Situ Quantization)**: Quantize models on-the-fly at load time
//! - **PagedAttention**: Memory-efficient attention for longer contexts
//! - **Multi-Device Support**: CPU, CUDA, Metal acceleration
//! - **Tool Calling**: Full function calling support via ADK interface
//! - **LoRA/X-LoRA Adapters**: Fine-tuned model support with hot-swapping
//! - **Vision Models**: Multimodal inference with image understanding
//! - **Speech Models**: Text-to-speech synthesis with multi-speaker support
//! - **Diffusion Models**: Image generation with FLUX models
//! - **Embedding Models**: Semantic embeddings for RAG and search
//! - **Multi-Model Serving**: Load and route to multiple models
//! - **MCP Integration**: Connect to MCP servers for external tools
//!
//! ## Quick Start
//!
//! ```rust,ignore
//! use adk_mistralrs::{MistralRsModel, MistralRsConfig, ModelSource};
//! use adk_core::Llm;
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     // Load a model from HuggingFace
//!     let model = MistralRsModel::from_hf("mistralai/Magistral-Small-2509").await?;
//!
//!     // Use with ADK agents
//!     let agent = LlmAgent::builder()
//!         .name("assistant")
//!         .model(model)
//!         .instruction("You are a helpful assistant.")
//!         .build();
//!
//!     Ok(())
//! }
//! ```
//!
//! ## ISQ Quantization Example
//!
//! Reduce memory usage by quantizing models on-the-fly:
//!
//! ```rust,ignore
//! use adk_mistralrs::{MistralRsModel, MistralRsConfig, ModelSource, QuantizationLevel};
//!
//! let config = MistralRsConfig::builder()
//!     .model_source(ModelSource::huggingface("mistralai/Mistral-7B-v0.1"))
//!     .isq(QuantizationLevel::Q4K)  // 4-bit quantization
//!     .paged_attention(true)        // Memory-efficient attention
//!     .build();
//!
//! let model = MistralRsModel::new(config).await?;
//! ```
//!
//! ## LoRA Adapter Example
//!
//! Load and hot-swap fine-tuned adapters:
//!
//! ```rust,ignore
//! use adk_mistralrs::{MistralRsAdapterModel, AdapterConfig, MistralRsConfig, ModelSource};
//!
//! let config = MistralRsConfig::builder()
//!     .model_source(ModelSource::huggingface("meta-llama/Llama-2-7b-hf"))
//!     .adapter(AdapterConfig::lora("username/my-lora-adapter"))
//!     .build();
//!
//! let model = MistralRsAdapterModel::new(config).await?;
//!
//! // List available adapters
//! println!("Adapters: {:?}", model.available_adapters());
//!
//! // Swap adapters at runtime
//! model.swap_adapter("another-adapter").await?;
//! ```
//!
//! ## Vision Model Example
//!
//! Process images with vision-language models:
//!
//! ```rust,ignore
//! use adk_mistralrs::MistralRsVisionModel;
//!
//! let model = MistralRsVisionModel::from_hf("microsoft/Phi-3.5-vision-instruct").await?;
//!
//! let image = image::open("photo.jpg")?;
//! let response = model.generate_with_image(
//!     "Describe this image in detail.",
//!     vec![image],
//! ).await?;
//! ```
//!
//! ## Multi-Model Serving Example
//!
//! Load multiple models and route requests:
//!
//! ```rust,ignore
//! use adk_mistralrs::{MistralRsMultiModel, MistralRsConfig, ModelSource};
//!
//! let mut multi = MistralRsMultiModel::new();
//!
//! // Add models
//! multi.add_model("phi", MistralRsConfig::builder()
//!     .model_source(ModelSource::huggingface("mistralai/Magistral-Small-2509"))
//!     .build()
//! ).await?;
//!
//! multi.add_model("llama", MistralRsConfig::builder()
//!     .model_source(ModelSource::huggingface("meta-llama/Llama-3.2-3B-Instruct"))
//!     .build()
//! ).await?;
//!
//! // Route requests by model name
//! let response = multi.generate_with_model(Some("llama"), request, false).await?;
//! ```
//!
//! ## Module Overview
//!
//! - [`client`] - Main [`MistralRsModel`] implementing the ADK [`Llm`] trait
//! - [`config`] - Configuration types: [`MistralRsConfig`], [`ModelSource`], [`QuantizationLevel`]
//! - [`adapter`] - LoRA/X-LoRA adapter support via [`MistralRsAdapterModel`]
//! - [`vision`] - Vision model support via [`MistralRsVisionModel`]
//! - [`embedding`] - Embedding model support via [`MistralRsEmbeddingModel`]
//! - [`speech`] - Speech synthesis via [`MistralRsSpeechModel`]
//! - [`diffusion`] - Image generation via [`MistralRsDiffusionModel`]
//! - [`multimodel`] - Multi-model serving via [`MistralRsMultiModel`]
//! - [`mcp`] - MCP client configuration via [`McpClientConfig`]
//! - [`convert`] - Type conversion utilities between ADK and mistral.rs
//! - [`error`] - Error types via [`MistralRsError`]
//!
//! ## Feature Flags
//!
//! Enable hardware acceleration with feature flags:
//!
//! ```toml
//! [dependencies]
//! adk-mistralrs = { git = "https://github.com/zavora-ai/adk-rust", features = ["cuda"] }
//! ```
//!
//! Available features:
//! - `cuda` - NVIDIA CUDA acceleration
//! - `metal` - Apple Metal acceleration (macOS)
//! - `mkl` - Intel MKL acceleration
//! - `accelerate` - Apple Accelerate framework
//! - `flash-attn` - Flash Attention (requires CUDA)

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
