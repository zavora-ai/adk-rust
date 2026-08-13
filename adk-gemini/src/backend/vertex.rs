//! Vertex AI backend for the Gemini API.
//!
//! This backend communicates with `{region}-aiplatform.googleapis.com` for
//! regional endpoints, or `aiplatform.googleapis.com` when the location is
//! `global`. It uses Google Cloud credentials (ADC, service account, WIF, or
//! API key), the gRPC SDK for non-streaming requests (with REST fallback on
//! transport errors), and REST SSE for streaming.
//!
//! Cached-content operations (create, get, update, list, delete) go through the
//! Vertex REST endpoint
//! `{endpoint}/v1/projects/{project}/locations/{location}/cachedContents`.
//! The Files API, batch operations, and the Interactions API remain
//! [`GoogleCloudUnsupported`](crate::client::Error::GoogleCloudUnsupported) on
//! this backend — a deliberate exclusion: Vertex AI has no Files API
//! equivalent, batch prediction uses a different resource model (BigQuery/GCS
//! jobs), and the Interactions API is Studio-only while in Beta.
//!
//! Streaming support inspired by [PR #74](https://github.com/zavora-ai/adk-rust/pull/74)
//! by @mikefaille.

use super::{BackendStream, GeminiBackend};
use crate::{
    cache::model::{
        CacheExpirationRequest, CachedContent, CreateCachedContentRequest,
        ListCachedContentsResponse,
    },
    client::{
        BadResponseSnafu, DecodeResponseSnafu, DeserializeSnafu, Error,
        GoogleCloudCredentialHeadersSnafu, GoogleCloudCredentialHeadersUnavailableSnafu,
        GoogleCloudRequestDeserializeSnafu, GoogleCloudRequestNotObjectSnafu,
        GoogleCloudRequestSerializeSnafu, GoogleCloudResponseDeserializeSnafu,
        GoogleCloudResponseSerializeSnafu, Model, UrlParseSnafu, ValidationSnafu,
    },
    embedding::{ContentEmbeddingResponse, EmbedContentRequest},
    generation::{GenerateContentRequest, GenerationResponse},
};
use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures::TryStreamExt;
use google_cloud_aiplatform_v1::client::PredictionService;
use google_cloud_auth::credentials::Credentials;
use reqwest::Client;
use serde_json::json;
use snafu::{OptionExt, ResultExt};
use tracing::{debug, instrument};
use url::Url;

/// Vertex AI backend.
///
/// Supports content generation, streaming, embeddings, and cached-content
/// management. The Files API, batch operations, and the Interactions API are
/// deliberately left unsupported (see the module documentation).
#[derive(Debug)]
pub struct VertexBackend {
    pub(crate) prediction: PredictionService,
    pub(crate) credentials: Credentials,
    pub(crate) endpoint: String,
    pub(crate) model: Model,
}

impl VertexBackend {
    /// Create a new Vertex backend.
    pub fn new(
        model: Model,
        prediction: PredictionService,
        credentials: Credentials,
        endpoint: String,
    ) -> Self {
        Self { prediction, credentials, endpoint, model }
    }

    /// Get auth headers from credentials.
    async fn auth_headers(&self) -> Result<reqwest::header::HeaderMap, Error> {
        match self
            .credentials
            .headers(Default::default())
            .await
            .context(GoogleCloudCredentialHeadersSnafu)?
        {
            google_cloud_auth::credentials::CacheableResource::New { data, .. } => Ok(data),
            google_cloud_auth::credentials::CacheableResource::NotModified => {
                GoogleCloudCredentialHeadersUnavailableSnafu.fail()
            }
        }
    }

    /// Check HTTP response status.
    async fn check_response(response: reqwest::Response) -> Result<reqwest::Response, Error> {
        let status = response.status();
        if !status.is_success() {
            let description = response.text().await.ok();
            BadResponseSnafu { code: status.as_u16(), description }.fail()
        } else {
            Ok(response)
        }
    }

