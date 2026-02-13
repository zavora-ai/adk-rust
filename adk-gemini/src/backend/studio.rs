use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures::TryStreamExt;
use jsonwebtoken::{EncodingKey, Header};
use mime::Mime;
use reqwest::{
    Client, Response,
    header::{HeaderMap, HeaderName, HeaderValue},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use snafu::ResultExt;
use std::sync::{Arc, LazyLock};
use tokio::sync::Mutex;
use url::Url;

use crate::backend::{BackendStream, GeminiBackend};
use crate::batch::model::{BatchGenerateContentRequest, BatchOperation, ListBatchesResponse};
use crate::cache::model::{
    CacheExpirationRequest, CachedContent, CreateCachedContentRequest, ListCachedContentsResponse,
};
use crate::common::Model;
use crate::embedding::{
    BatchContentEmbeddingResponse, BatchEmbedContentsRequest, ContentEmbeddingResponse,
    EmbedContentRequest,
};
use crate::error::{
    BadResponseSnafu, ConstructUrlSnafu, DecodeResponseSnafu, Error, InvalidApiKeySnafu,
    PerformRequestNewSnafu, ServiceAccountJwtSnafu, UrlParseSnafu,
};
use crate::files::model::{File, ListFilesResponse};
use crate::generation::model::{GenerateContentRequest, GenerationResponse};

static DEFAULT_BASE_URL: LazyLock<Url> = LazyLock::new(|| {
    Url::parse("https://generativelanguage.googleapis.com/v1beta/")
        .expect("unreachable error: failed to parse default base URL")
});

#[derive(Debug, Clone)]
pub enum AuthConfig {
    ApiKey(String),
    ServiceAccount(ServiceAccountTokenSource),
}

#[derive(Debug, Deserialize, Clone)]
pub struct ServiceAccountKey {
    pub client_email: String,
    pub private_key: String,
    pub token_uri: String,
}

#[derive(Debug, Clone)]
struct CachedToken {
    access_token: String,
    expires_at: i64,
}

#[derive(Debug, Clone)]
pub struct ServiceAccountTokenSource {
    key: ServiceAccountKey,
    scopes: Vec<String>,
    cached: Arc<Mutex<Option<CachedToken>>>,
}

impl ServiceAccountTokenSource {
    pub fn new(key: ServiceAccountKey) -> Self {
        Self {
            key,
            scopes: vec!["https://www.googleapis.com/auth/cloud-platform".to_string()],
            cached: Arc::new(Mutex::new(None)),
        }
    }

    async fn access_token(&self, http_client: &Client) -> Result<String, Error> {
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        {
            let cache = self.cached.lock().await;
            if let Some(token) = cache.as_ref().filter(|t| t.expires_at.saturating_sub(60) > now) {
                return Ok(token.access_token.clone());
            }
        }

        let jwt = self.build_jwt(now)?;
        let token = self.fetch_token(http_client, jwt).await?;

        let mut cache = self.cached.lock().await;
        *cache = Some(token.clone());
        Ok(token.access_token)
    }

    fn build_jwt(&self, now: i64) -> Result<String, Error> {
        #[derive(Serialize)]
        struct Claims<'a> {
            iss: &'a str,
            scope: &'a str,
            aud: &'a str,
            iat: i64,
            exp: i64,
        }

        let exp = now + 3600;
        let scope = self.scopes.join(" ");
        let claims = Claims {
            iss: &self.key.client_email,
            scope: &scope,
            aud: &self.key.token_uri,
            iat: now,
            exp,
        };
        let encoding_key = EncodingKey::from_rsa_pem(self.key.private_key.as_bytes())
            .context(ServiceAccountJwtSnafu)?;
        jsonwebtoken::encode(&Header::new(jsonwebtoken::Algorithm::RS256), &claims, &encoding_key)
            .context(ServiceAccountJwtSnafu)
    }

    async fn fetch_token(&self, http_client: &Client, jwt: String) -> Result<CachedToken, Error> {
        #[derive(Deserialize)]
        struct TokenResponse {
            access_token: String,
            expires_in: i64,
        }

        let url = &self.key.token_uri;
        let response = http_client
            .post(url)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                ("assertion", &jwt),
            ])
            .send()
            .await
            .map_err(|e| Error::ServiceAccountToken { source: e, url: url.clone() })?;

        let response = check_response(response).await?;
        let token: TokenResponse = response.json().await.context(DecodeResponseSnafu)?;
        let expires_at = time::OffsetDateTime::now_utc().unix_timestamp() + token.expires_in;
        Ok(CachedToken { access_token: token.access_token, expires_at })
    }
}

async fn check_response(response: Response) -> Result<Response, Error> {
    let status = response.status();
    if !status.is_success() {
        let description = response.text().await.ok();
        BadResponseSnafu { code: status.as_u16(), description }.fail()
    } else {
        Ok(response)
    }
}

#[derive(Debug)]
pub struct StudioBackend {
    pub http_client: Client,
    pub base_url: Url,
    pub model: Model,
    pub auth: AuthConfig,
}

