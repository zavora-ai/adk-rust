use crate::{
    backend,
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
use eventsource_stream::EventStreamError;
use futures::Stream;
#[cfg(feature = "vertex")]
use google_cloud_aiplatform_v1::client::PredictionService;
#[cfg(feature = "vertex")]
use google_cloud_auth::credentials::{self, Credentials};
use mime::Mime;
use reqwest::{ClientBuilder, header::InvalidHeaderValue};
use serde::{Deserialize, Serialize};
use snafu::{ResultExt, Snafu};
use std::{
    fmt::{self, Formatter},
    sync::{Arc, LazyLock},
};
use tracing::{Level, Span, instrument};
use url::Url;

use crate::batch::model::*;
use crate::cache::model::*;

static DEFAULT_BASE_URL: LazyLock<Url> = LazyLock::new(|| {
    Url::parse("https://generativelanguage.googleapis.com/v1beta/")
        .expect("unreachable error: failed to parse default base URL")
});
static V1_BASE_URL: LazyLock<Url> = LazyLock::new(|| {
    Url::parse("https://generativelanguage.googleapis.com/v1/")
        .expect("unreachable error: failed to parse v1 base URL")
});

// ══════════════════════════════════════════════════════════════════════
// Model enum
// ══════════════════════════════════════════════════════════════════════

/// Available Gemini model identifiers.
///
/// Each variant maps to a specific model version on the Gemini API.
/// Use [`Model::Custom`] for model IDs not yet represented as variants.
#[derive(Debug, Default, Clone, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub enum Model {
    // ── Gemini 3.5 (latest generation) ──────────────────────────
    /// Gemini 3.5 Flash — latest stable model, default for new projects.
    #[serde(rename = "models/gemini-3.5-flash")]
    #[default]
    Gemini35Flash,

    // ── Gemini 3.1 ────────────────────────────────────────────────
    /// Gemini 3.1 Pro preview — strongest reasoning.
    #[serde(rename = "models/gemini-3.1-pro-preview")]
    Gemini31ProPreview,
    /// Gemini 3.1 Flash Lite preview — cheapest and fastest.
    #[serde(rename = "models/gemini-3.1-flash-lite-preview")]
    Gemini31FlashLitePreview,

    // ── Gemini 3 ─────────────────────────────────────────────────
    /// Gemini 3 Pro preview.
    #[serde(rename = "models/gemini-3-pro-preview")]
    Gemini3ProPreview,
    /// Gemini 3 Pro Image preview — multimodal output.
    #[serde(rename = "models/gemini-3-pro-image-preview")]
    Gemini3ProImagePreview,
    /// Gemini 3 Flash preview — good balance of speed and capability.
    #[serde(rename = "models/gemini-3-flash-preview")]
    Gemini3FlashPreview,

    // ── Gemini 2.5 ───────────────────────────────────────────────
    /// Gemini 2.5 Pro — advanced reasoning.
    #[serde(rename = "models/gemini-2.5-pro")]
    Gemini25Pro,
    /// Gemini 2.5 Pro preview with TTS support.
    #[serde(rename = "models/gemini-2.5-pro-preview-tts")]
    Gemini25ProPreviewTts,
    /// Gemini 2.5 Flash — fast and capable.
    #[serde(rename = "models/gemini-2.5-flash")]
    Gemini25Flash,
    /// Gemini 2.5 Flash preview (September 2025).
    #[serde(rename = "models/gemini-2.5-flash-preview-09-2025")]
    Gemini25FlashPreview092025,
    /// Gemini 2.5 Flash with image generation.
    #[serde(rename = "models/gemini-2.5-flash-image")]
    Gemini25FlashImage,
    /// Deprecated: use `Gemini25FlashImage` instead.
    #[deprecated(note = "Use Model::Gemini25FlashImage instead")]
    #[serde(rename = "models/gemini-2.5-flash-image-preview")]
    Gemini25FlashImagePreview,
    /// Gemini 2.5 Flash native audio preview (December 2025).
    #[serde(rename = "models/gemini-2.5-flash-native-audio-preview-12-2025")]
    Gemini25FlashLive122025,
    /// Gemini 2.5 Flash native audio preview (September 2025).
    #[serde(rename = "models/gemini-2.5-flash-native-audio-preview-09-2025")]
    Gemini25FlashLive092025,
    /// Gemini 2.5 Flash preview with TTS support.
    #[serde(rename = "models/gemini-2.5-flash-preview-tts")]
    Gemini25FlashPreviewTts,
    /// Gemini 2.5 Flash Lite — cost-efficient.
    #[serde(rename = "models/gemini-2.5-flash-lite")]
    Gemini25FlashLite,
    /// Gemini 2.5 Flash Lite preview (September 2025).
    #[serde(rename = "models/gemini-2.5-flash-lite-preview-09-2025")]
    Gemini25FlashLitePreview092025,

    // ── Gemini 2.0 (deprecated, shutting down March 31, 2026) ────
    /// Gemini 2.0 Flash (deprecated).
    #[deprecated(note = "Gemini 2.0 models shut down March 31, 2026")]
    #[serde(rename = "models/gemini-2.0-flash")]
    Gemini20Flash,
    /// Gemini 2.0 Flash 001 (deprecated).
    #[deprecated(note = "Gemini 2.0 models shut down March 31, 2026")]
    #[serde(rename = "models/gemini-2.0-flash-001")]
    Gemini20Flash001,
    /// Gemini 2.0 Flash experimental (deprecated).
    #[deprecated(note = "Gemini 2.0 models shut down March 31, 2026")]
    #[serde(rename = "models/gemini-2.0-flash-exp")]
    Gemini20FlashExp,
    /// Gemini 2.0 Flash Lite (deprecated).
    #[deprecated(note = "Gemini 2.0 models shut down March 31, 2026")]
    #[serde(rename = "models/gemini-2.0-flash-lite")]
    Gemini20FlashLite,
    /// Gemini 2.0 Flash Lite 001 (deprecated).
    #[deprecated(note = "Gemini 2.0 models shut down March 31, 2026")]
    #[serde(rename = "models/gemini-2.0-flash-lite-001")]
    Gemini20FlashLite001,

    // ── Embedding models ─────────────────────────────────────────
    /// Gemini Embedding 001 (3072 dimensions). Replaces text-embedding-004.
    #[serde(rename = "models/gemini-embedding-001")]
    GeminiEmbedding001,
    /// Deprecated: use `GeminiEmbedding001` instead.
    #[deprecated(note = "Use Model::GeminiEmbedding001 (gemini-embedding-001) instead")]
    #[serde(rename = "models/text-embedding-004")]
    TextEmbedding004,

    // ── Custom ───────────────────────────────────────────────────
    /// A custom model identifier string (e.g. `"models/my-tuned-model"`).
    #[serde(untagged)]
    Custom(String),
}