    /// Returns `true` if the error message indicates a transient transport-layer
    /// failure (HTTP/2 stream errors, send-request failures) that is safe to
    /// retry, as opposed to a deterministic application error.
    pub fn is_transport_error(message: &str) -> bool {
        let normalized = message.to_ascii_lowercase();
        normalized.contains("transport reports an error")
            || normalized.contains("http2 error")
            || normalized.contains("client error (sendrequest)")
            || normalized.contains("stream error")
    }

    /// Strip fields from the request that Vertex AI doesn't support.
    ///
    /// The `includeServerSideToolInvocations` field is only supported by the
    /// AI Studio REST API (generativelanguage.googleapis.com). Vertex AI
    /// (aiplatform.googleapis.com) rejects it with `INVALID_ARGUMENT`.
    fn strip_unsupported_fields(request: &mut GenerateContentRequest) {
        if let Some(ref mut tc) = request.tool_config {
            tc.include_server_side_tool_invocations = None;
        }
    }

    /// Non-streaming generate via REST (fallback when gRPC has transport issues).
    async fn generate_content_rest(
        &self,
        request: &GenerateContentRequest,
    ) -> Result<GenerationResponse, Error> {
        let url = Url::parse(&format!(
            "{}/v1/{}:generateContent",
            self.endpoint.trim_end_matches('/'),
            self.model
        ))
        .context(UrlParseSnafu)?;

        let auth_headers = self.auth_headers().await?;

        let response = Client::new()
            .post(url.clone())
            .headers(auth_headers)
            .query(&[("$alt", "json;enum-encoding=int")])
            .json(request)
            .send()
            .await
            .map_err(|source| Error::PerformRequest { source, url })?;
        let response = Self::check_response(response).await?;

        let vertex_resp: google_cloud_aiplatform_v1::model::GenerateContentResponse =
            response.json().await.context(DecodeResponseSnafu)?;
        let value =
            serde_json::to_value(&vertex_resp).context(GoogleCloudResponseSerializeSnafu)?;
        serde_json::from_value(value).context(GoogleCloudResponseDeserializeSnafu)
    }

    // ── Cached-content plumbing ──────────────────────────────────

    /// Extract `(project, location)` from the model resource path.
    ///
    /// The backend's model is always a full Vertex resource name of the form
    /// `projects/{project}/locations/{location}/...` (see
    /// [`Model::vertex_model_path`]), so the cachedContents parent is derived
    /// from it rather than stored separately.
    fn project_and_location(&self) -> Result<(String, String), Error> {
        let model = self.model.to_string();
        let mut segments = model.split('/');
        match (segments.next(), segments.next(), segments.next(), segments.next()) {
            (Some("projects"), Some(project), Some("locations"), Some(location))
                if !project.is_empty() && !location.is_empty() =>
            {
                Ok((project.to_string(), location.to_string()))
            }
            _ => ValidationSnafu {
                message: format!(
                    "vertex model path '{model}' does not start with \
                     'projects/{{project}}/locations/{{location}}'; cached content on Vertex AI \
                     requires the full model resource name — build the client with \
                     GeminiBuilder::with_google_cloud"
                ),
            }
            .fail(),
        }
    }

    /// Build a cachedContents URL.
    ///
    /// `None` yields the collection URL (create/list). `Some(name)` accepts a
    /// full resource name (`projects/…/cachedContents/{id}`), a Studio-style
    /// name (`cachedContents/{id}`), or a bare id, and resolves all three to
    /// the full Vertex resource URL.
    fn cache_url(&self, name: Option<&str>) -> Result<Url, Error> {
        let (project, location) = self.project_and_location()?;
        let parent = format!("projects/{project}/locations/{location}");
        let suffix = match name {
            None => format!("{parent}/cachedContents"),
            Some(n) if n.starts_with("projects/") => n.to_string(),
            Some(n) => {
                let id = n.strip_prefix("cachedContents/").unwrap_or(n);
                format!("{parent}/cachedContents/{id}")
            }
        };
        Url::parse(&format!("{}/v1/{suffix}", self.endpoint.trim_end_matches('/')))
            .context(UrlParseSnafu)
    }

