//! Backend abstraction for Gemini API providers.
//!
//! This module defines the `GeminiBackend` trait that abstracts over different
//! Gemini API backends (AI Studio REST vs Vertex AI). Each backend handles
//! authentication, URL construction, and request dispatch independently.
//!
//! Inspired by [PR #74](https://github.com/zavora-ai/adk-rust/pull/74) by @mikefaille.

pub mod studio;
#[cfg(feature = "vertex")]
pub mod vertex;

use crate::{
    batch::model::{
        BatchGenerateContentRequest, BatchGenerateContentResponse, ListBatchesResponse,
    },
    cache::model::{
        CacheExpirationRequest, CachedContent, CreateCachedContentRequest,
        ListCachedContentsResponse,
    },
    client::Error,
    embedding::{
        BatchContentEmbeddingResponse, BatchEmbedContentsRequest, ContentEmbeddingResponse,
        EmbedContentRequest,
    },
    files::model::{File, ListFilesResponse},
    generation::{GenerateContentRequest, GenerationResponse},
    model_info::{ListModelsResponse, ModelInfo},
    tools::model::VertexRagStore,
};
use async_trait::async_trait;
use futures::Stream;
use mime::Mime;
use std::pin::Pin;

/// A boxed, pinned stream of results — the common return type for streaming operations.
pub type BackendStream<T> = Pin<Box<dyn Stream<Item = Result<T, Error>> + Send>>;

/// Trait defining the interface for Gemini backends (AI Studio REST vs Vertex AI).
///
/// Required methods cover the core operations (generate, stream, embed).
/// Optional operations (batch, files, cache) have default implementations that
/// return `GoogleCloudUnsupported`, so backends only implement what they support.
#[async_trait]
pub trait GeminiBackend: Send + Sync + std::fmt::Debug {
    // ── Core operations (required) ──────────────────────────────────────

    /// Generate content (non-streaming).
    async fn generate_content(
        &self,
        request: GenerateContentRequest,
    ) -> Result<GenerationResponse, Error>;

    /// Generate content with streaming SSE response.
    async fn generate_content_stream(
        &self,
        request: GenerateContentRequest,
    ) -> Result<BackendStream<GenerationResponse>, Error>;

    // ── Vertex RAG grounding (Vertex AI only) ──────────────────────────

    /// Generate content with a server-side Vertex RAG grounding declaration
    /// (non-streaming).
    ///
    /// The Vertex AI backend declares `store` as a `retrieval` tool on the
    /// outgoing request. The default implementation rejects the request —
    /// `vertexRagStore` retrieval is a Vertex AI capability.
    async fn generate_content_with_vertex_rag(
        &self,
        _request: GenerateContentRequest,
        _store: VertexRagStore,
    ) -> Result<GenerationResponse, Error> {
        Err(Error::Validation {
            message: "vertexRagStore retrieval requires the Vertex AI backend; construct the client with a Google Cloud configuration (e.g. Gemini::with_google_cloud_adc_model)".to_string(),
        })
    }

    /// Streaming variant of
    /// [`generate_content_with_vertex_rag`](Self::generate_content_with_vertex_rag).
    async fn generate_content_stream_with_vertex_rag(
        &self,
        _request: GenerateContentRequest,
        _store: VertexRagStore,
    ) -> Result<BackendStream<GenerationResponse>, Error> {
        Err(Error::Validation {
            message: "vertexRagStore retrieval requires the Vertex AI backend; construct the client with a Google Cloud configuration (e.g. Gemini::with_google_cloud_adc_model)".to_string(),
        })
    }

    /// Embed content.
    async fn embed_content(
        &self,
        request: EmbedContentRequest,
    ) -> Result<ContentEmbeddingResponse, Error>;

    // ── Batch embeddings ────────────────────────────────────────────────

    /// Embed multiple contents in a single request.
    async fn batch_embed_contents(
        &self,
        _request: BatchEmbedContentsRequest,
    ) -> Result<BatchContentEmbeddingResponse, Error> {
        Err(Error::GoogleCloudUnsupported { operation: "batchEmbedContents" })
    }