impl Model {
    /// Returns the model identifier as a string slice.
    pub fn as_str(&self) -> &str {
        #[allow(deprecated)]
        match self {
            Model::Gemini35Flash => "models/gemini-3.5-flash",
            Model::Gemini31ProPreview => "models/gemini-3.1-pro-preview",
            Model::Gemini31FlashLitePreview => "models/gemini-3.1-flash-lite-preview",
            Model::Gemini3ProPreview => "models/gemini-3-pro-preview",
            Model::Gemini3ProImagePreview => "models/gemini-3-pro-image-preview",
            Model::Gemini3FlashPreview => "models/gemini-3-flash-preview",
            Model::Gemini25Pro => "models/gemini-2.5-pro",
            Model::Gemini25ProPreviewTts => "models/gemini-2.5-pro-preview-tts",
            Model::Gemini25Flash => "models/gemini-2.5-flash",
            Model::Gemini25FlashPreview092025 => "models/gemini-2.5-flash-preview-09-2025",
            Model::Gemini25FlashImage => "models/gemini-2.5-flash-image",
            Model::Gemini25FlashImagePreview => "models/gemini-2.5-flash-image-preview",
            Model::Gemini25FlashLive122025 => {
                "models/gemini-2.5-flash-native-audio-preview-12-2025"
            }
            Model::Gemini25FlashLive092025 => {
                "models/gemini-2.5-flash-native-audio-preview-09-2025"
            }
            Model::Gemini25FlashPreviewTts => "models/gemini-2.5-flash-preview-tts",
            Model::Gemini25FlashLite => "models/gemini-2.5-flash-lite",
            Model::Gemini25FlashLitePreview092025 => "models/gemini-2.5-flash-lite-preview-09-2025",
            Model::Gemini20Flash => "models/gemini-2.0-flash",
            Model::Gemini20Flash001 => "models/gemini-2.0-flash-001",
            Model::Gemini20FlashExp => "models/gemini-2.0-flash-exp",
            Model::Gemini20FlashLite => "models/gemini-2.0-flash-lite",
            Model::Gemini20FlashLite001 => "models/gemini-2.0-flash-lite-001",
            Model::GeminiEmbedding001 => "models/gemini-embedding-001",
            Model::TextEmbedding004 => "models/text-embedding-004",
            Model::Custom(model) => model,
        }
    }

    /// Returns the Vertex AI model resource path for this model.
    pub fn vertex_model_path(&self, project_id: &str, location: &str) -> String {
        #[allow(deprecated)]
        let model_id = match self {
            Model::Gemini35Flash => "gemini-3.5-flash",
            Model::Gemini31ProPreview => "gemini-3.1-pro-preview",
            Model::Gemini31FlashLitePreview => "gemini-3.1-flash-lite-preview",
            Model::Gemini3ProPreview => "gemini-3-pro-preview",
            Model::Gemini3ProImagePreview => "gemini-3-pro-image-preview",
            Model::Gemini3FlashPreview => "gemini-3-flash-preview",
            Model::Gemini25Pro => "gemini-2.5-pro",
            Model::Gemini25ProPreviewTts => "gemini-2.5-pro-preview-tts",
            Model::Gemini25Flash => "gemini-2.5-flash",
            Model::Gemini25FlashPreview092025 => "gemini-2.5-flash-preview-09-2025",
            Model::Gemini25FlashImage => "gemini-2.5-flash-image",
            Model::Gemini25FlashImagePreview => "gemini-2.5-flash-image-preview",
            Model::Gemini25FlashLive122025 => "gemini-2.5-flash-native-audio-preview-12-2025",
            Model::Gemini25FlashLive092025 => "gemini-2.5-flash-native-audio-preview-09-2025",
            Model::Gemini25FlashPreviewTts => "gemini-2.5-flash-preview-tts",
            Model::Gemini25FlashLite => "gemini-2.5-flash-lite",
            Model::Gemini25FlashLitePreview092025 => "gemini-2.5-flash-lite-preview-09-2025",
            Model::Gemini20Flash => "gemini-2.0-flash",
            Model::Gemini20Flash001 => "gemini-2.0-flash-001",
            Model::Gemini20FlashExp => "gemini-2.0-flash-exp",
            Model::Gemini20FlashLite => "gemini-2.0-flash-lite",
            Model::Gemini20FlashLite001 => "gemini-2.0-flash-lite-001",
            Model::GeminiEmbedding001 => "gemini-embedding-001",
            Model::TextEmbedding004 => "text-embedding-004",
            Model::Custom(model) => {
                if model.starts_with("projects/") {
                    return model.clone();
                }
                if model.starts_with("publishers/") {
                    return format!("projects/{project_id}/locations/{location}/{model}");
                }
                model.strip_prefix("models/").unwrap_or(model)
            }
        };
        format!("projects/{project_id}/locations/{location}/publishers/google/models/{model_id}")
    }
}

