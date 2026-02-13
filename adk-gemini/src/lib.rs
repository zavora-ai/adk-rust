//! # adk-gemini
//!
//! A Rust client library for Google's Gemini 2.0 API.
//!
//! ## Crate Organization
//!
//! This crate is organized into domain-specific modules that align with the Gemini API's
//! capabilities:
//!
//! - **`generation`** - Content generation, including text, images, and audio
//! - **`embedding`** - Text embedding generation for semantic analysis
//! - **`batch`** - Batch processing for multiple requests
//! - **`files`** - File upload and management
//! - **`cache`** - Content caching for reusable contexts
//! - **`safety`** - Content moderation and safety settings
//! - **`tools`** - Function calling and tool integration
//! - **`types`** - Core primitive types shared across modules
//! - **`prelude`** - Convenient re-exports of commonly used types
//!
//! ## Quick Start
//!
//! For most use cases, import from the prelude:
//!
//! ```rust
//! use adk_gemini::prelude::*;
//! ```
//!
//! For more specialized types, import them directly from the crate root or their
//! respective modules.

// Internal macro module — must be declared before any module that uses it
#[macro_use]
mod macros;

pub mod backend;
pub mod builder;
pub mod client;
pub mod common;
pub mod error;
mod types;

/// Convenient re-exports of commonly used types
pub mod prelude;

/// Batch processing for multiple generation requests
pub mod batch;

/// Content caching for reusable contexts and system instructions
pub mod cache;

/// Text embedding generation for semantic analysis
pub mod embedding;

/// File upload and management
pub mod files;

/// Content generation including text, images, and audio
pub mod generation;

/// Content moderation and safety settings
pub mod safety;

/// Function calling and tool integration
pub mod tools;

// ========== Core Types ==========
// These are the fundamental types used throughout the API

/// Builder for creating a new Gemini client
pub use builder::GeminiBuilder;
/// The main Gemini API client
pub use client::GeminiClient as Gemini;
/// The main Gemini API client (as GeminiClient)
pub use client::GeminiClient;
/// Available Gemini models
pub use common::Model;
/// The main client error type
pub use error::Error as ClientError;
/// Re-export google_cloud_auth credentials for downstream crates (VertexADC)
#[cfg(feature = "vertex")]
pub use google_cloud_auth::credentials;
/// Configuration for Gemini Live backend (Public or Vertex)
pub use types::GeminiLiveBackend;
/// Context for Vertex AI backend (project_id, location, token)
#[cfg(feature = "vertex")]
pub use types::VertexContext;

/// Core primitive types for building requests and parsing responses
pub use types::{Blob, CodeExecutionResultData, Content, Message, Modality, Part, Role};

// ========== Content Generation ==========
// Types for generating text, images, and audio content

pub use generation::{
    builder::ContentBuilder, model::BlockReason, model::Candidate, model::CitationMetadata,
    model::CitationSource, model::FinishReason, model::GenerateContentRequest,
    model::GenerationConfig, model::GenerationResponse, model::GroundingChunk,
    model::GroundingMetadata, model::GroundingSegment, model::GroundingSupport,
    model::MultiSpeakerVoiceConfig, model::PrebuiltVoiceConfig, model::PromptFeedback,
    model::PromptTokenDetails, model::SpeakerVoiceConfig, model::SpeechConfig,
    model::ThinkingConfig, model::UsageMetadata, model::VoiceConfig, model::WebGroundingChunk,
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
    FunctionResponse, Tool, ToolConfig,
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