    // ── Batch generation ────────────────────────────────────────────────

    /// Submit a batch content generation request.
    async fn batch_generate_content(
        &self,
        _request: BatchGenerateContentRequest,
    ) -> Result<BatchGenerateContentResponse, Error> {
        Err(Error::GoogleCloudUnsupported { operation: "batchGenerateContent" })
    }

    /// Get the status of a batch operation by name.
    async fn get_batch_operation(&self, _name: &str) -> Result<serde_json::Value, Error> {
        Err(Error::GoogleCloudUnsupported { operation: "getBatchOperation" })
    }

    /// List batch operations with pagination.
    async fn list_batch_operations(
        &self,
        _page_size: Option<u32>,
        _page_token: Option<String>,
    ) -> Result<ListBatchesResponse, Error> {
        Err(Error::GoogleCloudUnsupported { operation: "listBatchOperations" })
    }

    /// Cancel a running batch operation.
    async fn cancel_batch_operation(&self, _name: &str) -> Result<(), Error> {
        Err(Error::GoogleCloudUnsupported { operation: "cancelBatchOperation" })
    }

    /// Delete a batch operation resource.
    async fn delete_batch_operation(&self, _name: &str) -> Result<(), Error> {
        Err(Error::GoogleCloudUnsupported { operation: "deleteBatchOperation" })
    }

    // ── File operations ─────────────────────────────────────────────────

    /// Upload a file to the Gemini API.
    async fn upload_file(
        &self,
        _display_name: Option<String>,
        _file_bytes: Vec<u8>,
        _mime_type: Mime,
    ) -> Result<File, Error> {
        Err(Error::GoogleCloudUnsupported { operation: "uploadFile" })
    }

    /// Get file metadata by name.
    async fn get_file(&self, _name: &str) -> Result<File, Error> {
        Err(Error::GoogleCloudUnsupported { operation: "getFile" })
    }

    /// Download file content by name.
    async fn download_file(&self, _name: &str) -> Result<Vec<u8>, Error> {
        Err(Error::GoogleCloudUnsupported { operation: "downloadFile" })
    }

    /// List files with pagination.
    async fn list_files(
        &self,
        _page_size: Option<u32>,
        _page_token: Option<String>,
    ) -> Result<ListFilesResponse, Error> {
        Err(Error::GoogleCloudUnsupported { operation: "listFiles" })
    }

    /// Delete a file by name.
    async fn delete_file(&self, _name: &str) -> Result<(), Error> {
        Err(Error::GoogleCloudUnsupported { operation: "deleteFile" })
    }

    // ── Cache operations ────────────────────────────────────────────────

    /// Create cached content.
    async fn create_cached_content(
        &self,
        _request: CreateCachedContentRequest,
    ) -> Result<CachedContent, Error> {
        Err(Error::GoogleCloudUnsupported { operation: "createCachedContent" })
    }

    /// Get cached content by name.
    async fn get_cached_content(&self, _name: &str) -> Result<CachedContent, Error> {
        Err(Error::GoogleCloudUnsupported { operation: "getCachedContent" })
    }

    /// List cached contents with pagination.
    async fn list_cached_contents(
        &self,
        _page_size: Option<i32>,
        _page_token: Option<String>,
    ) -> Result<ListCachedContentsResponse, Error> {
        Err(Error::GoogleCloudUnsupported { operation: "listCachedContents" })
    }

    /// Update cached content expiration.
    async fn update_cached_content(
        &self,
        _name: &str,
        _expiration: CacheExpirationRequest,
    ) -> Result<CachedContent, Error> {
        Err(Error::GoogleCloudUnsupported { operation: "updateCachedContent" })
    }

    /// Delete cached content by name.
    async fn delete_cached_content(&self, _name: &str) -> Result<(), Error> {
        Err(Error::GoogleCloudUnsupported { operation: "deleteCachedContent" })
    }

