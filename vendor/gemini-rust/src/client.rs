use crate::{
    batch::{BatchBuilder, BatchHandle},
    cache::{CacheBuilder, CachedContentHandle},
    embedding::{
        BatchContentEmbeddingResponse, BatchEmbedContentsRequest, ContentEmbeddingResponse,
        EmbedBuilder, EmbedContentRequest,
    },
    files::{
        handle::FileHandle,
        model::{File, ListFilesResponse},
    },
    generation::{ContentBuilder, GenerateContentRequest, GenerationResponse},
};
use eventsource_stream::{EventStreamError, Eventsource};
use futures::{Stream, StreamExt, TryStreamExt};
use mime::Mime;
use reqwest::{
    header::{HeaderMap, HeaderName, HeaderValue, InvalidHeaderValue},
    Client, ClientBuilder, RequestBuilder, Response,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use snafu::{OptionExt, ResultExt, Snafu};
use std::{
    fmt::{self, Formatter},
    sync::{Arc, LazyLock},
};
use tracing::{instrument, Level, Span};
use url::Url;

use crate::batch::model::*;
use crate::cache::model::*;

static DEFAULT_BASE_URL: LazyLock<Url> = LazyLock::new(|| {
    Url::parse("https://generativelanguage.googleapis.com/v1beta/")
        .expect("unreachable error: failed to parse default base URL")
});

#[derive(Debug, Default, Clone, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub enum Model {
    #[default]
    #[serde(rename = "models/gemini-2.5-flash")]
    Gemini25Flash,
    #[serde(rename = "models/gemini-2.5-flash-lite")]
    Gemini25FlashLite,
    #[serde(rename = "models/gemini-2.5-pro")]
    Gemini25Pro,
    #[serde(rename = "models/text-embedding-004")]
    TextEmbedding004,
    #[serde(untagged)]
    Custom(String),
}

impl Model {
    pub fn as_str(&self) -> &str {
        match self {
            Model::Gemini25Flash => "models/gemini-2.5-flash",
            Model::Gemini25FlashLite => "models/gemini-2.5-flash-lite",
            Model::Gemini25Pro => "models/gemini-2.5-pro",
            Model::TextEmbedding004 => "models/text-embedding-004",
            Model::Custom(model) => model,
        }
    }
}

impl From<String> for Model {
    fn from(model: String) -> Self {
        Self::Custom(model)
    }
}

impl fmt::Display for Model {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Model::Gemini25Flash => write!(f, "models/gemini-2.5-flash"),
            Model::Gemini25FlashLite => write!(f, "models/gemini-2.5-flash-lite"),
            Model::Gemini25Pro => write!(f, "models/gemini-2.5-pro"),
            Model::TextEmbedding004 => write!(f, "models/text-embedding-004"),
            Model::Custom(model) => write!(f, "{}", model),
        }
    }
}

#[derive(Debug, Snafu)]
#[snafu(visibility(pub))]
pub enum Error {
    #[snafu(display("failed to parse API key"))]
    InvalidApiKey {
        source: InvalidHeaderValue,
    },

    #[snafu(display("failed to construct URL (probably incorrect model name): {suffix}"))]
    ConstructUrl {
        source: url::ParseError,
        suffix: String,
    },

    PerformRequestNew {
        source: reqwest::Error,
    },

    #[snafu(display("failed to perform request to '{url}'"))]
    PerformRequest {
        source: reqwest::Error,
        url: Url,
    },

    #[snafu(display(
        "bad response from server; code {code}; description: {}",
        description.as_deref().unwrap_or("none")
    ))]
    BadResponse {
        /// HTTP status code
        code: u16,
        /// HTTP error description
        description: Option<String>,
    },

    MissingResponseHeader {
        header: String,
    },

    #[snafu(display("failed to obtain stream SSE part"))]
    BadPart {
        source: EventStreamError<reqwest::Error>,
    },

    #[snafu(display("failed to deserialize JSON response"))]
    Deserialize {
        source: serde_json::Error,
    },

    #[snafu(display("failed to generate content"))]
    DecodeResponse {
        source: reqwest::Error,
    },

    #[snafu(display("failed to parse URL"))]
    UrlParse {
        source: url::ParseError,
    },

    #[snafu(display("I/O error during file operations"))]
    Io {
        source: std::io::Error,
    },
}