    /// GET a JSON resource with auth headers.
    async fn get_json<T: serde::de::DeserializeOwned>(&self, url: Url) -> Result<T, Error> {
        let auth_headers = self.auth_headers().await?;
        let response = Client::new()
            .get(url.clone())
            .headers(auth_headers)
            .send()
            .await
            .map_err(|source| Error::PerformRequest { source, url })?;
        let response = Self::check_response(response).await?;
        response.json().await.context(DecodeResponseSnafu)
    }

    /// POST a JSON body with auth headers and decode the JSON response.
    async fn post_json<Req: serde::Serialize, Res: serde::de::DeserializeOwned>(
        &self,
        url: Url,
        body: &Req,
    ) -> Result<Res, Error> {
        let auth_headers = self.auth_headers().await?;
        let response = Client::new()
            .post(url.clone())
            .headers(auth_headers)
            .json(body)
            .send()
            .await
            .map_err(|source| Error::PerformRequest { source, url })?;
        let response = Self::check_response(response).await?;
        response.json().await.context(DecodeResponseSnafu)
    }
}

#[async_trait]
impl GeminiBackend for VertexBackend {
    async fn generate_content(
        &self,
        request: GenerateContentRequest,
    ) -> Result<GenerationResponse, Error> {
        // Strip fields unsupported by Vertex AI before sending.
        let mut request = request;
        Self::strip_unsupported_fields(&mut request);

        // Try gRPC first, fall back to REST on transport errors.
        let rest_request = request.clone();
        let mut request_value =
            serde_json::to_value(&request).context(GoogleCloudRequestSerializeSnafu)?;
        let model = self.model.to_string();
        let request_object =
            request_value.as_object_mut().context(GoogleCloudRequestNotObjectSnafu)?;
        request_object.insert("model".to_string(), serde_json::Value::String(model));

        let grpc_request: google_cloud_aiplatform_v1::model::GenerateContentRequest =
            serde_json::from_value(request_value).context(GoogleCloudRequestDeserializeSnafu)?;

        match self.prediction.generate_content().with_request(grpc_request).send().await {
            Ok(response) => {
                let value =
                    serde_json::to_value(&response).context(GoogleCloudResponseSerializeSnafu)?;
                serde_json::from_value(value).context(GoogleCloudResponseDeserializeSnafu)
            }
            Err(source) => {
                if Self::is_transport_error(&source.to_string()) {
                    tracing::warn!(
                        error = %source,
                        "Vertex SDK transport error on generateContent; falling back to REST"
                    );
                    self.generate_content_rest(&rest_request).await
                } else {
                    Err(Error::GoogleCloudRequest { source })
                }
            }
        }
    }