impl StudioBackend {
    pub fn new(api_key: String, base_url: Option<Url>, model: Model) -> Result<Self, Error> {
        let base_url = base_url.unwrap_or_else(|| DEFAULT_BASE_URL.clone());
        let headers = HeaderMap::from_iter([(
            HeaderName::from_static("x-goog-api-key"),
            HeaderValue::from_str(&api_key).context(InvalidApiKeySnafu)?,
        )]);

        let http_client =
            Client::builder().default_headers(headers).build().context(PerformRequestNewSnafu)?;

        Ok(Self { http_client, base_url, model, auth: AuthConfig::ApiKey(api_key) })
    }

    pub fn new_with_client(
        http_client: Client,
        base_url: Url,
        model: Model,
        auth: AuthConfig,
    ) -> Self {
        Self { http_client, base_url, model, auth }
    }

    async fn perform_request<B, D, F, T>(
        &self,
        build_request: B,
        deserialize: D,
    ) -> Result<T, Error>
    where
        B: FnOnce(&Client) -> reqwest::RequestBuilder,
        D: FnOnce(Response) -> F,
        F: std::future::Future<Output = Result<T, Error>>,
    {
        let mut request = build_request(&self.http_client);

        if let AuthConfig::ServiceAccount(sa) = &self.auth {
            let token = sa.access_token(&self.http_client).await?;
            request = request.bearer_auth(token);
        }

        let response = request.send().await.map_err(|e| Error::PerformRequestNew { source: e })?;
        let response = check_response(response).await?;
        deserialize(response).await
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, url: Url) -> Result<T, Error> {
        self.perform_request(
            |c| c.get(url),
            |r| async move { r.json().await.context(DecodeResponseSnafu) },
        )
        .await
    }

    async fn post_json<T: serde::de::DeserializeOwned, B: Serialize + ?Sized>(
        &self,
        url: Url,
        body: &B,
    ) -> Result<T, Error> {
        self.perform_request(
            |c| c.post(url).json(body),
            |r| async move { r.json().await.context(DecodeResponseSnafu) },
        )
        .await
    }

    fn build_url_with_suffix(&self, suffix: &str) -> Result<Url, Error> {
        self.base_url.join(suffix).context(ConstructUrlSnafu { suffix: suffix.to_string() })
    }

    fn build_url(&self, endpoint: &str) -> Result<Url, Error> {
        let suffix =
            format!("models/{}:{}", self.model.as_str().trim_start_matches("models/"), endpoint);
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
                    format!("cachedContents/{}", n)
                }
            })
            .unwrap_or_else(|| "cachedContents".to_string());
        self.build_url_with_suffix(&suffix)
    }

    async fn create_upload(
        &self,
        size: usize,
        display_name: Option<String>,
        mime_type: Mime,
    ) -> Result<Url, Error> {
        let url = self
            .base_url
            .join("upload/v1beta/files")
            .context(ConstructUrlSnafu { suffix: "upload/v1beta/files".to_string() })?;

        let mut headers = HeaderMap::new();
        headers.insert("X-Goog-Upload-Protocol", HeaderValue::from_static("resumable"));
        headers.insert("X-Goog-Upload-Command", HeaderValue::from_static("start"));
        headers.insert("X-Goog-Upload-Header-Content-Length", HeaderValue::from(size));
        headers.insert(
            "X-Goog-Upload-Header-Content-Type",
            HeaderValue::from_str(mime_type.as_ref()).unwrap(),
        );

        #[derive(Serialize)]
        struct Metadata {
            file: FileMetadata,
        }
        #[derive(Serialize)]
        struct FileMetadata {
            display_name: Option<String>,
        }

        let metadata = Metadata { file: FileMetadata { display_name } };

        let response = self
            .perform_request(
                |c| c.post(url).headers(headers).json(&metadata),
                |r| async move { Ok(r) },
            )
            .await?;

        let upload_url = response
            .headers()
            .get("X-Goog-Upload-URL")
            .ok_or_else(|| Error::MissingResponseHeader {
                header: "X-Goog-Upload-URL".to_string(),
            })?
            .to_str()
            .map_err(|_| Error::MissingResponseHeader {
                header: "X-Goog-Upload-URL (invalid utf8)".to_string(),
            })?;

        Url::parse(upload_url).context(UrlParseSnafu)
    }
}

#[async_trait]
impl GeminiBackend for StudioBackend {
    fn model(&self) -> &str {
        self.model.as_str()
    }

    async fn generate_content(
        &self,
        req: GenerateContentRequest,
    ) -> Result<GenerationResponse, Error> {
        let url = self.build_url("generateContent")?;
        self.post_json(url, &req).await
    }

    async fn generate_content_stream(
        &self,
        req: GenerateContentRequest,
    ) -> Result<BackendStream<GenerationResponse>, Error> {
        let mut url = self.build_url("streamGenerateContent")?;
        url.query_pairs_mut().append_pair("alt", "sse");

        let mut request = self.http_client.post(url).json(&req);
        if let AuthConfig::ServiceAccount(sa) = &self.auth {
            let token = sa.access_token(&self.http_client).await?;
            request = request.bearer_auth(token);
        }

        let response = request.send().await.map_err(|e| Error::PerformRequestNew { source: e })?;
        let response = check_response(response).await?;

        let stream = response.bytes_stream();

        let stream = stream.eventsource().map_err(|e| Error::BadPart { source: e }).and_then(
            |event| async move {
                serde_json::from_str::<GenerationResponse>(&event.data)
                    .map_err(|e| Error::Deserialize { source: e })
            },
        );

        Ok(Box::pin(stream))
    }