/// Internal client for making requests to the Gemini API
pub struct GeminiClient {
    http_client: Client,
    pub model: Model,
    base_url: Url,
}

impl GeminiClient {
    /// Create a new client with custom base URL
    fn with_base_url<K: AsRef<str>, M: Into<Model>>(
        client_builder: ClientBuilder,
        api_key: K,
        model: M,
        base_url: Url,
    ) -> Result<Self, Error> {
        let headers = HeaderMap::from_iter([(
            HeaderName::from_static("x-goog-api-key"),
            HeaderValue::from_str(api_key.as_ref()).context(InvalidApiKeySnafu)?,
        )]);

        let http_client = client_builder
            .default_headers(headers)
            .build()
            .expect("all parameters must be valid");

        Ok(Self {
            http_client,
            model: model.into(),
            base_url,
        })
    }

    /// Check the response status code and return an error if it is not successful
    #[tracing::instrument(skip_all, err)]
    async fn check_response(response: Response) -> Result<Response, Error> {
        let status = response.status();
        if !status.is_success() {
            let description = response.text().await.ok();
            BadResponseSnafu {
                code: status.as_u16(),
                description,
            }
            .fail()
        } else {
            Ok(response)
        }
    }

    /// Performs an HTTP request to the Gemini API with standardized error handling.
    ///
    /// This method provides a generic way to make HTTP requests to the Gemini API with
    /// consistent error handling, response checking, and deserialization. It handles:
    /// - Building the HTTP request using a provided builder function
    /// - Sending the request and handling network errors
    /// - Checking the response status code for errors
    /// - Deserializing the response using a provided deserializer function
    ///
    /// # Type Parameters
    /// * `B` - A function that takes a `&Client` and returns a `RequestBuilder`
    /// * `D` - An async function that takes ownership of a `Response` and returns a `Result<T, Error>`
    /// * `T` - The type of the deserialized response
    ///
    /// # Note
    /// The `AsyncFn` trait is a standard Rust feature (stabilized in v1.85) and does not
    /// require any additional imports or feature flags.
    ///
    /// # Parameters
    /// * `builder` - A function that constructs the HTTP request using the client
    /// * `deserializer` - An async function that processes the response into the desired type
    ///
    /// # Examples
    ///
    /// Basic HTTP operations:
    /// ```no_run
    /// # use gemini_rust::client::*;
    /// # use reqwest::Response;
    /// # use url::Url;
    /// # use serde_json::Value;
    /// # use snafu::ResultExt;
    /// # async fn examples(client: &GeminiClient) -> Result<(), Box<dyn std::error::Error>> {
    /// # let url: Url = "https://example.com".parse()?;
    /// # let request = Value::Null;
    ///
    /// // POST request with JSON payload
    /// let _response = client
    ///     .perform_request(
    ///         |c| c.post(url.clone()).json(&request),
    ///         async |r| r.json().await.context(DecodeResponseSnafu),
    ///     )
    ///     .await?;
    ///
    /// // GET request with JSON response
    /// let _response = client
    ///     .perform_request(
    ///         |c| c.get(url.clone()),
    ///         async |r| r.json().await.context(DecodeResponseSnafu),
    ///     )
    ///     .await?;
    ///
    /// // DELETE request with no response body
    /// let _response = client
    ///     .perform_request(|c| c.delete(url), async |_r| Ok(()))
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// Request returning a stream:
    /// ```no_run
    /// # use gemini_rust::client::*;
    /// # use reqwest::Response;
    /// # use url::Url;
    /// # use serde_json::Value;
    /// # async fn example(client: &GeminiClient) -> Result<(), Box<dyn std::error::Error>> {
    /// # let url: Url = "https://example.com".parse()?;
    /// # let request = Value::Null;
    /// let _stream = client
    ///     .perform_request(
    ///         |c| c.post(url).json(&request),
    ///         async |r| Ok(r.bytes_stream()),
    ///     )
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    #[tracing::instrument(skip_all)]
    #[doc(hidden)]
    pub async fn perform_request<
        B: FnOnce(&Client) -> RequestBuilder,
        D: AsyncFn(Response) -> Result<T, Error>,
        T,
    >(
        &self,
        builder: B,
        deserializer: D,
    ) -> Result<T, Error> {
        let request = builder(&self.http_client);
        tracing::debug!("request built successfully");
        let response = request.send().await.context(PerformRequestNewSnafu)?;
        tracing::debug!("response received successfully");
        let response = Self::check_response(response).await?;
        tracing::debug!("response ok");
        deserializer(response).await
    }