impl From<String> for Model {
    #[allow(deprecated)]
    fn from(model: String) -> Self {
        // Match known model names (with or without "models/" prefix) to proper variants.
        let bare = model.strip_prefix("models/").unwrap_or(&model);
        match bare {
            // Gemini 3.5 models (latest generation)
            "gemini-3.5-flash" => Self::Gemini35Flash,
            // Gemini 3.1 models
            "gemini-3.1-pro-preview" => Self::Gemini31ProPreview,
            // Gemini 3 models
            "gemini-3-pro-preview" => Self::Gemini3ProPreview,
            "gemini-3-pro-image-preview" => Self::Gemini3ProImagePreview,
            "gemini-3-flash-preview" => Self::Gemini3FlashPreview,
            // Gemini 2.5 models
            "gemini-2.5-pro" => Self::Gemini25Pro,
            "gemini-2.5-pro-preview-tts" => Self::Gemini25ProPreviewTts,
            "gemini-2.5-flash" => Self::Gemini25Flash,
            "gemini-2.5-flash-preview-09-2025" => Self::Gemini25FlashPreview092025,
            "gemini-2.5-flash-image" => Self::Gemini25FlashImage,
            "gemini-2.5-flash-image-preview" => Self::Gemini25FlashImagePreview,
            "gemini-2.5-flash-native-audio-preview-12-2025" => Self::Gemini25FlashLive122025,
            "gemini-2.5-flash-native-audio-preview-09-2025" => Self::Gemini25FlashLive092025,
            "gemini-2.5-flash-preview-tts" => Self::Gemini25FlashPreviewTts,
            "gemini-2.5-flash-lite" => Self::Gemini25FlashLite,
            "gemini-2.5-flash-lite-preview-09-2025" => Self::Gemini25FlashLitePreview092025,
            // Gemini 2.0 models (deprecated, shutting down March 31, 2026)
            "gemini-2.0-flash" => Self::Gemini20Flash,
            "gemini-2.0-flash-001" => Self::Gemini20Flash001,
            "gemini-2.0-flash-exp" => Self::Gemini20FlashExp,
            "gemini-2.0-flash-lite" => Self::Gemini20FlashLite,
            "gemini-2.0-flash-lite-001" => Self::Gemini20FlashLite001,
            // Embedding models
            "gemini-embedding-001" => Self::GeminiEmbedding001,
            "text-embedding-004" => Self::TextEmbedding004,
            _ => Self::Custom(model),
        }
    }
}

impl fmt::Display for Model {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        #[allow(deprecated)]
        match self {
            Model::Custom(model) => {
                // Ensure custom models always have the "models/" prefix for API URLs
                if model.starts_with("models/")
                    || model.starts_with("projects/")
                    || model.starts_with("publishers/")
                {
                    write!(f, "{model}")
                } else {
                    write!(f, "models/{model}")
                }
            }
            other => write!(f, "{}", other.as_str()),
        }
    }
}

// ══════════════════════════════════════════════════════════════════════
// Error enum
// ══════════════════════════════════════════════════════════════════════

/// Errors that can occur when interacting with the Gemini API.
#[derive(Debug, Snafu)]
#[snafu(visibility(pub))]
pub enum Error {
    /// The provided API key is not a valid HTTP header value.
    #[snafu(display("failed to parse API key"))]
    InvalidApiKey {
        /// The underlying header value error.
        source: InvalidHeaderValue,
    },

    /// Failed to construct a request URL (likely an incorrect model name).
    #[snafu(display("failed to construct URL (probably incorrect model name): {suffix}"))]
    ConstructUrl {
        /// The underlying URL parse error.
        source: url::ParseError,
        /// The URL suffix that failed to parse.
        suffix: String,
    },

    /// An HTTP request failed before reaching the server.
    #[snafu(display("failed to perform request: {source}"))]
    PerformRequestNew {
        /// The underlying reqwest error.
        source: reqwest::Error,
    },

    /// An HTTP request to a specific URL failed.
    #[snafu(display("failed to perform request to '{url}'"))]
    PerformRequest {
        /// The underlying reqwest error.
        source: reqwest::Error,
        /// The URL that was being requested.
        url: Url,
    },

    /// The server returned a non-success HTTP status code.
    #[snafu(display("bad response from server; code {code}; description: {}", description.as_deref().unwrap_or("none")))]
    BadResponse {
        /// The HTTP status code.
        code: u16,
        /// An optional error description from the server.
        description: Option<String>,
    },

    /// A required response header was missing.
    MissingResponseHeader {
        /// The name of the missing header.
        header: String,
    },

    /// Failed to read a Server-Sent Events (SSE) part from the stream.
    #[snafu(display("failed to obtain stream SSE part"))]
    BadPart {
        /// The underlying event stream error.
        source: EventStreamError<reqwest::Error>,
    },

    /// Failed to deserialize a JSON response body.
    #[snafu(display("failed to deserialize JSON response"))]
    Deserialize {
        /// The underlying serde_json error.
        source: serde_json::Error,
    },

    /// Failed to decode the response body.
    #[snafu(display("failed to generate content"))]
    DecodeResponse {
        /// The underlying reqwest error.
        source: reqwest::Error,
    },

    /// Failed to parse a URL string.
    #[snafu(display("failed to parse URL"))]
    UrlParse {
        /// The underlying URL parse error.
        source: url::ParseError,
    },

