#![deny(missing_docs)]

//! # adk-gemini
//!
//! A Rust client for the Google Gemini API, on Google AI Studio and Vertex AI. It is the client
//! behind `adk_model::GeminiModel`.
//!
//! ## Quick start
//!
//! ```rust,no_run
//! use adk_gemini::prelude::*;
//!
//! # #[tokio::main]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let client = Gemini::new(std::env::var("GEMINI_API_KEY")?)?;
//!
//! let response = client
//!     .generate_content()
//!     .with_user_message("Name one planet in a short sentence.")
//!     .execute()
//!     .await?;
//! println!("{}", response.text());
//! # Ok(())
//! # }
//! ```
//!
//! [`Gemini::new`] uses `gemini-3.7-flash`; [`Gemini::with_model`] takes any [`Model`].
//!
//! ## Crate organization
//!
//! | Module | Contents |
//! |--------|----------|
//! | [`generation`] | Content generation, including text, images, and audio |
//! | [`embedding`] | Embeddings |
//! | [`batch`] | Batch processing |
//! | [`files`] | File upload and management |
//! | [`cache`] | Context caching |
//! | [`safety`] | Safety settings and ratings |
//! | [`tools`] | Function declarations and built-in tools |
//! | [`backend`] | The [`backend::GeminiBackend`] trait and the AI Studio and Vertex AI backends |
//! | [`model_info`] | Model discovery |
//! | [`pricing`] | Token cost estimation |
//! | [`schema_adapter`] | Function-declaration schema normalization |
//! | `interactions` | The Interactions API (Beta), behind the `interactions` feature |
//! | [`prelude`] | Re-exports of commonly used types |
//!
//! ## Features
//!
//! | Feature | Enables |
//! |---------|---------|
//! | `studio` (default) | Marker only; the AI Studio backend is always compiled |
//! | `vertex` | Vertex AI constructors such as `Gemini::with_google_cloud_adc` |
//! | `interactions` | The Interactions API module and client methods |
//! | `backtrace` | `snafu`'s `backtraces-impl-backtrace-crate` feature |

/// Backend abstraction for Gemini API providers (AI Studio REST vs Vertex AI).
pub mod backend;
/// Gemini API client, builder, model enum, and error types.
pub mod client;
/// Model metadata discovery (list/get model info).
pub mod model_info;
mod models;
/// Token cost estimation for Gemini models.
pub mod pricing;
/// Provider-specific schema normalization for Gemini function-calling API.
pub mod schema_adapter;

/// Convenient re-exports of commonly used types
pub mod prelude;

/// Batch processing for multiple generation requests
pub mod batch;

/// Content caching for reusable contexts and system instructions
pub mod cache;

/// Common utilities and serialization helpers
pub mod common;

/// Text embedding generation for semantic analysis
pub mod embedding;

/// File upload and management
pub mod files;

/// Content generation including text, images, and audio
pub mod generation;

/// Gemini Interactions API (Beta) — stateful, step-based agentic interactions.
#[cfg(feature = "interactions")]
pub mod interactions;

/// Content moderation and safety settings
pub mod safety;

/// Function calling and tool integration
pub mod tools;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod response_parsing_tests;

// ========== Core Types ==========
// These are the fundamental types used throughout the API

/// The main client error type
pub use client::Error as ClientError;
/// The main Gemini API client
pub use client::Gemini;
/// Builder for creating a new Gemini client
pub use client::GeminiBuilder;
/// Available Gemini models
pub use client::Model;

/// Core primitive types for building requests and parsing responses
pub use models::{Blob, Content, FileDataRef, Message, Modality, Part, Role};

// ========== Content Generation ==========
// Types for generating text, images, and audio content

pub use generation::{
    builder::ContentBuilder, model::BlockReason, model::Candidate, model::CitationMetadata,
    model::CitationSource, model::FinishReason, model::GenerateContentRequest,
    model::GenerationConfig, model::GenerationResponse, model::GroundingChunk,
    model::GroundingMetadata, model::GroundingSegment, model::GroundingSupport,
    model::MultiSpeakerVoiceConfig, model::PrebuiltVoiceConfig, model::PromptFeedback,
    model::PromptTokenDetails, model::SearchEntryPoint, model::SpeakerVoiceConfig,
    model::SpeechConfig, model::ThinkingConfig, model::ThinkingLevel, model::UsageMetadata,
    model::VoiceConfig, model::WebGroundingChunk,
};

// ========== Text Embeddings ==========
// Types for generating and working with text embeddings

pub use embedding::{
    builder::EmbedBuilder, model::BatchContentEmbeddingResponse, model::BatchEmbedContentsRequest,
    model::ContentEmbedding, model::ContentEmbeddingResponse, model::EmbedContentRequest,
    model::TaskType,
};

// ========== Safety & Content Filtering ==========
// Types for content moderation and safety settings

pub use safety::model::{
    HarmBlockThreshold, HarmCategory, HarmProbability, SafetyRating, SafetySetting,
};

// ========== Function Calling & Tools ==========
// Types for integrating external tools and function calling

pub use tools::model::{
    FunctionCall, FunctionCallingConfig, FunctionCallingMode, FunctionDeclaration,
    FunctionResponse, FunctionResponsePart, RagResource, RagRetrievalConfig, RagRetrievalFilter,
    Tool, ToolConfig, VertexRagStore,
};

// ========== Batch Processing ==========
// Types for processing multiple requests in batch operations

pub use batch::{
    Error as BatchError, builder::BatchBuilder, handle::BatchGenerationResponseItem,
    handle::BatchHandle, handle::BatchHandle as Batch, handle::BatchStatus,
    handle::Error as BatchHandleError, model::BatchConfig, model::BatchGenerateContentRequest,
    model::BatchOperation, model::BatchStats, model::IndividualRequestError,
    model::RequestMetadata,
};

// ========== File Management ==========
// Types for uploading and managing files

pub use files::{
    Error as FilesError, builder::FileBuilder, handle::FileHandle, model::File, model::FileState,
};

// ========== Content Caching ==========
// Types for caching contexts and system instructions

pub use cache::{
    builder::CacheBuilder, handle::CachedContentHandle, model::CacheExpirationRequest,
    model::CacheExpirationResponse, model::CachedContent, model::CreateCachedContentRequest,
};

// ========== Pricing ==========
// Token cost estimation for Gemini models

pub use pricing::{CostBreakdown, GeminiPricing, estimate_cost, estimate_cost_long};

// ========== Schema Adapter ==========
// Provider-specific schema normalization for Gemini function-calling API

pub use schema_adapter::{GeminiSchemaAdapter, GeminiSchemaDialect};