    /// Perform a GET request and deserialize the JSON response.
    ///
    /// This is a convenience wrapper around [`perform_request`](Self::perform_request).
    #[tracing::instrument(skip(self), fields(request.type = "get", request.url = %url))]
    async fn get_json<T: serde::de::DeserializeOwned>(&self, url: Url) -> Result<T, Error> {
        self.perform_request(
            |c| c.get(url),
            async |r| r.json().await.context(DecodeResponseSnafu),
        )
        .await
    }

    /// Perform a POST request with JSON body and deserialize the JSON response.
    ///
    /// This is a convenience wrapper around [`perform_request`](Self::perform_request).
    #[tracing::instrument(skip(self, body), fields(request.type = "post", request.url = %url))]
    async fn post_json<Req: serde::Serialize, Res: serde::de::DeserializeOwned>(
        &self,
        url: Url,
        body: &Req,
    ) -> Result<Res, Error> {
        self.perform_request(
            |c| c.post(url).json(body),
            async |r| r.json().await.context(DecodeResponseSnafu),
        )
        .await
    }

    /// Generate content
    #[instrument(skip_all, fields(
        model,
        messages.parts.count = request.contents.len(),
        tools.present = request.tools.is_some(),
        system.instruction.present = request.system_instruction.is_some(),
        cached.content.present = request.cached_content.is_some(),
        usage.prompt_tokens,
        usage.candidates_tokens,
        usage.thoughts_tokens,
        usage.cached_content_tokens,
        usage.total_tokens,
    ), ret(level = Level::TRACE), err)]
    pub(crate) async fn generate_content_raw(
        &self,
        request: GenerateContentRequest,
    ) -> Result<GenerationResponse, Error> {
        let url = self.build_url("generateContent")?;
        let response: GenerationResponse = self.post_json(url, &request).await?;

        // Record usage metadata
        if let Some(usage) = &response.usage_metadata {
            #[rustfmt::skip]
            Span::current()
                .record("usage.prompt_tokens", usage.prompt_token_count)
                .record("usage.candidates_tokens", usage.candidates_token_count)
                .record("usage.thoughts_tokens", usage.thoughts_token_count)
                .record("usage.cached_content_tokens", usage.cached_content_token_count)
                .record("usage.total_tokens", usage.total_token_count);

            tracing::debug!("generation usage evaluated");
        }

        Ok(response)
    }

    /// Generate content with streaming
    #[instrument(skip_all, fields(
        model,
        messages.parts.count = request.contents.len(),
        tools.present = request.tools.is_some(),
        system.instruction.present = request.system_instruction.is_some(),
        cached.content.present = request.cached_content.is_some(),
    ), err)]
    pub(crate) async fn generate_content_stream(
        &self,
        request: GenerateContentRequest,
    ) -> Result<impl TryStreamExt<Ok = GenerationResponse, Error = Error> + Send + use<>, Error>
    {
        let mut url = self.build_url("streamGenerateContent")?;
        url.query_pairs_mut().append_pair("alt", "sse");

        let stream = self
            .perform_request(
                |c| c.post(url).json(&request),
                async |r| Ok(r.bytes_stream()),
            )
            .await?;

        Ok(stream
            .eventsource()
            .map(|event| event.context(BadPartSnafu))
            .map_ok(|event| {
                serde_json::from_str::<GenerationResponse>(&event.data).context(DeserializeSnafu)
            })
            .map(|r| r.flatten()))
    }

    /// Embed content
    #[instrument(skip_all, fields(
        model,
        task.type = request.task_type.as_ref().map(|t| format!("{:?}", t)),
        task.title = request.title,
        task.output.dimensionality = request.output_dimensionality,
    ))]
    pub(crate) async fn embed_content(
        &self,
        request: EmbedContentRequest,
    ) -> Result<ContentEmbeddingResponse, Error> {
        let url = self.build_url("embedContent")?;
        self.post_json(url, &request).await
    }

    /// Batch Embed content
    #[instrument(skip_all, fields(batch.size = request.requests.len()))]
    pub(crate) async fn embed_content_batch(
        &self,
        request: BatchEmbedContentsRequest,
    ) -> Result<BatchContentEmbeddingResponse, Error> {
        let url = self.build_url("batchEmbedContents")?;
        self.post_json(url, &request).await
    }

    /// Batch generate content (synchronous API that returns results immediately)
    #[instrument(skip_all, fields(
        batch.display_name = request.batch.display_name,
        batch.size = request.batch.input_config.batch_size(),
    ))]
    pub(crate) async fn batch_generate_content(
        &self,
        request: BatchGenerateContentRequest,
    ) -> Result<BatchGenerateContentResponse, Error> {
        let url = self.build_url("batchGenerateContent")?;
        self.post_json(url, &request).await
    }

    /// Get a batch operation
    #[instrument(skip_all, fields(
        operation.name = name,
    ))]
    pub(crate) async fn get_batch_operation<T: serde::de::DeserializeOwned>(
        &self,
        name: &str,
    ) -> Result<T, Error> {
        let url = self.build_batch_url(name, None)?;
        self.get_json(url).await
    }

    /// List batch operations
    #[instrument(skip_all, fields(
        page.size = page_size,
        page.token.present = page_token.is_some(),
    ))]
    pub(crate) async fn list_batch_operations(
        &self,
        page_size: Option<u32>,
        page_token: Option<String>,
    ) -> Result<ListBatchesResponse, Error> {
        let mut url = self.build_batch_url("batches", None)?;

        if let Some(size) = page_size {
            url.query_pairs_mut()
                .append_pair("pageSize", &size.to_string());
        }
        if let Some(token) = page_token {
            url.query_pairs_mut().append_pair("pageToken", &token);
        }

        self.get_json(url).await
    }

    /// List files
    #[instrument(skip_all, fields(
        page.size = page_size,
        page.token.present = page_token.is_some(),
    ))]
    pub(crate) async fn list_files(
        &self,
        page_size: Option<u32>,
        page_token: Option<String>,
    ) -> Result<ListFilesResponse, Error> {
        let mut url = self.build_files_url(None)?;

        if let Some(size) = page_size {
            url.query_pairs_mut()
                .append_pair("pageSize", &size.to_string());
        }
        if let Some(token) = page_token {
            url.query_pairs_mut().append_pair("pageToken", &token);
        }

        self.get_json(url).await
    }

    /// Cancel a batch operation
    #[instrument(skip_all, fields(
        operation.name = name,
    ))]
    pub(crate) async fn cancel_batch_operation(&self, name: &str) -> Result<(), Error> {
        let url = self.build_batch_url(name, Some("cancel"))?;
        self.perform_request(|c| c.post(url).json(&json!({})), async |_r| Ok(()))
            .await
    }

    /// Delete a batch operation
    #[instrument(skip_all, fields(
        operation.name = name,
    ))]
    pub(crate) async fn delete_batch_operation(&self, name: &str) -> Result<(), Error> {
        let url = self.build_batch_url(name, None)?;
        self.perform_request(|c| c.delete(url), async |_r| Ok(()))
            .await
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
            .context(ConstructUrlSnafu {
                suffix: "/upload/v1beta/files".to_string(),
            })?;

        self.perform_request(
            |c| {
                c.post(url)
                    .header("X-Goog-Upload-Protocol", "resumable")
                    .header("X-Goog-Upload-Command", "start")
                    .header("X-Goog-Upload-Content-Length", bytes.to_string())
                    .header("X-Goog-Upload-Header-Content-Type", mime_type.to_string())
                    .json(&json!({"file": {"displayName": display_name}}))
            },
            async |r| {
                r.headers()
                    .get("X-Goog-Upload-URL")
                    .context(MissingResponseHeaderSnafu {
                        header: "X-Goog-Upload-URL",
                    })
                    .and_then(|upload_url| {
                        upload_url
                            .to_str()
                            .map(str::to_string)
                            .map_err(|_| Error::BadResponse {
                                code: 500,
                                description: Some("Missing upload URL in response".to_string()),
                            })
                    })
                    .and_then(|url| Url::parse(&url).context(UrlParseSnafu))
            },
        )
        .await
    }

    /// Upload a file using the resumable upload protocol.
    #[instrument(skip_all, fields(
        file.size = file_bytes.len(),
        mime.type = mime_type.to_string(),
        file.display_name = display_name.as_deref(),
    ))]
    pub(crate) async fn upload_file(
        &self,
        display_name: Option<String>,
        file_bytes: Vec<u8>,
        mime_type: Mime,
    ) -> Result<File, Error> {
        // Step 1: Create resumable upload session
        let upload_url = self
            .create_upload(file_bytes.len(), display_name, mime_type)
            .await?;

        // Step 2: Upload file content
        let upload_response = self
            .http_client
            .post(upload_url.clone())
            .header("X-Goog-Upload-Command", "upload, finalize")
            .header("X-Goog-Upload-Offset", "0")
            .body(file_bytes)
            .send()
            .await
            .map_err(|e| Error::PerformRequest {
                source: e,
                url: upload_url,
            })?;

        let final_response = Self::check_response(upload_response).await?;

        #[derive(serde::Deserialize)]
        struct UploadResponse {
            file: File,
        }

        let upload_response: UploadResponse =
            final_response.json().await.context(DecodeResponseSnafu)?;
        Ok(upload_response.file)
    }

    /// Get a file resource
    #[instrument(skip_all, fields(
        file.name = name,
    ))]
    pub(crate) async fn get_file(&self, name: &str) -> Result<File, Error> {
        let url = self.build_files_url(Some(name))?;
        self.get_json(url).await
    }

    /// Delete a file resource
    #[instrument(skip_all, fields(
        file.name = name,
    ))]
    pub(crate) async fn delete_file(&self, name: &str) -> Result<(), Error> {
        let url = self.build_files_url(Some(name))?;
        self.perform_request(|c| c.delete(url), async |_r| Ok(()))
            .await
    }

    /// Download a file resource
    #[instrument(skip_all, fields(
        file.name = name,
    ))]
    pub(crate) async fn download_file(&self, name: &str) -> Result<Vec<u8>, Error> {
        let mut url = self
            .base_url
            .join(&format!("/download/v1beta/{name}:download"))
            .context(ConstructUrlSnafu {
                suffix: format!("/download/v1beta/{name}:download"),
            })?;
        url.query_pairs_mut().append_pair("alt", "media");

        self.perform_request(
            |c| c.get(url),
            async |r| {
                r.bytes()
                    .await
                    .context(DecodeResponseSnafu)
                    .map(|bytes| bytes.to_vec())
            },
        )
        .await
    }

    /// Create cached content
    pub(crate) async fn create_cached_content(
        &self,
        cached_content: CreateCachedContentRequest,
    ) -> Result<CachedContent, Error> {
        let url = self.build_cache_url(None)?;
        self.post_json(url, &cached_content).await
    }

    /// Get cached content
    pub(crate) async fn get_cached_content(&self, name: &str) -> Result<CachedContent, Error> {
        let url = self.build_cache_url(Some(name))?;
        self.get_json(url).await
    }

    /// Update cached content (typically to update TTL)
    pub(crate) async fn update_cached_content(
        &self,
        name: &str,
        expiration: CacheExpirationRequest,
    ) -> Result<CachedContent, Error> {
        let url = self.build_cache_url(Some(name))?;

        // Create a minimal update payload with just the expiration
        let update_payload = match expiration {
            CacheExpirationRequest::Ttl { ttl } => json!({ "ttl": ttl }),
            CacheExpirationRequest::ExpireTime { expire_time } => {
                json!({ "expireTime": expire_time.format(&time::format_description::well_known::Rfc3339).unwrap() })
            }
        };

        self.perform_request(
            |c| c.patch(url.clone()).json(&update_payload),
            async |r| r.json().await.context(DecodeResponseSnafu),
        )
        .await
    }

    /// Delete cached content
    pub(crate) async fn delete_cached_content(&self, name: &str) -> Result<(), Error> {
        let url = self.build_cache_url(Some(name))?;
        self.perform_request(|c| c.delete(url.clone()), async |_r| Ok(()))
            .await
    }

    /// List cached contents
    pub(crate) async fn list_cached_contents(
        &self,
        page_size: Option<i32>,
        page_token: Option<String>,
    ) -> Result<ListCachedContentsResponse, Error> {
        let mut url = self.build_cache_url(None)?;

        if let Some(size) = page_size {
            url.query_pairs_mut()
                .append_pair("pageSize", &size.to_string());
        }
        if let Some(token) = page_token {
            url.query_pairs_mut().append_pair("pageToken", &token);
        }

        self.get_json(url).await
    }

    /// Build a URL with the given suffix
    #[tracing::instrument(skip(self), ret(level = Level::DEBUG))]
    fn build_url_with_suffix(&self, suffix: &str) -> Result<Url, Error> {
        self.base_url.join(suffix).context(ConstructUrlSnafu {
            suffix: suffix.to_string(),
        })
    }

    /// Build a URL for the API
    #[tracing::instrument(skip(self), ret(level = Level::DEBUG))]
    fn build_url(&self, endpoint: &str) -> Result<Url, Error> {
        let suffix = format!("{}:{endpoint}", self.model);
        self.build_url_with_suffix(&suffix)
    }

    /// Build a URL for a batch operation
    fn build_batch_url(&self, name: &str, action: Option<&str>) -> Result<Url, Error> {
        let suffix = action
            .map(|a| format!("{name}:{a}"))
            .unwrap_or_else(|| name.to_string());
        self.build_url_with_suffix(&suffix)
    }

    /// Build a URL for file operations
    fn build_files_url(&self, name: Option<&str>) -> Result<Url, Error> {
        let suffix = name
            .map(|n| format!("files/{}", n.strip_prefix("files/").unwrap_or(n)))
            .unwrap_or_else(|| "files".to_string());
        self.build_url_with_suffix(&suffix)
    }

    /// Build a URL for cache operations
    fn build_cache_url(&self, name: Option<&str>) -> Result<Url, Error> {
        let suffix = name
            .map(|n| {
                if n.starts_with("cachedContents/") {
                    n.to_string()
                } else {
                    format!("cachedContents/{}", n)
                }
            })
            .unwrap_or_else(|| "cachedContents".to_string());
        self.build_url_with_suffix(&suffix)
    }
}