    /// Failed to build Google Cloud credentials.
    #[snafu(display("failed to build google cloud credentials"))]
    #[cfg(feature = "vertex")]
    GoogleCloudAuth {
        /// The underlying credentials build error.
        source: google_cloud_auth::build_errors::Error,
    },

    /// Failed to obtain Google Cloud auth headers.
    #[snafu(display("failed to obtain google cloud auth headers"))]
    #[cfg(feature = "vertex")]
    GoogleCloudCredentialHeaders {
        /// The underlying credentials error.
        source: google_cloud_auth::errors::CredentialsError,
    },

    /// Google Cloud credentials returned NotModified without cached headers.
    #[snafu(display("google cloud credentials returned NotModified without cached headers"))]
    GoogleCloudCredentialHeadersUnavailable,

    /// Failed to parse Google Cloud credentials JSON.
    #[snafu(display("failed to parse google cloud credentials JSON"))]
    GoogleCloudCredentialParse {
        /// The underlying JSON parse error.
        source: serde_json::Error,
    },

    /// Failed to build the Google Cloud Vertex AI client.
    #[snafu(display("failed to build google cloud vertex client"))]
    #[cfg(feature = "vertex")]
    GoogleCloudClientBuild {
        /// The underlying client builder error.
        source: google_cloud_gax::client_builder::Error,
    },

    /// Failed to send a request via the Google Cloud Vertex AI client.
    #[snafu(display("failed to send google cloud vertex request"))]
    #[cfg(feature = "vertex")]
    GoogleCloudRequest {
        /// The underlying Vertex AI error.
        source: google_cloud_aiplatform_v1::Error,
    },

    /// Failed to serialize a Google Cloud request payload.
    #[snafu(display("failed to serialize google cloud request"))]
    GoogleCloudRequestSerialize {
        /// The underlying serialization error.
        source: serde_json::Error,
    },

    /// Failed to deserialize a Google Cloud request payload.
    #[snafu(display("failed to deserialize google cloud request"))]
    GoogleCloudRequestDeserialize {
        /// The underlying deserialization error.
        source: serde_json::Error,
    },

    /// Failed to serialize a Google Cloud response.
    #[snafu(display("failed to serialize google cloud response"))]
    GoogleCloudResponseSerialize {
        /// The underlying serialization error.
        source: serde_json::Error,
    },

    /// Failed to deserialize a Google Cloud response.
    #[snafu(display("failed to deserialize google cloud response"))]
    GoogleCloudResponseDeserialize {
        /// The underlying deserialization error.
        source: serde_json::Error,
    },

    /// The Google Cloud request payload is not a JSON object.
    #[snafu(display("google cloud request payload is not an object"))]
    GoogleCloudRequestNotObject,

    /// Google Cloud configuration is required for this authentication mode.
    #[snafu(display("google cloud configuration is required for this authentication mode"))]
    MissingGoogleCloudConfig,

    /// Google Cloud authentication is required for this configuration.
    #[snafu(display("google cloud authentication is required for this configuration"))]
    MissingGoogleCloudAuth,

    /// The service account JSON is missing the `project_id` field.
    #[snafu(display("service account JSON is missing required field 'project_id'"))]
    MissingGoogleCloudProjectId,

    /// An API key is required for this configuration.
    #[snafu(display("api key is required for this configuration"))]
    MissingApiKey,

    /// The requested operation is not supported by the Vertex AI backend.
    #[snafu(display(
        "operation '{operation}' is not supported with the google cloud sdk backend (PredictionService currently exposes generateContent/embedContent only)"
    ))]
    GoogleCloudUnsupported {
        /// The unsupported operation name.
        operation: &'static str,
    },

    /// Failed to create a tokio runtime for the Google Cloud client.
    #[snafu(display("failed to create tokio runtime for google cloud client"))]
    TokioRuntime {
        /// The underlying I/O error.
        source: std::io::Error,
    },

    /// The Google Cloud client initialization thread panicked.
    #[snafu(display("google cloud client initialization thread panicked"))]
    GoogleCloudInitThreadPanicked,

    /// An I/O error occurred during file operations.
    #[snafu(display("I/O error during file operations"))]
    Io {
        /// The underlying I/O error.
        source: std::io::Error,
    },

    /// The generation config is invalid.
    #[snafu(display("invalid generation config: {message}"))]
    InvalidGenerationConfig {
        /// Description of the validation failure.
        message: String,
    },
}

// ══════════════════════════════════════════════════════════════════════
// GeminiClient — thin facade over a backend trait object
// ══════════════════════════════════════════════════════════════════════

/// Internal client for making requests to the Gemini API.
///
/// Delegates all operations to a [`GeminiBackend`](backend::GeminiBackend)
/// trait object (AI Studio REST or Vertex AI).
pub struct GeminiClient {
    /// The model this client is configured to use.
    pub model: Model,
    backend: Box<dyn backend::GeminiBackend>,
}

impl std::fmt::Debug for GeminiClient {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("GeminiClient")
            .field("model", &self.model)
            .field("backend", &self.backend)
            .finish()
    }
}

impl GeminiClient {
    /// Create a client backed by AI Studio REST.
    fn with_studio(model: Model, studio: backend::studio::StudioBackend) -> Self {
        Self { model, backend: Box::new(studio) }
    }

    /// Create a client backed by Vertex AI.
    #[cfg(feature = "vertex")]
    fn with_vertex(model: Model, vertex: backend::vertex::VertexBackend) -> Self {
        Self { model, backend: Box::new(vertex) }
    }