    // ── Model discovery ─────────────────────────────────────────────

    /// List available models. Returns a paginated list of model metadata.
    async fn list_models(
        &self,
        _page_size: Option<u32>,
        _page_token: Option<String>,
    ) -> Result<ListModelsResponse, Error> {
        Err(Error::GoogleCloudUnsupported { operation: "listModels" })
    }

    /// Get metadata for a specific model.
    async fn get_model(&self, _name: &str) -> Result<ModelInfo, Error> {
        Err(Error::GoogleCloudUnsupported { operation: "getModel" })
    }

    // ── Interactions API (Beta) ─────────────────────────────────────────

    /// Create an interaction (non-streaming).
    #[cfg(feature = "interactions")]
    async fn create_interaction(
        &self,
        _request: crate::interactions::CreateInteractionRequest,
    ) -> Result<crate::interactions::Interaction, Error> {
        Err(Error::GoogleCloudUnsupported { operation: "createInteraction" })
    }

    /// Create an interaction with a streaming SSE response.
    #[cfg(feature = "interactions")]
    async fn create_interaction_stream(
        &self,
        _request: crate::interactions::CreateInteractionRequest,
    ) -> Result<BackendStream<crate::interactions::InteractionSseEvent>, Error> {
        Err(Error::GoogleCloudUnsupported { operation: "createInteractionStream" })
    }

    /// Retrieve a stored interaction by ID.
    #[cfg(feature = "interactions")]
    async fn get_interaction(
        &self,
        _id: &str,
        _include_input: bool,
    ) -> Result<crate::interactions::Interaction, Error> {
        Err(Error::GoogleCloudUnsupported { operation: "getInteraction" })
    }

    /// Delete a stored interaction by ID.
    #[cfg(feature = "interactions")]
    async fn delete_interaction(&self, _id: &str) -> Result<(), Error> {
        Err(Error::GoogleCloudUnsupported { operation: "deleteInteraction" })
    }

    /// Cancel a running background interaction by ID.
    #[cfg(feature = "interactions")]
    async fn cancel_interaction(
        &self,
        _id: &str,
    ) -> Result<crate::interactions::Interaction, Error> {
        Err(Error::GoogleCloudUnsupported { operation: "cancelInteraction" })
    }

    /// Create a managed-agent configuration on the server.
    #[cfg(feature = "interactions")]
    async fn create_agent(
        &self,
        _request: crate::interactions::managed_agent::CreateAgentRequest,
    ) -> Result<crate::interactions::managed_agent::SavedAgent, Error> {
        Err(Error::GoogleCloudUnsupported { operation: "createAgent" })
    }

    /// List saved managed-agent configurations with pagination.
    #[cfg(feature = "interactions")]
    async fn list_agents(
        &self,
        _page_size: Option<u32>,
        _page_token: Option<String>,
    ) -> Result<crate::interactions::managed_agent::ListAgentsResponse, Error> {
        Err(Error::GoogleCloudUnsupported { operation: "listAgents" })
    }

    /// Retrieve a single saved managed-agent configuration by ID.
    #[cfg(feature = "interactions")]
    async fn get_agent(
        &self,
        _id: &str,
    ) -> Result<crate::interactions::managed_agent::SavedAgent, Error> {
        Err(Error::GoogleCloudUnsupported { operation: "getAgent" })
    }

    /// Delete a saved managed-agent configuration by ID.
    #[cfg(feature = "interactions")]
    async fn delete_agent(&self, _id: &str) -> Result<(), Error> {
        Err(Error::GoogleCloudUnsupported { operation: "deleteAgent" })
    }

    /// Download an environment snapshot as raw bytes given an environment ID.
    #[cfg(feature = "interactions")]
    async fn download_environment(&self, _env_id: &str) -> Result<Vec<u8>, Error> {
        Err(Error::GoogleCloudUnsupported { operation: "downloadEnvironment" })
    }
}