    async fn generate_content_stream(
        &self,
        request: GenerateContentRequest,
    ) -> Result<BackendStream<GenerationResponse>, Error> {
        // Strip fields unsupported by Vertex AI before sending.
        let mut request = request;
        Self::strip_unsupported_fields(&mut request);

        // Vertex AI REST supports streamGenerateContent with SSE, same as AI Studio.
        let url = Url::parse(&format!(
            "{}/v1/{}:streamGenerateContent?alt=sse",
            self.endpoint.trim_end_matches('/'),
            self.model
        ))
        .context(UrlParseSnafu)?;

        let auth_headers = self.auth_headers().await?;

        let response = Client::new()
            .post(url.clone())
            .headers(auth_headers)
            .json(&request)
            .send()
            .await
            .map_err(|source| Error::PerformRequest { source, url })?;
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
        // Use REST for embeddings (same pattern as existing code).
        let content_value =
            serde_json::to_value(&request.content).context(GoogleCloudRequestSerializeSnafu)?;
        let content: google_cloud_aiplatform_v1::model::Content =
            serde_json::from_value(content_value).context(GoogleCloudRequestDeserializeSnafu)?;

        // Build EmbedContentConfig with title, task_type, output_dimensionality
        let mut config =
            google_cloud_aiplatform_v1::model::embed_content_request::EmbedContentConfig::new();
        if let Some(title) = request.title {
            config = config.set_title(title);
        }
        if let Some(task_type) = request.task_type {
            let task_type =
                google_cloud_aiplatform_v1::model::embed_content_request::EmbeddingTaskType::from(
                    task_type.as_ref(),
                );
            config = config.set_task_type(task_type);
        }
        if let Some(output_dimensionality) = request.output_dimensionality {
            config = config.set_output_dimensionality(output_dimensionality);
        }

        let vertex_request = google_cloud_aiplatform_v1::model::EmbedContentRequest::new()
            .set_content(content)
            .set_embed_content_config(config);

        let url = Url::parse(&format!(
            "{}/v1/{}:embedContent",
            self.endpoint.trim_end_matches('/'),
            self.model
        ))
        .context(UrlParseSnafu)?;

        let auth_headers = self.auth_headers().await?;

        let response = Client::new()
            .post(url.clone())
            .headers(auth_headers)
            .query(&[("$alt", "json;enum-encoding=int")])
            .json(&vertex_request)
            .send()
            .await
            .map_err(|source| Error::PerformRequest { source, url })?;
        let response = Self::check_response(response).await?;

        let vertex_resp: google_cloud_aiplatform_v1::model::EmbedContentResponse =
            response.json().await.context(DecodeResponseSnafu)?;
        let value =
            serde_json::to_value(&vertex_resp).context(GoogleCloudResponseSerializeSnafu)?;
        serde_json::from_value(value).context(GoogleCloudResponseDeserializeSnafu)
    }

    // ── Cache operations ────────────────────────────────────────