    // ── Delegating methods ──────────────────────────────────────────────

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
        if let Some(ref gc) = request.generation_config {
            gc.validate().map_err(|message| Error::InvalidGenerationConfig { message })?;
        }

        let response = self.backend.generate_content(request).await?;

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
    ) -> Result<backend::BackendStream<GenerationResponse>, Error> {
        if let Some(ref gc) = request.generation_config {
            gc.validate().map_err(|message| Error::InvalidGenerationConfig { message })?;
        }

        self.backend.generate_content_stream(request).await
    }

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
        self.backend.embed_content(request).await
    }

    #[instrument(skip_all, fields(batch.size = request.requests.len()))]
    pub(crate) async fn embed_content_batch(
        &self,
        request: BatchEmbedContentsRequest,
    ) -> Result<BatchContentEmbeddingResponse, Error> {
        self.backend.batch_embed_contents(request).await
    }

    #[instrument(skip_all, fields(
        batch.display_name = request.batch.display_name,
        batch.size = request.batch.input_config.batch_size(),
    ))]
    pub(crate) async fn batch_generate_content(
        &self,
        request: BatchGenerateContentRequest,
    ) -> Result<BatchGenerateContentResponse, Error> {
        self.backend.batch_generate_content(request).await
    }

    #[instrument(skip_all, fields(operation.name = name))]
    pub(crate) async fn get_batch_operation<T: serde::de::DeserializeOwned>(
        &self,
        name: &str,
    ) -> Result<T, Error> {
        let value = self.backend.get_batch_operation(name).await?;
        serde_json::from_value(value).context(DeserializeSnafu)
    }

    #[instrument(skip_all, fields(page.size = page_size, page.token.present = page_token.is_some()))]
    pub(crate) async fn list_batch_operations(
        &self,
        page_size: Option<u32>,
        page_token: Option<String>,
    ) -> Result<ListBatchesResponse, Error> {
        self.backend.list_batch_operations(page_size, page_token).await
    }

    #[instrument(skip_all, fields(page.size = page_size, page.token.present = page_token.is_some()))]
    pub(crate) async fn list_files(
        &self,
        page_size: Option<u32>,
        page_token: Option<String>,
    ) -> Result<ListFilesResponse, Error> {
        self.backend.list_files(page_size, page_token).await
    }

    #[instrument(skip_all, fields(operation.name = name))]
    pub(crate) async fn cancel_batch_operation(&self, name: &str) -> Result<(), Error> {
        self.backend.cancel_batch_operation(name).await
    }

    #[instrument(skip_all, fields(operation.name = name))]
    pub(crate) async fn delete_batch_operation(&self, name: &str) -> Result<(), Error> {
        self.backend.delete_batch_operation(name).await
    }

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
        self.backend.upload_file(display_name, file_bytes, mime_type).await
    }

    #[instrument(skip_all, fields(file.name = name))]
    pub(crate) async fn get_file(&self, name: &str) -> Result<File, Error> {
        self.backend.get_file(name).await
    }

    #[instrument(skip_all, fields(file.name = name))]
    pub(crate) async fn delete_file(&self, name: &str) -> Result<(), Error> {
        self.backend.delete_file(name).await
    }

    #[instrument(skip_all, fields(file.name = name))]
    pub(crate) async fn download_file(&self, name: &str) -> Result<Vec<u8>, Error> {
        self.backend.download_file(name).await
    }

    pub(crate) async fn create_cached_content(
        &self,
        cached_content: CreateCachedContentRequest,
    ) -> Result<CachedContent, Error> {
        self.backend.create_cached_content(cached_content).await
    }

    pub(crate) async fn get_cached_content(&self, name: &str) -> Result<CachedContent, Error> {
        self.backend.get_cached_content(name).await
    }

    pub(crate) async fn update_cached_content(
        &self,
        name: &str,
        expiration: CacheExpirationRequest,
    ) -> Result<CachedContent, Error> {
        self.backend.update_cached_content(name, expiration).await
    }

    pub(crate) async fn delete_cached_content(&self, name: &str) -> Result<(), Error> {
        self.backend.delete_cached_content(name).await
    }

    pub(crate) async fn list_cached_contents(
        &self,
        page_size: Option<i32>,
        page_token: Option<String>,
    ) -> Result<ListCachedContentsResponse, Error> {
        self.backend.list_cached_contents(page_size, page_token).await
    }

    // ── Model discovery ─────────────────────────────────────────────────

    #[instrument(skip_all, fields(page.size = page_size, page.token.present = page_token.is_some()))]
    pub(crate) async fn list_models(
        &self,
        page_size: Option<u32>,
        page_token: Option<String>,
    ) -> Result<crate::model_info::ListModelsResponse, Error> {
        self.backend.list_models(page_size, page_token).await
    }

    #[instrument(skip_all, fields(model.name = name))]
    pub(crate) async fn get_model(
        &self,
        name: &str,
    ) -> Result<crate::model_info::ModelInfo, Error> {
        self.backend.get_model(name).await
    }
}

// ══════════════════════════════════════════════════════════════════════
// Auth helpers & builder infrastructure
// ══════════════════════════════════════════════════════════════════════

#[cfg(feature = "vertex")]
#[derive(Debug, Clone)]
enum GoogleCloudAuth {
    ApiKey(String),
    Credentials(Credentials),
}

#[cfg(feature = "vertex")]
impl GoogleCloudAuth {
    fn credentials(&self) -> Result<Credentials, Error> {
        match self {
            GoogleCloudAuth::ApiKey(api_key) => {
                Ok(credentials::api_key_credentials::Builder::new(api_key).build())
            }
            GoogleCloudAuth::Credentials(credentials) => Ok(credentials.clone()),
        }
    }
}