/// A builder for the `Gemini` client.
///
/// # Examples
///
/// ## Basic usage
///
/// ```no_run
/// use gemini_rust::{GeminiBuilder, Model};
///
/// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
/// let gemini = GeminiBuilder::new("YOUR_API_KEY")
///     .with_model(Model::Gemini25Pro)
///     .build()?;
/// # Ok(())
/// # }
/// ```
///
/// ## With proxy configuration
///
/// ```no_run
/// use gemini_rust::{GeminiBuilder, Model};
/// use reqwest::{ClientBuilder, Proxy};
///
/// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
/// let proxy = Proxy::https("https://my.proxy")?;
/// let http_client = ClientBuilder::new().proxy(proxy);
///
/// let gemini = GeminiBuilder::new("YOUR_API_KEY")
///     .with_http_client(http_client)
///     .build()?;
/// # Ok(())
/// # }
/// ```
pub struct GeminiBuilder {
    key: String,
    model: Model,
    client_builder: ClientBuilder,
    base_url: Url,
}

impl GeminiBuilder {
    /// Creates a new `GeminiBuilder` with the given API key.
    pub fn new<K: Into<String>>(key: K) -> Self {
        Self {
            key: key.into(),
            model: Model::default(),
            client_builder: ClientBuilder::default(),
            base_url: DEFAULT_BASE_URL.clone(),
        }
    }

