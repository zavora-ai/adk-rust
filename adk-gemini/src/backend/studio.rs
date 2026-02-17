//! AI Studio REST backend for the Gemini API.
//!
//! This backend communicates with `generativelanguage.googleapis.com` using
//! API-key authentication and standard REST/SSE endpoints.

use super::{BackendStream, GeminiBackend};
use crate::{
    batch::model::{
        BatchGenerateContentRequest, BatchGenerateContentResponse, ListBatchesResponse,
    },
    cache::model::{
        CacheExpirationRequest, CachedContent, CreateCachedContentRequest,
        ListCachedContentsResponse,
    },
    client::{
        BadResponseSnafu, ConstructUrlSnafu, DecodeResponseSnafu, DeserializeSnafu, Error,
        InvalidApiKeySnafu, MissingResponseHeaderSnafu, Model, PerformRequestNewSnafu,
        UrlParseSnafu,
    },
    embedding::{
        BatchContentEmbeddingResponse, BatchEmbedContentsRequest, ContentEmbeddingResponse,
        EmbedContentRequest,
    },
    files::model::{File, ListFilesResponse},
    generation::{GenerateContentRequest, GenerationResponse},
};
use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures::TryStreamExt;
use mime::Mime;
use reqwest::{
    Client, Response,
    header::{HeaderMap, HeaderName, HeaderValue},
};
use serde_json::json;
use snafu::{OptionExt, ResultExt};
use std::sync::LazyLock;
use url::Url;

static DEFAULT_BASE_URL: LazyLock<Url> = LazyLock::new(|| {
    Url::parse("https://generativelanguage.googleapis.com/v1beta/")
        .expect("unreachable error: failed to parse default base URL")
});

static V1_BASE_URL: LazyLock<Url> = LazyLock::new(|| {
    Url::parse("https://generativelanguage.googleapis.com/v1/")
        .expect("unreachable error: failed to parse v1 base URL")
});

/// Returns the default (v1beta) base URL.
pub fn default_base_url() -> &'static Url {
    &DEFAULT_BASE_URL
}

/// Returns the v1 (stable) base URL.
pub fn v1_base_url() -> &'static Url {
    &V1_BASE_URL
}

/// AI Studio REST backend.
#[derive(Debug)]
pub struct StudioBackend {
    pub(crate) http_client: Client,
    pub(crate) base_url: Url,
    pub(crate) model: Model,
}

impl StudioBackend {
    /// Create a new Studio backend with the given API key, model, and base URL.
    pub fn new(api_key: &str, model: Model, base_url: Url) -> Result<Self, Error> {
        let headers = HeaderMap::from_iter([(
            HeaderName::from_static("x-goog-api-key"),
            HeaderValue::from_str(api_key).context(InvalidApiKeySnafu)?,
        )]);

        let http_client = Client::builder()
            .default_headers(headers)
            .build()
            .expect("all parameters must be valid");

        Ok(Self { http_client, base_url, model })
    }

    /// Create with a custom `reqwest::Client` (e.g. for proxy support).
    pub fn with_client(http_client: Client, model: Model, base_url: Url) -> Self {
        Self { http_client, base_url, model }
    }

    // ── URL helpers ─────────────────────────────────────────────────────

    fn build_url_with_suffix(&self, suffix: &str) -> Result<Url, Error> {
        self.base_url.join(suffix).context(ConstructUrlSnafu { suffix: suffix.to_string() })
    }

    fn build_url(&self, endpoint: &str) -> Result<Url, Error> {
        let suffix = format!("{}:{endpoint}", self.model);
        self.build_url_with_suffix(&suffix)
    }

    fn build_batch_url(&self, name: &str, action: Option<&str>) -> Result<Url, Error> {
        let suffix = action.map(|a| format!("{name}:{a}")).unwrap_or_else(|| name.to_string());
        self.build_url_with_suffix(&suffix)
    }

    fn build_files_url(&self, name: Option<&str>) -> Result<Url, Error> {
        let suffix = name
            .map(|n| format!("files/{}", n.strip_prefix("files/").unwrap_or(n)))
            .unwrap_or_else(|| "files".to_string());
        self.build_url_with_suffix(&suffix)
    }

    fn build_cache_url(&self, name: Option<&str>) -> Result<Url, Error> {
        let suffix = name
            .map(|n| {
                if n.starts_with("cachedContents/") {
                    n.to_string()
                } else {
                    format!("cachedContents/{n}")
                }
            })
            .unwrap_or_else(|| "cachedContents".to_string());
        self.build_url_with_suffix(&suffix)
    }

    // ── Request helpers ─────────────────────────────────────────────────

    async fn check_response(response: Response) -> Result<Response, Error> {
        let status = response.status();
        if !status.is_success() {
            let description = response.text().await.ok();
            BadResponseSnafu { code: status.as_u16(), description }.fail()
        } else {
            Ok(response)
        }
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, url: Url) -> Result<T, Error> {
        let response = self.http_client.get(url).send().await.context(PerformRequestNewSnafu)?;
        let response = Self::check_response(response).await?;
        response.json().await.context(DecodeResponseSnafu)
    }

