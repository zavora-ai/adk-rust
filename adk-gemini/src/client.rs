use futures::Stream;
use futures::stream::BoxStream;
use mime::Mime;
use std::sync::Arc;

use crate::Model;
use crate::backend::GeminiBackend;
use crate::batch::{BatchBuilder, BatchHandle};
use crate::builder::GeminiBuilder;
use crate::cache::{CacheBuilder, CachedContentHandle};
use crate::embedding::{
    BatchContentEmbeddingResponse, BatchEmbedContentsRequest, ContentEmbeddingResponse,
    EmbedBuilder, EmbedContentRequest,
};
use crate::error::Error;
use crate::files::{handle::FileHandle, model::File};
use crate::generation::{ContentBuilder, GenerateContentRequest, GenerationResponse};

#[cfg(feature = "vertex")]
pub use crate::backend::vertex::extract_service_account_project_id;

/// The main entry point for interacting with the Gemini API.
///
/// This client provides a high-level interface for generating content, managing files,
/// and working with other Gemini resources. It supports both the Google AI Studio API
/// and Vertex AI (Google Cloud).
///
/// The client uses an internal backend implementation (Studio vs Vertex) to handle
/// the specifics of each API.
#[derive(Clone, Debug)]
pub struct GeminiClient {
    backend: Arc<dyn GeminiBackend>,
}

impl GeminiClient {
    /// Internal constructor used by the Builder
    pub fn with_backend(backend: Arc<dyn GeminiBackend>) -> Self {
        Self { backend }
    }

    /// Create a new client with the given API key.
    ///
    /// This uses the default model and base URL.
    pub fn new(key: impl Into<String>) -> Result<Self, Error> {
        GeminiBuilder::new(key).build()
    }

    /// Create a new client with the given API key and model.
    pub fn with_model(key: impl Into<String>, model: impl Into<String>) -> Result<Self, Error> {
        GeminiBuilder::new(key).with_model(model.into()).build()
    }

    /// Returns the model name used by this client.
    pub fn model(&self) -> &str {
        self.backend.model()
    }

    /// Create a new client with the given API key. (Alias for backward compatibility)
    pub fn new_with_api_key(key: impl Into<String>) -> Self {
        Self::new(key).expect("failed to build default client")
    }

    /// Create a new client using the Gemini 2.5 Pro model.
    pub fn pro(key: impl Into<String>) -> Result<Self, Error> {
        GeminiBuilder::new(key).with_model(Model::Gemini25Pro).build()
    }

    /// Create a new client using the Gemini 2.5 Flash model.
    pub fn flash(key: impl Into<String>) -> Result<Self, Error> {
        GeminiBuilder::new(key).with_model(Model::Gemini25Flash).build()
    }

    /// Create a new client with the given API key, model, and base URL.
    pub fn with_model_and_base_url(
        key: impl Into<String>,
        model: impl Into<String>,
        base_url: reqwest::Url,
    ) -> Result<Self, Error> {
        GeminiBuilder::new(key).with_model(model.into()).with_base_url(base_url).build()
    }

    // --- Content Generation ---

    /// Start building a content generation request using a fluent API.
    pub fn generate_content(&self) -> ContentBuilder {
        ContentBuilder::new(Arc::new(self.clone()))
    }

    /// Generate content (unary).
    pub(crate) async fn generate_content_raw(
        &self,
        req: GenerateContentRequest,
    ) -> Result<GenerationResponse, Error> {
        req.validate().map_err(|e| Error::Validation { source: Box::new(e) })?;
        self.backend.generate_content(req).await
    }