#[cfg(feature = "vertex")]
#[derive(Debug, Clone)]
struct GoogleCloudConfig {
    project_id: String,
    location: String,
}

#[cfg(feature = "vertex")]
impl GoogleCloudConfig {
    fn endpoint(&self) -> String {
        if self.location == "global" {
            "https://aiplatform.googleapis.com".to_string()
        } else {
            format!("https://{}-aiplatform.googleapis.com", self.location)
        }
    }
}

#[cfg(feature = "vertex")]
fn extract_service_account_project_id(service_account_json: &str) -> Result<String, Error> {
    let value: serde_json::Value =
        serde_json::from_str(service_account_json).context(GoogleCloudCredentialParseSnafu)?;

    let project_id = value
        .get("project_id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or(Error::MissingGoogleCloudProjectId)?;

    Ok(project_id.to_string())
}

#[cfg(feature = "vertex")]
fn build_vertex_prediction_service(
    endpoint: String,
    credentials: Credentials,
) -> Result<PredictionService, Error> {
    let build_in_runtime =
        |endpoint: String, credentials: Credentials| -> Result<PredictionService, Error> {
            let runtime = tokio::runtime::Runtime::new().context(TokioRuntimeSnafu)?;
            runtime
                .block_on(
                    PredictionService::builder()
                        .with_endpoint(endpoint)
                        .with_credentials(credentials)
                        .build(),
                )
                .context(GoogleCloudClientBuildSnafu)
        };

    if tokio::runtime::Handle::try_current().is_ok() {
        let worker = std::thread::Builder::new()
            .name("adk-gemini-vertex-init".to_string())
            .spawn(move || build_in_runtime(endpoint, credentials))
            .map_err(|source| Error::TokioRuntime { source })?;

        return worker.join().map_err(|_| Error::GoogleCloudInitThreadPanicked)?;
    }

    build_in_runtime(endpoint, credentials)
}

// ══════════════════════════════════════════════════════════════════════
// GeminiBuilder
// ══════════════════════════════════════════════════════════════════════

/// A builder for the `Gemini` client.
///
/// # Examples
///
/// ## Basic usage
///
/// ```no_run
/// use adk_gemini::{GeminiBuilder, Model};
///
/// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
/// let gemini = GeminiBuilder::new("YOUR_API_KEY")
///     .with_model(Model::Gemini25Pro)
///     .build()?;
/// # Ok(())
/// # }
/// ```
pub struct GeminiBuilder {
    model: Model,
    client_builder: ClientBuilder,
    base_url: Url,
    #[cfg(feature = "vertex")]
    google_cloud: Option<GoogleCloudConfig>,
    api_key: Option<String>,
    #[cfg(feature = "vertex")]
    google_cloud_auth: Option<GoogleCloudAuth>,
}

impl GeminiBuilder {
    /// Create a new builder with the given API key.
    pub fn new<K: Into<String>>(key: K) -> Self {
        Self {
            model: Model::default(),
            client_builder: ClientBuilder::default(),
            base_url: DEFAULT_BASE_URL.clone(),
            #[cfg(feature = "vertex")]
            google_cloud: None,
            api_key: Some(key.into()),
            #[cfg(feature = "vertex")]
            google_cloud_auth: None,
        }
    }

    /// Set the model to use.
    pub fn with_model<M: Into<Model>>(mut self, model: M) -> Self {
        self.model = model.into();
        self
    }

    /// Set a custom HTTP client builder.
    pub fn with_http_client(mut self, client_builder: ClientBuilder) -> Self {
        self.client_builder = client_builder;
        self
    }

    /// Set a custom base URL (overrides Google Cloud config).
    pub fn with_base_url(mut self, base_url: Url) -> Self {
        self.base_url = base_url;
        #[cfg(feature = "vertex")]
        {
            self.google_cloud = None;
            self.google_cloud_auth = None;
        }
        self
    }

    /// Authenticate with a service account JSON key (Vertex AI).
    #[cfg(feature = "vertex")]
    pub fn with_service_account_json(mut self, service_account_json: &str) -> Result<Self, Error> {
        let value =
            serde_json::from_str(service_account_json).context(GoogleCloudCredentialParseSnafu)?;
        let credentials = google_cloud_auth::credentials::service_account::Builder::new(value)
            .build()
            .context(GoogleCloudAuthSnafu)?;
        self.google_cloud_auth = Some(GoogleCloudAuth::Credentials(credentials));
        Ok(self)
    }

    /// Set the Google Cloud project and location for Vertex AI.
    #[cfg(feature = "vertex")]
    pub fn with_google_cloud<P: Into<String>, L: Into<String>>(
        mut self,
        project_id: P,
        location: L,
    ) -> Self {
        self.google_cloud =
            Some(GoogleCloudConfig { project_id: project_id.into(), location: location.into() });
        self
    }

    /// Use Application Default Credentials (ADC) for Vertex AI authentication.
    #[cfg(feature = "vertex")]
    pub fn with_google_cloud_adc(mut self) -> Result<Self, Error> {
        let credentials = google_cloud_auth::credentials::Builder::default()
            .build()
            .context(GoogleCloudAuthSnafu)?;
        self.google_cloud_auth = Some(GoogleCloudAuth::Credentials(credentials));
        Ok(self)
    }

    /// Use Workload Identity Federation JSON for Vertex AI authentication.
    #[cfg(feature = "vertex")]
    pub fn with_google_cloud_wif_json(mut self, wif_json: &str) -> Result<Self, Error> {
        let value = serde_json::from_str(wif_json).context(GoogleCloudCredentialParseSnafu)?;
        let credentials = google_cloud_auth::credentials::external_account::Builder::new(value)
            .build()
            .context(GoogleCloudAuthSnafu)?;
        self.google_cloud_auth = Some(GoogleCloudAuth::Credentials(credentials));
        Ok(self)
    }

    /// Builds the `Gemini` client.
    pub fn build(self) -> Result<Gemini, Error> {
        #[cfg(feature = "vertex")]
        {
            if self.google_cloud.is_none() && self.google_cloud_auth.is_some() {
                return MissingGoogleCloudConfigSnafu.fail();
            }

            // ── Vertex AI path ──────────────────────────────────────────────
            if let Some(config) = &self.google_cloud {
                let model = Model::Custom(
                    self.model.vertex_model_path(&config.project_id, &config.location),
                );
                let google_cloud_auth = match self.google_cloud_auth {
                    Some(auth) => auth,
                    None => match self.api_key {
                        Some(api_key) if !api_key.is_empty() => GoogleCloudAuth::ApiKey(api_key),
                        _ => return MissingGoogleCloudAuthSnafu.fail(),
                    },
                };
                let credentials = google_cloud_auth.credentials()?;
                let endpoint = config.endpoint();
                let prediction =
                    build_vertex_prediction_service(endpoint.clone(), credentials.clone())?;

                let vertex = backend::vertex::VertexBackend::new(
                    model.clone(),
                    prediction,
                    credentials,
                    endpoint,
                );

                return Ok(Gemini { client: Arc::new(GeminiClient::with_vertex(model, vertex)) });
            }
        }

        // ── AI Studio REST path ─────────────────────────────────────────
        let api_key = self.api_key.ok_or(Error::MissingApiKey)?;
        if api_key.is_empty() {
            return MissingApiKeySnafu.fail();
        }

        let studio =
            backend::studio::StudioBackend::new(&api_key, self.model.clone(), self.base_url)?;

        Ok(Gemini { client: Arc::new(GeminiClient::with_studio(self.model, studio)) })
    }
}

// ══════════════════════════════════════════════════════════════════════
// Gemini — the main public-facing client
// ══════════════════════════════════════════════════════════════════════

/// The main public-facing Gemini API client.
///
/// Provides methods for content generation, embeddings, batch processing,
/// file management, caching, and model discovery.
pub struct Gemini {
    client: Arc<GeminiClient>,
}

impl Gemini {
    /// Create a new client with the specified API key
    pub fn new<K: AsRef<str>>(api_key: K) -> Result<Self, Error> {
        Self::with_model(api_key, Model::default())
    }

    /// Create a new client for the Gemini 3.1 Pro model
    pub fn pro<K: AsRef<str>>(api_key: K) -> Result<Self, Error> {
        Self::with_model(api_key, Model::Gemini31ProPreview)
    }

    /// Create a new client with the specified API key and model
    pub fn with_model<K: AsRef<str>, M: Into<Model>>(api_key: K, model: M) -> Result<Self, Error> {
        Self::with_model_and_base_url(api_key, model, DEFAULT_BASE_URL.clone())
    }

    /// Create a new client with the specified API key using the v1 (stable) API.
    pub fn with_v1<K: AsRef<str>>(api_key: K) -> Result<Self, Error> {
        Self::with_model_and_base_url(api_key, Model::default(), V1_BASE_URL.clone())
    }

    /// Create a new client with the specified API key and model using the v1 (stable) API.
    pub fn with_model_v1<K: AsRef<str>, M: Into<Model>>(
        api_key: K,
        model: M,
    ) -> Result<Self, Error> {
        Self::with_model_and_base_url(api_key, model, V1_BASE_URL.clone())
    }

    /// Create a new client with custom base URL
    pub fn with_base_url<K: AsRef<str>>(api_key: K, base_url: Url) -> Result<Self, Error> {
        Self::with_model_and_base_url(api_key, Model::default(), base_url)
    }

    /// Create a new client using Vertex AI (Google Cloud) endpoints.
    #[cfg(feature = "vertex")]
    pub fn with_google_cloud<K: AsRef<str>, P: AsRef<str>, L: AsRef<str>>(
        api_key: K,
        project_id: P,
        location: L,
    ) -> Result<Self, Error> {
        Self::with_google_cloud_model(api_key, project_id, location, Model::default())
    }

    /// Create a new client using Vertex AI (Google Cloud) endpoints and a specific model.
    #[cfg(feature = "vertex")]
    pub fn with_google_cloud_model<K: AsRef<str>, P: AsRef<str>, L: AsRef<str>, M: Into<Model>>(
        api_key: K,
        project_id: P,
        location: L,
        model: M,
    ) -> Result<Self, Error> {
        GeminiBuilder::new(api_key.as_ref())
            .with_model(model)
            .with_google_cloud(project_id.as_ref(), location.as_ref())
            .build()
    }

    /// Create a new client using Vertex AI (Google Cloud) endpoints with Application Default Credentials (ADC).
    #[cfg(feature = "vertex")]
    pub fn with_google_cloud_adc<P: AsRef<str>, L: AsRef<str>>(
        project_id: P,
        location: L,
    ) -> Result<Self, Error> {
        Self::with_google_cloud_adc_model(project_id, location, Model::default())
    }

    /// Create a new client using Vertex AI (Google Cloud) endpoints and a specific model with ADC.
    #[cfg(feature = "vertex")]
    pub fn with_google_cloud_adc_model<P: AsRef<str>, L: AsRef<str>, M: Into<Model>>(
        project_id: P,
        location: L,
        model: M,
    ) -> Result<Self, Error> {
        GeminiBuilder::new("")
            .with_model(model)
            .with_google_cloud(project_id.as_ref(), location.as_ref())
            .with_google_cloud_adc()?
            .build()
    }

    /// Create a new client using Vertex AI (Google Cloud) endpoints and Workload Identity Federation JSON.
    #[cfg(feature = "vertex")]
    pub fn with_google_cloud_wif_json<P: AsRef<str>, L: AsRef<str>, M: Into<Model>>(
        wif_json: &str,
        project_id: P,
        location: L,
        model: M,
    ) -> Result<Self, Error> {
        GeminiBuilder::new("")
            .with_model(model)
            .with_google_cloud(project_id.as_ref(), location.as_ref())
            .with_google_cloud_wif_json(wif_json)?
            .build()
    }

    /// Create a new client using a service account JSON key.
    #[cfg(feature = "vertex")]
    pub fn with_service_account_json(service_account_json: &str) -> Result<Self, Error> {
        Self::with_service_account_json_model(service_account_json, Model::default())
    }

    /// Create a new client using a service account JSON key and a specific model.
    #[cfg(feature = "vertex")]
    pub fn with_service_account_json_model<M: Into<Model>>(
        service_account_json: &str,
        model: M,
    ) -> Result<Self, Error> {
        let project_id = extract_service_account_project_id(service_account_json)?;
        GeminiBuilder::new("")
            .with_model(model)
            .with_service_account_json(service_account_json)?
            .with_google_cloud(project_id, "us-central1")
            .build()
    }

    /// Create a new client using Vertex AI (Google Cloud) endpoints and a service account JSON key.
    #[cfg(feature = "vertex")]
    pub fn with_google_cloud_service_account_json<M: Into<Model>>(
        service_account_json: &str,
        project_id: &str,
        location: &str,
        model: M,
    ) -> Result<Self, Error> {
        GeminiBuilder::new("")
            .with_model(model)
            .with_service_account_json(service_account_json)?
            .with_google_cloud(project_id, location)
            .build()
    }

    /// Create a new client with the specified API key, model, and base URL
    pub fn with_model_and_base_url<K: AsRef<str>, M: Into<Model>>(
        api_key: K,
        model: M,
        base_url: Url,
    ) -> Result<Self, Error> {
        let model = model.into();
        let studio =
            backend::studio::StudioBackend::new(api_key.as_ref(), model.clone(), base_url)?;
        Ok(Self { client: Arc::new(GeminiClient::with_studio(model, studio)) })
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

    // ── Model discovery ─────────────────────────────────────────────────

    /// Lists available Gemini models with pagination.
    ///
    /// Returns a stream of [`ModelInfo`](crate::model_info::ModelInfo) items.
    /// This is useful for discovering which models are available and their
    /// capabilities (token limits, supported methods, etc.).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use futures::StreamExt;
    ///
    /// let gemini = Gemini::new("YOUR_API_KEY")?;
    /// let mut models = gemini.list_models(None);
    /// while let Some(model) = models.next().await {
    ///     let model = model?;
    ///     println!("{}: {}", model.name, model.display_name);
    /// }
    /// ```
    pub fn list_models(
        &self,
        page_size: impl Into<Option<u32>>,
    ) -> impl Stream<Item = Result<crate::model_info::ModelInfo, Error>> + Send {
        let client = self.client.clone();
        let page_size = page_size.into();
        async_stream::try_stream! {
            let mut page_token: Option<String> = None;
            loop {
                let response = client
                    .list_models(page_size, page_token.clone())
                    .await?;

                for model in response.models {
                    yield model;
                }

                if let Some(next_page_token) = response.next_page_token {
                    page_token = Some(next_page_token);
                } else {
                    break;
                }
            }
        }
    }

    /// Get metadata for a specific model by name.
    ///
    /// The name can be provided with or without the `models/` prefix
    /// (e.g. both `"gemini-2.5-flash"` and `"models/gemini-2.5-flash"` work).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let gemini = Gemini::new("YOUR_API_KEY")?;
    /// let info = gemini.get_model("gemini-2.5-flash").await?;
    /// println!("Input limit: {} tokens", info.input_token_limit);
    /// println!("Output limit: {} tokens", info.output_token_limit);
    /// ```
    pub async fn get_model(&self, name: &str) -> Result<crate::model_info::ModelInfo, Error> {
        self.client.get_model(name).await
    }
}

// ══════════════════════════════════════════════════════════════════════
// Tests
// ══════════════════════════════════════════════════════════════════════

#[cfg(test)]
#[cfg(feature = "vertex")]
mod client_tests {
    use super::{Error, GoogleCloudConfig, extract_service_account_project_id};
    use crate::backend::vertex::VertexBackend;

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

    #[test]
    fn vertex_transport_error_detection_matches_http2_failure() {
        assert!(VertexBackend::is_transport_error(
            "the transport reports an error: client error (SendRequest): http2 error"
        ));
        assert!(!VertexBackend::is_transport_error("permission denied"));
    }

    #[test]
    fn vertex_regional_endpoint_uses_location_prefix() {
        let config = GoogleCloudConfig {
            project_id: "my-project".to_string(),
            location: "us-central1".to_string(),
        };
        assert_eq!(config.endpoint(), "https://us-central1-aiplatform.googleapis.com");
    }

    #[test]
    fn vertex_global_endpoint_omits_location_prefix() {
        let config = GoogleCloudConfig {
            project_id: "my-project".to_string(),
            location: "global".to_string(),
        };
        assert_eq!(config.endpoint(), "https://aiplatform.googleapis.com");
    }

    #[test]
    fn vertex_other_regional_endpoint_formats_correctly() {
        let config = GoogleCloudConfig {
            project_id: "my-project".to_string(),
            location: "europe-west4".to_string(),
        };
        assert_eq!(config.endpoint(), "https://europe-west4-aiplatform.googleapis.com");
    }
}