    async fn post_json<Req: serde::Serialize, Res: serde::de::DeserializeOwned>(
        &self,
        url: Url,
        body: &Req,
    ) -> Result<Res, Error> {
        let response =
            self.http_client.post(url).json(body).send().await.context(PerformRequestNewSnafu)?;
        let response = Self::check_response(response).await?;
        response.json().await.context(DecodeResponseSnafu)
    }

    async fn create_upload(
        &self,
        bytes: usize,
        display_name: Option<String>,
        mime_type: Mime,
    ) -> Result<Url, Error> {
        let url = self
            .base_url
            .join("/upload/v1beta/files")
            .context(ConstructUrlSnafu { suffix: "/upload/v1beta/files".to_string() })?;

        let response = self
            .http_client
            .post(url)
            .header("X-Goog-Upload-Protocol", "resumable")
            .header("X-Goog-Upload-Command", "start")
            .header("X-Goog-Upload-Content-Length", bytes.to_string())
            .header("X-Goog-Upload-Header-Content-Type", mime_type.to_string())
            .json(&json!({"file": {"displayName": display_name}}))
            .send()
            .await
            .context(PerformRequestNewSnafu)?;
        let response = Self::check_response(response).await?;

        response
            .headers()
            .get("X-Goog-Upload-URL")
            .context(MissingResponseHeaderSnafu { header: "X-Goog-Upload-URL" })
            .and_then(|v| {
                v.to_str().map(str::to_string).map_err(|_| Error::BadResponse {
                    code: 500,
                    description: Some("Missing upload URL in response".to_string()),
                })
            })
            .and_then(|url| Url::parse(&url).context(UrlParseSnafu))
    }
}

#[async_trait]
impl GeminiBackend for StudioBackend {
    // ── Core ────────────────────────────────────────────────────────────

    async fn generate_content(
        &self,
        request: GenerateContentRequest,
    ) -> Result<GenerationResponse, Error> {
        let url = self.build_url("generateContent")?;
        self.post_json(url, &request).await
    }

    async fn generate_content_stream(
        &self,
        request: GenerateContentRequest,
    ) -> Result<BackendStream<GenerationResponse>, Error> {
        let mut url = self.build_url("streamGenerateContent")?;
        url.query_pairs_mut().append_pair("alt", "sse");

        let response = self
            .http_client
            .post(url)
            .json(&request)
            .send()
            .await
            .context(PerformRequestNewSnafu)?;
        let response = Self::check_response(response).await?;

        let stream = response
            .bytes_stream()
            .eventsource()
            .map_err(|e| Error::BadPart { source: e })
            .and_then(|event| async move {
                serde_json::from_str::<GenerationResponse>(&event.data).context(DeserializeSnafu)
            });

        Ok(Box::pin(stream))
    }

    async fn embed_content(
        &self,
        request: EmbedContentRequest,
    ) -> Result<ContentEmbeddingResponse, Error> {
        let url = self.build_url("embedContent")?;
        self.post_json(url, &request).await
    }

    // ── Batch embeddings ────────────────────────────────────────────────

    async fn batch_embed_contents(
        &self,
        request: BatchEmbedContentsRequest,
    ) -> Result<BatchContentEmbeddingResponse, Error> {
        let url = self.build_url("batchEmbedContents")?;
        self.post_json(url, &request).await
    }

    // ── Batch generation ────────────────────────────────────────────────

    async fn batch_generate_content(
        &self,
        request: BatchGenerateContentRequest,
    ) -> Result<BatchGenerateContentResponse, Error> {
        let url = self.build_url("batchGenerateContent")?;
        self.post_json(url, &request).await
    }

    async fn get_batch_operation(&self, name: &str) -> Result<serde_json::Value, Error> {
        let url = self.build_batch_url(name, None)?;
        self.get_json(url).await
    }

    async fn list_batch_operations(
        &self,
        page_size: Option<u32>,
        page_token: Option<String>,
    ) -> Result<ListBatchesResponse, Error> {
        let mut url = self.build_batch_url("batches", None)?;
        if let Some(size) = page_size {
            url.query_pairs_mut().append_pair("pageSize", &size.to_string());
        }
        if let Some(token) = page_token {
            url.query_pairs_mut().append_pair("pageToken", &token);
        }
        self.get_json(url).await
    }

    async fn cancel_batch_operation(&self, name: &str) -> Result<(), Error> {
        let url = self.build_batch_url(name, Some("cancel"))?;
        let response = self
            .http_client
            .post(url)
            .json(&json!({}))
            .send()
            .await
            .context(PerformRequestNewSnafu)?;
        Self::check_response(response).await?;
        Ok(())
    }

    async fn delete_batch_operation(&self, name: &str) -> Result<(), Error> {
        let url = self.build_batch_url(name, None)?;
        let response = self.http_client.delete(url).send().await.context(PerformRequestNewSnafu)?;
        Self::check_response(response).await?;
        Ok(())
    }

    // ── Files ───────────────────────────────────────────────────────────

