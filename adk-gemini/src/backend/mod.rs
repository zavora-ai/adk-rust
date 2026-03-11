//! Backend abstraction for Gemini API providers.
//!
//! This module defines the [`GeminiBackend`] trait that abstracts over different
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

    /// Embed content.
    async fn embed_content(
        &self,
        request: EmbedContentRequest,
    ) -> Result<ContentEmbeddingResponse, Error>;

    // ── Batch embeddings ────────────────────────────────────────────────

    async fn batch_embed_contents(
        &self,
        _request: BatchEmbedContentsRequest,
    ) -> Result<BatchContentEmbeddingResponse, Error> {
        Err(Error::GoogleCloudUnsupported { operation: "batchEmbedContents" })
    }

    // ── Batch generation ────────────────────────────────────────────────

    async fn batch_generate_content(
        &self,
        _request: BatchGenerateContentRequest,
    ) -> Result<BatchGenerateContentResponse, Error> {
        Err(Error::GoogleCloudUnsupported { operation: "batchGenerateContent" })
    }

    async fn get_batch_operation(&self, _name: &str) -> Result<serde_json::Value, Error> {
        Err(Error::GoogleCloudUnsupported { operation: "getBatchOperation" })
    }

    async fn list_batch_operations(
        &self,
        _page_size: Option<u32>,
        _page_token: Option<String>,
    ) -> Result<ListBatchesResponse, Error> {
        Err(Error::GoogleCloudUnsupported { operation: "listBatchOperations" })
    }

    async fn cancel_batch_operation(&self, _name: &str) -> Result<(), Error> {
        Err(Error::GoogleCloudUnsupported { operation: "cancelBatchOperation" })
    }

    async fn delete_batch_operation(&self, _name: &str) -> Result<(), Error> {
        Err(Error::GoogleCloudUnsupported { operation: "deleteBatchOperation" })
    }

    // ── File operations ─────────────────────────────────────────────────

    async fn upload_file(
        &self,
        _display_name: Option<String>,
        _file_bytes: Vec<u8>,
        _mime_type: Mime,
    ) -> Result<File, Error> {
        Err(Error::GoogleCloudUnsupported { operation: "uploadFile" })
    }

    async fn get_file(&self, _name: &str) -> Result<File, Error> {
        Err(Error::GoogleCloudUnsupported { operation: "getFile" })
    }

    async fn download_file(&self, _name: &str) -> Result<Vec<u8>, Error> {
        Err(Error::GoogleCloudUnsupported { operation: "downloadFile" })
    }

    async fn list_files(
        &self,
        _page_size: Option<u32>,
        _page_token: Option<String>,
    ) -> Result<ListFilesResponse, Error> {
        Err(Error::GoogleCloudUnsupported { operation: "listFiles" })
    }

    async fn delete_file(&self, _name: &str) -> Result<(), Error> {
        Err(Error::GoogleCloudUnsupported { operation: "deleteFile" })
    }

    // ── Cache operations ────────────────────────────────────────────────

    async fn create_cached_content(
        &self,
        _request: CreateCachedContentRequest,
    ) -> Result<CachedContent, Error> {
        Err(Error::GoogleCloudUnsupported { operation: "createCachedContent" })
    }

    async fn get_cached_content(&self, _name: &str) -> Result<CachedContent, Error> {
        Err(Error::GoogleCloudUnsupported { operation: "getCachedContent" })
    }

    async fn list_cached_contents(
        &self,
        _page_size: Option<i32>,
        _page_token: Option<String>,
    ) -> Result<ListCachedContentsResponse, Error> {
        Err(Error::GoogleCloudUnsupported { operation: "listCachedContents" })
    }

    async fn update_cached_content(
        &self,
        _name: &str,
        _expiration: CacheExpirationRequest,
    ) -> Result<CachedContent, Error> {
        Err(Error::GoogleCloudUnsupported { operation: "updateCachedContent" })
    }

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
}