    /// Generate content (streaming).
    pub(crate) async fn generate_content_stream(
        &self,
        req: GenerateContentRequest,
    ) -> Result<BoxStream<'static, Result<GenerationResponse, Error>>, Error> {
        req.validate().map_err(|e| Error::Validation { source: Box::new(e) })?;
        self.backend.generate_content_stream(req).await
    }

    /// Count tokens for a given request.
    pub async fn count_tokens(&self, req: GenerateContentRequest) -> Result<u32, Error> {
        self.backend.count_tokens(req).await
    }

    // --- Text Embeddings ---

    /// Start building a text embedding request using a fluent API.
    pub fn embed_content(&self) -> EmbedBuilder {
        EmbedBuilder::new(Arc::new(self.clone()))
    }

    /// Embed content (raw).
    pub(crate) async fn embed_content_raw(
        &self,
        req: EmbedContentRequest,
    ) -> Result<ContentEmbeddingResponse, Error> {
        self.backend.embed_content(req).await
    }

    /// Batch embed content.
    pub(crate) async fn embed_content_batch(
        &self,
        req: BatchEmbedContentsRequest,
    ) -> Result<BatchContentEmbeddingResponse, Error> {
        self.backend.batch_embed_contents(req).await
    }

    // --- Batch Operations ---

    /// Start building a batch generation request using a fluent API.
    pub fn batch_generate_content(&self) -> BatchBuilder {
        BatchBuilder::new(Arc::new(self.clone()))
    }

    /// Create a batch generation operation (raw).
    pub(crate) async fn batch_generate_content_raw(
        &self,
        req: crate::batch::model::BatchGenerateContentRequest,
    ) -> Result<crate::batch::model::BatchOperation, Error> {
        self.backend.create_batch(req).await
    }

    /// Get a handle to an existing batch operation.
    pub fn get_batch(&self, name: &str) -> BatchHandle {
        BatchHandle::new(name.to_string(), Arc::new(self.clone()))
    }

    /// Get details of a batch operation.
    pub(crate) async fn get_batch_operation(
        &self,
        name: &str,
    ) -> Result<crate::batch::model::BatchOperation, Error> {
        self.backend.get_batch(name).await
    }

    /// Lists batch operations.
    pub fn list_batches(
        &self,
        page_size: impl Into<Option<u32>>,
    ) -> impl Stream<Item = Result<crate::batch::model::BatchOperation, Error>> + Send {
        let client = self.backend.clone();
        let page_size = page_size.into();
        async_stream::try_stream! {
            let mut page_token: Option<String> = None;
            loop {
                let response = client
                    .list_batches(page_size, page_token.clone())
                    .await?;

                for operation in response.operations {
                    yield operation as crate::batch::model::BatchOperation;
                }

                if let Some(next_page_token) = response.next_page_token {
                    page_token = Some(next_page_token);
                } else {
                    break;
                }
            }
        }
    }

    pub(crate) async fn cancel_batch_operation(&self, name: &str) -> Result<(), Error> {
        self.backend.cancel_batch(name).await
    }

    pub(crate) async fn delete_batch_operation(&self, name: &str) -> Result<(), Error> {
        self.backend.delete_batch(name).await
    }

    // --- Content Caching ---

    /// Start building a cached content request using a fluent API.
    pub fn create_cache(&self) -> CacheBuilder {
        CacheBuilder::new(Arc::new(self.clone()))
    }

    /// Get a handle to existing cached content.
    pub fn get_cached_content(&self, name: &str) -> CachedContentHandle {
        CachedContentHandle::new(name.to_string(), Arc::new(self.clone()))
    }

    /// Create cached content (raw).
    pub(crate) async fn create_cached_content_raw(
        &self,
        req: crate::cache::model::CreateCachedContentRequest,
    ) -> Result<crate::cache::model::CachedContent, Error> {
        self.backend.create_cached_content(req).await
    }

    /// Get details of cached content.
    pub(crate) async fn get_cached_content_raw(
        &self,
        name: &str,
    ) -> Result<crate::cache::model::CachedContent, Error> {
        self.backend.get_cached_content(name).await
    }

    /// Update cached content expiration.
    pub(crate) async fn update_cached_content(
        &self,
        name: &str,
        expiration: crate::cache::model::CacheExpirationRequest,
    ) -> Result<crate::cache::model::CachedContent, Error> {
        self.backend.update_cached_content(name, expiration).await
    }

    /// Delete cached content.
    pub(crate) async fn delete_cached_content(&self, name: &str) -> Result<(), Error> {
        self.backend.delete_cached_content(name).await
    }

    /// Lists cached contents.
    pub fn list_cached_contents(
        &self,
        page_size: impl Into<Option<i32>>,
    ) -> impl Stream<Item = Result<crate::cache::model::CachedContentSummary, Error>> + Send {
        let client = self.backend.clone();
        let page_size = page_size.into();
        async_stream::try_stream! {
            let mut page_token: Option<String> = None;
            loop {
                let response = client
                    .list_cached_contents(page_size, page_token.clone())
                    .await?;

                for cached_content in response.cached_contents {
                    yield cached_content as crate::cache::model::CachedContentSummary;
                }

                if let Some(next_page_token) = response.next_page_token {
                    page_token = Some(next_page_token);
                } else {
                    break;
                }
            }
        }
    }

    // --- File Management ---

    /// Start building a file upload request using a fluent API.
    pub fn create_file<B: Into<Vec<u8>>>(&self, bytes: B) -> crate::files::builder::FileBuilder {
        crate::files::builder::FileBuilder::new(Arc::new(self.clone()), bytes)
    }

    /// Get a handle to an existing file.
    pub async fn get_file(&self, name: &str) -> Result<FileHandle, Error> {
        let file = self.backend.get_file(name).await?;
        Ok(FileHandle::new(Arc::new(self.clone()), file))
    }

    /// Upload a file (raw).
    pub(crate) async fn upload_file(
        &self,
        display_name: Option<String>,
        file_bytes: Vec<u8>,
        mime_type: Mime,
    ) -> Result<File, Error> {
        self.backend.upload_file(display_name, file_bytes, mime_type).await
    }

    /// Delete a file.
    pub(crate) async fn delete_file(&self, name: &str) -> Result<(), Error> {
        self.backend.delete_file(name).await
    }

    /// Download a file (returns raw bytes).
    pub(crate) async fn download_file(&self, name: &str) -> Result<Vec<u8>, Error> {
        self.backend.download_file(name).await
    }

    /// Lists files.
    pub fn list_files(
        &self,
        page_size: impl Into<Option<u32>>,
    ) -> impl Stream<Item = Result<FileHandle, Error>> + Send {
        let client = Arc::new(self.clone());
        let backend = self.backend.clone();
        let page_size = page_size.into();
        async_stream::try_stream! {
            let mut page_token: Option<String> = None;
            loop {
                let response = backend
                    .list_files(page_size, page_token.clone())
                    .await?;

                for file in response.files {
                    yield FileHandle::new(client.clone(), file) as FileHandle;
                }

                if let Some(next_page_token) = response.next_page_token {
                    page_token = Some(next_page_token);
                } else {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
#[cfg(feature = "vertex")]
mod client_tests {
    use super::{Error, extract_service_account_project_id};

    #[test]
    fn extract_service_account_project_id_reads_project_id() {
        let json = r#"{
            "type": "service_account",
            "project_id": "test-project-123",
            "private_key_id": "key-id"
        }"#;

        let project_id = extract_service_account_project_id(json).expect("project id should parse");
        assert_eq!(project_id, "test-project-123");
    }

    #[test]
    fn extract_service_account_project_id_missing_field_errors() {
        let json = r#"{
            "type": "service_account",
            "private_key_id": "key-id"
        }"#;

        let err =
            extract_service_account_project_id(json).expect_err("missing project_id should fail");
        assert!(matches!(err, Error::MissingGoogleCloudProjectId));
    }

    #[test]
    fn extract_service_account_project_id_invalid_json_errors() {
        let err =
            extract_service_account_project_id("not-json").expect_err("invalid json should fail");
        assert!(matches!(err, Error::GoogleCloudCredentialParse { .. }));
    }
}