    async fn count_tokens(&self, req: GenerateContentRequest) -> Result<u32, Error> {
        let url = self.build_url("countTokens")?;
        #[derive(Deserialize)]
        struct CountTokensResponse {
            #[serde(rename = "totalTokens")]
            total_tokens: u32,
        }
        let response: CountTokensResponse = self.post_json(url, &req).await?;
        Ok(response.total_tokens)
    }

    async fn embed_content(
        &self,
        req: EmbedContentRequest,
    ) -> Result<ContentEmbeddingResponse, Error> {
        let url = self.build_url("embedContent")?;
        self.post_json(url, &req).await
    }

    async fn batch_embed_contents(
        &self,
        req: BatchEmbedContentsRequest,
    ) -> Result<BatchContentEmbeddingResponse, Error> {
        let url = self.build_url("batchEmbedContents")?;
        self.post_json(url, &req).await
    }

    async fn create_cached_content(
        &self,
        req: CreateCachedContentRequest,
    ) -> Result<CachedContent, Error> {
        let url = self.build_cache_url(None)?;
        self.post_json(url, &req).await
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
        // For Studio, update uses PATCH. PR #29 used `patch_json`.
        // I'll use the logic from HEAD but with the unified POST/PATCH helpers if possible.
        // Actually, the HEAD version had a custom update_payload.
        let update_payload = match expiration {
            CacheExpirationRequest::Ttl { ttl } => json!({ "ttl": ttl }),
            CacheExpirationRequest::ExpireTime { expire_time } => {
                json!({ "expireTime": expire_time.format(&time::format_description::well_known::Rfc3339).unwrap() })
            }
        };

        self.perform_request(
            |c| c.patch(url).json(&update_payload),
            |r| async move { r.json().await.context(DecodeResponseSnafu) },
        )
        .await
    }

    async fn delete_cached_content(&self, name: &str) -> Result<(), Error> {
        let url = self.build_cache_url(Some(name))?;
        self.perform_request(|c| c.delete(url), |_| async move { Ok(()) }).await
    }

    async fn create_batch(
        &self,
        req: BatchGenerateContentRequest,
    ) -> Result<BatchOperation, Error> {
        let url = self.build_url("batchGenerateContent")?;
        self.post_json(url, &req).await
    }

    async fn get_batch(&self, name: &str) -> Result<BatchOperation, Error> {
        let suffix =
            if name.contains('/') { name.to_string() } else { format!("batches/{}", name) };
        let url = self.build_url_with_suffix(&suffix)?;
        self.get_json(url).await
    }

    async fn list_batches(
        &self,
        page_size: Option<u32>,
        page_token: Option<String>,
    ) -> Result<ListBatchesResponse, Error> {
        let mut url = self.build_url_with_suffix("batches")?;
        if let Some(size) = page_size {
            url.query_pairs_mut().append_pair("pageSize", &size.to_string());
        }
        if let Some(token) = page_token {
            url.query_pairs_mut().append_pair("pageToken", &token);
        }
        self.get_json(url).await
    }

    async fn cancel_batch(&self, name: &str) -> Result<(), Error> {
        let suffix = format!("{}:cancel", name);
        let url = self.build_url_with_suffix(&suffix)?;
        self.perform_request(|c| c.post(url), |_| async move { Ok(()) }).await
    }

    async fn delete_batch(&self, name: &str) -> Result<(), Error> {
        let url = self.build_url_with_suffix(name)?;
        self.perform_request(|c| c.delete(url), |_| async move { Ok(()) }).await
    }

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

        let upload_response: UploadResponse = self
            .perform_request(
                |c| {
                    c.post(upload_url)
                        .header("X-Goog-Upload-Command", "upload, finalize")
                        .header("X-Goog-Upload-Offset", "0")
                        .body(file_bytes)
                },
                |r| async move { r.json().await.context(DecodeResponseSnafu) },
            )
            .await?;

        Ok(upload_response.file)
    }

    async fn get_file(&self, name: &str) -> Result<File, Error> {
        let url = self.build_files_url(Some(name))?;
        self.get_json(url).await
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
        self.perform_request(|c| c.delete(url), |_| async move { Ok(()) }).await
    }

    async fn download_file(&self, name: &str) -> Result<Vec<u8>, Error> {
        let mut url = self
            .base_url
            .join(&format!("/download/v1beta/{name}:download"))
            .context(ConstructUrlSnafu { suffix: format!("/download/v1beta/{name}:download") })?;
        url.query_pairs_mut().append_pair("alt", "media");

        self.perform_request(
            |c| c.get(url),
            |r| async move { r.bytes().await.context(DecodeResponseSnafu).map(|b| b.to_vec()) },
        )
        .await
    }
}
