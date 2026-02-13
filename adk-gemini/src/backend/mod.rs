pub mod studio;
#[cfg(feature = "vertex")]
pub mod vertex;

use crate::{
    batch::model::{BatchGenerateContentRequest, BatchOperation, ListBatchesResponse},
    cache::model::{
        CacheExpirationRequest, CachedContent, CreateCachedContentRequest,
        ListCachedContentsResponse,
    },
    embedding::model::{
        BatchContentEmbeddingResponse, BatchEmbedContentsRequest, ContentEmbeddingResponse,
        EmbedContentRequest,
    },
    error::Error,
    files::model::{File, ListFilesResponse},
    generation::model::{GenerateContentRequest, GenerationResponse},
};
use async_trait::async_trait;
use futures::Stream;
use std::pin::Pin;

pub type BackendStream<T> = Pin<Box<dyn Stream<Item = Result<T, Error>> + Send>>;

/// Trait defining the interface for Gemini backends (Studio vs Vertex).
/// This ensures calling code never needs to know which backend is active.
#[async_trait]
pub trait GeminiBackend: Send + Sync + std::fmt::Debug {
    /// Get the model name associated with this backend
    fn model(&self) -> &str;

    /// Generate content (text, images, etc.)
    async fn generate_content(
        &self,
        req: GenerateContentRequest,
    ) -> Result<GenerationResponse, Error>;

    /// Generate content with streaming response
    async fn generate_content_stream(
        &self,
        req: GenerateContentRequest,
    ) -> Result<BackendStream<GenerationResponse>, Error>;

    /// Count tokens
    async fn count_tokens(&self, req: GenerateContentRequest) -> Result<u32, Error>;

    /// Generate text embeddings
    async fn embed_content(
        &self,
        req: EmbedContentRequest,
    ) -> Result<ContentEmbeddingResponse, Error>;

    /// Generate batch text embeddings
    async fn batch_embed_contents(
        &self,
        req: BatchEmbedContentsRequest,
    ) -> Result<BatchContentEmbeddingResponse, Error>;

    // --- Batch Operations ---

    /// Create a batch prediction job
    async fn create_batch(
        &self,
        _req: BatchGenerateContentRequest,
    ) -> Result<BatchOperation, Error> {
        Err(Error::GoogleCloudUnsupported { operation: "createBatch" })
    }

    /// Get a batch prediction job
    async fn get_batch(&self, _name: &str) -> Result<BatchOperation, Error> {
        Err(Error::GoogleCloudUnsupported { operation: "getBatch" })
    }

    /// List batch prediction jobs
    async fn list_batches(
        &self,
        _page_size: Option<u32>,
        _page_token: Option<String>,
    ) -> Result<ListBatchesResponse, Error> {
        Err(Error::GoogleCloudUnsupported { operation: "listBatches" })
    }

    /// Cancel a batch prediction job
    async fn cancel_batch(&self, _name: &str) -> Result<(), Error> {
        Err(Error::GoogleCloudUnsupported { operation: "cancelBatch" })
    }

    /// Delete a batch prediction job
    async fn delete_batch(&self, _name: &str) -> Result<(), Error> {
        Err(Error::GoogleCloudUnsupported { operation: "deleteBatch" })
    }

    // --- File Operations ---

    /// Upload a file
    async fn upload_file(
        &self,
        _display_name: Option<String>,
        _bytes: Vec<u8>,
        _mime_type: mime::Mime,
    ) -> Result<File, Error> {
        Err(Error::GoogleCloudUnsupported { operation: "uploadFile" })
    }

    /// Get a file (metadata)
    async fn get_file(&self, _name: &str) -> Result<File, Error> {
        Err(Error::GoogleCloudUnsupported { operation: "getFile" })
    }

    /// Download a file (content)
    async fn download_file(&self, _name: &str) -> Result<Vec<u8>, Error> {
        Err(Error::GoogleCloudUnsupported { operation: "downloadFile" })
    }

    /// List files
    async fn list_files(
        &self,
        _page_size: Option<u32>,
        _page_token: Option<String>,
    ) -> Result<ListFilesResponse, Error> {
        Err(Error::GoogleCloudUnsupported { operation: "listFiles" })
    }

    /// Delete a file
    async fn delete_file(&self, _name: &str) -> Result<(), Error> {
        Err(Error::GoogleCloudUnsupported { operation: "deleteFile" })
    }

    // --- Cache Operations ---

    /// Create cached content
    async fn create_cached_content(
        &self,
        _req: CreateCachedContentRequest,
    ) -> Result<CachedContent, Error> {
        Err(Error::GoogleCloudUnsupported { operation: "createCachedContent" })
    }

    /// Get cached content
    async fn get_cached_content(&self, _name: &str) -> Result<CachedContent, Error> {
        Err(Error::GoogleCloudUnsupported { operation: "getCachedContent" })
    }

    /// List cached contents
    async fn list_cached_contents(
        &self,
        _page_size: Option<i32>,
        _page_token: Option<String>,
    ) -> Result<ListCachedContentsResponse, Error> {
        Err(Error::GoogleCloudUnsupported { operation: "listCachedContents" })
    }

    /// Update cached content expiration
    async fn update_cached_content(
        &self,
        _name: &str,
        _req: CacheExpirationRequest,
    ) -> Result<CachedContent, Error> {
        Err(Error::GoogleCloudUnsupported { operation: "updateCachedContent" })
    }

    /// Delete cached content
    async fn delete_cached_content(&self, _name: &str) -> Result<(), Error> {
        Err(Error::GoogleCloudUnsupported { operation: "deleteCachedContent" })
    }
}