    /// Sets the model for the client.
    pub fn with_model<M: Into<Model>>(mut self, model: M) -> Self {
        self.model = model.into();
        self
    }

    /// Sets a custom `reqwest::ClientBuilder`.
    pub fn with_http_client(mut self, client_builder: ClientBuilder) -> Self {
        self.client_builder = client_builder;
        self
    }

    /// Sets a custom base URL for the API.
    pub fn with_base_url(mut self, base_url: Url) -> Self {
        self.base_url = base_url;
        self
    }

    /// Builds the `Gemini` client.
    pub fn build(self) -> Result<Gemini, Error> {
        Ok(Gemini {
            client: Arc::new(GeminiClient::with_base_url(
                self.client_builder,
                self.key,
                self.model,
                self.base_url,
            )?),
        })
    }
}

/// Client for the Gemini API
#[derive(Clone)]
pub struct Gemini {
    client: Arc<GeminiClient>,
}

impl Gemini {
    /// Create a new client with the specified API key
    pub fn new<K: AsRef<str>>(api_key: K) -> Result<Self, Error> {
        Self::with_model(api_key, Model::default())
    }

    /// Create a new client for the Gemini Pro model
    pub fn pro<K: AsRef<str>>(api_key: K) -> Result<Self, Error> {
        Self::with_model(api_key, Model::Gemini25Pro)
    }