    #[instrument(skip_all, fields(
        display.name = request.display_name,
        contents.count = request.contents.as_ref().map(Vec::len),
        tools.present = request.tools.is_some(),
        system.instruction.present = request.system_instruction.is_some(),
    ))]
    async fn create_cached_content(
        &self,
        request: CreateCachedContentRequest,
    ) -> Result<CachedContent, Error> {
        let (project, location) = self.project_and_location()?;
        // Vertex requires the full model resource name in the payload, unlike
        // Studio's `models/{model}` form.
        let mut request = request;
        request.model = Model::Custom(request.model.vertex_model_path(&project, &location));

        let url = self.cache_url(None)?;
        debug!(cache.model = %request.model, "creating cached content");
        self.post_json(url, &request).await
    }

    #[instrument(skip_all, fields(cache.name = name))]
    async fn get_cached_content(&self, name: &str) -> Result<CachedContent, Error> {
        let url = self.cache_url(Some(name))?;
        self.get_json(url).await
    }

    #[instrument(skip_all, fields(
        page.size = page_size,
        page.token.present = page_token.is_some(),
    ))]
    async fn list_cached_contents(
        &self,
        page_size: Option<i32>,
        page_token: Option<String>,
    ) -> Result<ListCachedContentsResponse, Error> {
        let mut url = self.cache_url(None)?;
        if let Some(size) = page_size {
            url.query_pairs_mut().append_pair("pageSize", &size.to_string());
        }
        if let Some(token) = page_token {
            url.query_pairs_mut().append_pair("pageToken", &token);
        }
        self.get_json(url).await
    }

    #[instrument(skip_all, fields(cache.name = name))]
    async fn update_cached_content(
        &self,
        name: &str,
        expiration: CacheExpirationRequest,
    ) -> Result<CachedContent, Error> {
        let mut url = self.cache_url(Some(name))?;
        // Only `ttl` and `expireTime` are mutable; scope the PATCH accordingly.
        let (update_mask, update_payload) = match expiration {
            CacheExpirationRequest::Ttl { ttl } => ("ttl", json!({ "ttl": ttl })),
            CacheExpirationRequest::ExpireTime { expire_time } => (
                "expireTime",
                json!({
                    "expireTime": expire_time
                        .format(&time::format_description::well_known::Rfc3339)
                        .unwrap()
                }),
            ),
        };
        url.query_pairs_mut().append_pair("updateMask", update_mask);
        debug!(cache.update_mask = update_mask, "updating cached content expiration");

        let auth_headers = self.auth_headers().await?;
        let response = Client::new()
            .patch(url.clone())
            .headers(auth_headers)
            .json(&update_payload)
            .send()
            .await
            .map_err(|source| Error::PerformRequest { source, url })?;
        let response = Self::check_response(response).await?;
        response.json().await.context(DecodeResponseSnafu)
    }

    #[instrument(skip_all, fields(cache.name = name))]
    async fn delete_cached_content(&self, name: &str) -> Result<(), Error> {
        let url = self.cache_url(Some(name))?;
        let auth_headers = self.auth_headers().await?;
        let response = Client::new()
            .delete(url.clone())
            .headers(auth_headers)
            .send()
            .await
            .map_err(|source| Error::PerformRequest { source, url })?;
        Self::check_response(response).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Content;
    use serde_json::{Value, json};
    use wiremock::matchers::{body_partial_json, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const PARENT: &str = "projects/test-project/locations/us-central1";

    async fn backend_with_model(endpoint: &str, model: Model) -> VertexBackend {
        let credentials =
            google_cloud_auth::credentials::api_key_credentials::Builder::new("test-key").build();
        let prediction = PredictionService::builder()
            .with_endpoint(endpoint)
            .with_credentials(credentials.clone())
            .build()
            .await
            .expect("prediction service should build offline");
        VertexBackend::new(model, prediction, credentials, endpoint.to_string())
    }

    async fn backend(endpoint: &str) -> VertexBackend {
        backend_with_model(
            endpoint,
            Model::Custom(format!("{PARENT}/publishers/google/models/gemini-2.5-flash")),
        )
        .await
    }

    fn cached_content_json(name: &str) -> Value {
        json!({
            "name": name,
            "model": format!("{PARENT}/publishers/google/models/gemini-2.5-flash"),
            "createTime": "2026-01-01T00:00:00Z",
            "updateTime": "2026-01-01T00:10:00Z",
            "expireTime": "2026-01-01T01:00:00Z",
            "usageMetadata": { "totalTokenCount": 2048 }
        })
    }

    #[tokio::test]
    async fn create_cached_content_posts_to_collection_and_normalizes_model() {
        let server = MockServer::start().await;
        let cache_name = format!("{PARENT}/cachedContents/cache-123");
        Mock::given(method("POST"))
            .and(path(format!("/v1/{PARENT}/cachedContents")))
            .and(body_partial_json(json!({
                "model": format!("{PARENT}/publishers/google/models/gemini-2.5-flash"),
                "displayName": "test-cache",
                "ttl": "3600s",
            })))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(cached_content_json(&cache_name)),
            )
            .expect(1)
            .mount(&server)
            .await;

        let backend = backend(&server.uri()).await;
        // A Studio-style model (`models/gemini-2.5-flash`) must be normalized
        // to the full Vertex resource name in the payload.
        let request = CreateCachedContentRequest {
            display_name: Some("test-cache".to_string()),
            model: Model::Gemini25Flash,
            contents: Some(vec![Content::text("context to cache")]),
            tools: None,
            system_instruction: None,
            tool_config: None,
            expiration: CacheExpirationRequest::Ttl { ttl: "3600s".to_string() },
        };

        let created = backend.create_cached_content(request).await.expect("create should succeed");
        assert_eq!(created.name, cache_name);
        assert_eq!(created.usage_metadata.total_token_count, 2048);
        assert!(created.expiration.expire_time.is_some());
    }

    #[tokio::test]
    async fn get_cached_content_resolves_bare_prefixed_and_full_names() {
        let server = MockServer::start().await;
        let cache_name = format!("{PARENT}/cachedContents/cache-123");
        Mock::given(method("GET"))
            .and(path(format!("/v1/{cache_name}")))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(cached_content_json(&cache_name)),
            )
            .expect(3)
            .mount(&server)
            .await;

        let backend = backend(&server.uri()).await;
        for name in ["cache-123", "cachedContents/cache-123", cache_name.as_str()] {
            let cached = backend.get_cached_content(name).await.expect("get should succeed");
            assert_eq!(cached.name, cache_name);
        }
    }

    #[tokio::test]
    async fn list_cached_contents_sends_pagination_query() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/v1/{PARENT}/cachedContents")))
            .and(query_param("pageSize", "5"))
            .and(query_param("pageToken", "tok-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "cachedContents": [cached_content_json(&format!("{PARENT}/cachedContents/cache-1"))],
                "nextPageToken": "tok-2",
            })))
            .expect(1)
            .mount(&server)
            .await;

        let backend = backend(&server.uri()).await;
        let response = backend
            .list_cached_contents(Some(5), Some("tok-1".to_string()))
            .await
            .expect("list should succeed");
        assert_eq!(response.cached_contents.len(), 1);
        assert_eq!(response.cached_contents[0].name, format!("{PARENT}/cachedContents/cache-1"));
        assert_eq!(response.next_page_token.as_deref(), Some("tok-2"));
    }

    #[tokio::test]
    async fn update_cached_content_patches_ttl_with_update_mask() {
        let server = MockServer::start().await;
        let cache_name = format!("{PARENT}/cachedContents/cache-123");
        Mock::given(method("PATCH"))
            .and(path(format!("/v1/{cache_name}")))
            .and(query_param("updateMask", "ttl"))
            .and(body_partial_json(json!({ "ttl": "600s" })))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(cached_content_json(&cache_name)),
            )
            .expect(1)
            .mount(&server)
            .await;

        let backend = backend(&server.uri()).await;
        let updated = backend
            .update_cached_content(
                &cache_name,
                CacheExpirationRequest::Ttl { ttl: "600s".to_string() },
            )
            .await
            .expect("update should succeed");
        assert_eq!(updated.name, cache_name);
    }

    #[tokio::test]
    async fn update_cached_content_patches_expire_time_with_update_mask() {
        let server = MockServer::start().await;
        let cache_name = format!("{PARENT}/cachedContents/cache-123");
        Mock::given(method("PATCH"))
            .and(path(format!("/v1/{cache_name}")))
            .and(query_param("updateMask", "expireTime"))
            .and(body_partial_json(json!({ "expireTime": "2026-01-01T01:00:00Z" })))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(cached_content_json(&cache_name)),
            )
            .expect(1)
            .mount(&server)
            .await;

        let expire_time = time::OffsetDateTime::parse(
            "2026-01-01T01:00:00Z",
            &time::format_description::well_known::Rfc3339,
        )
        .unwrap();
        let backend = backend(&server.uri()).await;
        let updated = backend
            .update_cached_content(&cache_name, CacheExpirationRequest::ExpireTime { expire_time })
            .await
            .expect("update should succeed");
        assert_eq!(updated.name, cache_name);
    }

    #[tokio::test]
    async fn delete_cached_content_issues_delete() {
        let server = MockServer::start().await;
        let cache_name = format!("{PARENT}/cachedContents/cache-123");
        Mock::given(method("DELETE"))
            .and(path(format!("/v1/{cache_name}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let backend = backend(&server.uri()).await;
        backend.delete_cached_content("cache-123").await.expect("delete should succeed");
    }

    #[tokio::test]
    async fn cache_ops_require_parent_in_model_path() {
        // A backend constructed with a bare model name has no project/location
        // to derive the cachedContents parent from.
        let backend = backend_with_model("https://example.invalid", Model::Gemini25Flash).await;
        let err =
            backend.get_cached_content("cache-123").await.expect_err("bare model path should fail");
        assert!(
            matches!(err, Error::Validation { ref message } if message.contains("models/gemini-2.5-flash"))
        );
    }
}