    async fn upload_file(
        &self,
        display_name: Option<String>,
        file_bytes: Vec<u8>,
        mime_type: Mime,
    ) -> Result<File, Error> {
        let upload_url = self.create_upload(file_bytes.len(), display_name, mime_type).await?;

        #[derive(serde::Deserialize)]
        struct UploadResponse {
            file: File,
        }

        let response = self
            .http_client
            .post(upload_url)
            .header("X-Goog-Upload-Command", "upload, finalize")
            .header("X-Goog-Upload-Offset", "0")
            .body(file_bytes)
            .send()
            .await
            .context(PerformRequestNewSnafu)?;
        let response = Self::check_response(response).await?;
        let upload: UploadResponse = response.json().await.context(DecodeResponseSnafu)?;
        Ok(upload.file)
    }

    async fn get_file(&self, name: &str) -> Result<File, Error> {
        let url = self.build_files_url(Some(name))?;
        self.get_json(url).await
    }

    async fn download_file(&self, name: &str) -> Result<Vec<u8>, Error> {
        let mut url = self
            .base_url
            .join(&format!("/download/v1beta/{name}:download"))
            .context(ConstructUrlSnafu { suffix: format!("/download/v1beta/{name}:download") })?;
        url.query_pairs_mut().append_pair("alt", "media");

        let response = self.http_client.get(url).send().await.context(PerformRequestNewSnafu)?;
        let response = Self::check_response(response).await?;
        response.bytes().await.context(DecodeResponseSnafu).map(|b| b.to_vec())
    }

    async fn list_files(
        &self,
        page_size: Option<u32>,
        page_token: Option<String>,
    ) -> Result<ListFilesResponse, Error> {
        let mut url = self.build_files_url(None)?;
        if let Some(size) = page_size {
            url.query_pairs_mut().append_pair("pageSize", &size.to_string());
        }
        if let Some(token) = page_token {
            url.query_pairs_mut().append_pair("pageToken", &token);
        }
        self.get_json(url).await
    }

    async fn delete_file(&self, name: &str) -> Result<(), Error> {
        let url = self.build_files_url(Some(name))?;
        let response = self.http_client.delete(url).send().await.context(PerformRequestNewSnafu)?;
        Self::check_response(response).await?;
        Ok(())
    }

    // ── Cache ───────────────────────────────────────────────────────────

    async fn create_cached_content(
        &self,
        request: CreateCachedContentRequest,
    ) -> Result<CachedContent, Error> {
        let url = self.build_cache_url(None)?;
        self.post_json(url, &request).await
    }

    async fn get_cached_content(&self, name: &str) -> Result<CachedContent, Error> {
        let url = self.build_cache_url(Some(name))?;
        self.get_json(url).await
    }

    async fn list_cached_contents(
        &self,
        page_size: Option<i32>,
        page_token: Option<String>,
    ) -> Result<ListCachedContentsResponse, Error> {
        let mut url = self.build_cache_url(None)?;
        if let Some(size) = page_size {
            url.query_pairs_mut().append_pair("pageSize", &size.to_string());
        }
        if let Some(token) = page_token {
            url.query_pairs_mut().append_pair("pageToken", &token);
        }
        self.get_json(url).await
    }

    async fn update_cached_content(
        &self,
        name: &str,
        expiration: CacheExpirationRequest,
    ) -> Result<CachedContent, Error> {
        let url = self.build_cache_url(Some(name))?;
        let update_payload = match expiration {
            CacheExpirationRequest::Ttl { ttl } => json!({ "ttl": ttl }),
            CacheExpirationRequest::ExpireTime { expire_time } => {
                json!({ "expireTime": expire_time.format(&time::format_description::well_known::Rfc3339).unwrap() })
            }
        };
        let response = self
            .http_client
            .patch(url)
            .json(&update_payload)
            .send()
            .await
            .context(PerformRequestNewSnafu)?;
        let response = Self::check_response(response).await?;
        response.json().await.context(DecodeResponseSnafu)
    }

    async fn delete_cached_content(&self, name: &str) -> Result<(), Error> {
        let url = self.build_cache_url(Some(name))?;
        let response = self.http_client.delete(url).send().await.context(PerformRequestNewSnafu)?;
        Self::check_response(response).await?;
        Ok(())
    }

    async fn list_models(
        &self,
        page_size: Option<u32>,
        page_token: Option<String>,
    ) -> Result<crate::model_info::ListModelsResponse, Error> {
        let mut url = self.build_url_with_suffix("models")?;
        if let Some(size) = page_size {
            url.query_pairs_mut().append_pair("pageSize", &size.to_string());
        }
        if let Some(token) = page_token {
            url.query_pairs_mut().append_pair("pageToken", &token);
        }
        self.get_json(url).await
    }

    async fn get_model(&self, name: &str) -> Result<crate::model_info::ModelInfo, Error> {
        let qualified =
            if name.starts_with("models/") { name.to_string() } else { format!("models/{name}") };
        let url = self.build_url_with_suffix(&qualified)?;
        self.get_json(url).await
    }
}