    /// Create a new client with the specified API key and model
    pub fn with_model<K: AsRef<str>, M: Into<Model>>(api_key: K, model: M) -> Result<Self, Error> {
        Self::with_model_and_base_url(api_key, model, DEFAULT_BASE_URL.clone())
    }

    /// Create a new client with custom base URL
    pub fn with_base_url<K: AsRef<str>>(api_key: K, base_url: Url) -> Result<Self, Error> {
        Self::with_model_and_base_url(api_key, Model::default(), base_url)
    }

    /// Create a new client with the specified API key, model, and base URL
    pub fn with_model_and_base_url<K: AsRef<str>, M: Into<Model>>(
        api_key: K,
        model: M,
        base_url: Url,
    ) -> Result<Self, Error> {
        let client =
            GeminiClient::with_base_url(Default::default(), api_key, model.into(), base_url)?;
        Ok(Self {
            client: Arc::new(client),
        })
    }

    /// Start building a content generation request
    pub fn generate_content(&self) -> ContentBuilder {
        ContentBuilder::new(self.client.clone())
    }

    /// Start building a content embedding request
    pub fn embed_content(&self) -> EmbedBuilder {
        EmbedBuilder::new(self.client.clone())
    }

    /// Start building a batch content generation request
    pub fn batch_generate_content(&self) -> BatchBuilder {
        BatchBuilder::new(self.client.clone())
    }

    /// Get a handle to a batch operation by its name.
    pub fn get_batch(&self, name: &str) -> BatchHandle {
        BatchHandle::new(name.to_string(), self.client.clone())
    }

    /// Lists batch operations.
    ///
    /// This method returns a stream that handles pagination automatically.
    pub fn list_batches(
        &self,
        page_size: impl Into<Option<u32>>,
    ) -> impl Stream<Item = Result<BatchOperation, Error>> + Send {
        let client = self.client.clone();
        let page_size = page_size.into();
        async_stream::try_stream! {
            let mut page_token: Option<String> = None;
            loop {
                let response = client
                    .list_batch_operations(page_size, page_token.clone())
                    .await?;

                for operation in response.operations {
                    yield operation;
                }

                if let Some(next_page_token) = response.next_page_token {
                    page_token = Some(next_page_token);
                } else {
                    break;
                }
            }
        }
    }

    /// Create cached content with a fluent API.
    pub fn create_cache(&self) -> CacheBuilder {
        CacheBuilder::new(self.client.clone())
    }

    /// Get a handle to cached content by its name.
    pub fn get_cached_content(&self, name: &str) -> CachedContentHandle {
        CachedContentHandle::new(name.to_string(), self.client.clone())
    }

    /// Lists cached contents.
    ///
    /// This method returns a stream that handles pagination automatically.
    pub fn list_cached_contents(
        &self,
        page_size: impl Into<Option<i32>>,
    ) -> impl Stream<Item = Result<CachedContentSummary, Error>> + Send {
        let client = self.client.clone();
        let page_size = page_size.into();
        async_stream::try_stream! {
            let mut page_token: Option<String> = None;
            loop {
                let response = client
                    .list_cached_contents(page_size, page_token.clone())
                    .await?;

                for cached_content in response.cached_contents {
                    yield cached_content;
                }

                if let Some(next_page_token) = response.next_page_token {
                    page_token = Some(next_page_token);
                } else {
                    break;
                }
            }
        }
    }

    /// Start building a file resource
    pub fn create_file<B: Into<Vec<u8>>>(&self, bytes: B) -> crate::files::builder::FileBuilder {
        crate::files::builder::FileBuilder::new(self.client.clone(), bytes)
    }

    /// Get a handle to a file by its name.
    pub async fn get_file(&self, name: &str) -> Result<FileHandle, Error> {
        let file = self.client.get_file(name).await?;
        Ok(FileHandle::new(self.client.clone(), file))
    }

    /// Lists files.
    ///
    /// This method returns a stream that handles pagination automatically.
    pub fn list_files(
        &self,
        page_size: impl Into<Option<u32>>,
    ) -> impl Stream<Item = Result<FileHandle, Error>> + Send {
        let client = self.client.clone();
        let page_size = page_size.into();
        async_stream::try_stream! {
            let mut page_token: Option<String> = None;
            loop {
                let response = client
                    .list_files(page_size, page_token.clone())
                    .await?;

                for file in response.files {
                    yield FileHandle::new(client.clone(), file);
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
