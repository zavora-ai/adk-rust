//! Google Cloud Storage through its JSON API (spec 3.5 §7.8 "Hosts", feature `host-gcs`).
//!
//! [`GcsStore`] is an [`ObjectStore`]; put it under an
//! [`ObjectStoreHost`](super::object::ObjectStoreHost):
//!
//! | Layout | Serve with |
//! |--------|-----------|
//! | [`ObjectStoreHost::in_place`](super::object::ObjectStoreHost::in_place) | A website bucket (`MainPageSuffix` `index.html`), directly or behind Cloud CDN; a second, private bucket keeps the releases |
//! | [`ObjectStoreHost::prefix_pointer`](super::object::ObjectStoreHost::prefix_pointer) | An edge that reads `current.pointer` and rewrites `/path` to `<pointer>/path` |
//!
//! The credential is an OAuth 2.0 access token with the `devstorage.read_write` scope for
//! the resource `https://storage.googleapis.com/`; bind the broker to a source that
//! refreshes it (a service account, workload identity, or application default
//! credentials). Publicly readable objects without a `Cache-Control` are cached by Google
//! for an hour, so set one with
//! [`with_cache_control`](super::object::ObjectStoreHost::with_cache_control) for the
//! in-place layout; pointers are always written uncached.

use adk_core::Url;
use async_trait::async_trait;
use bytes::Bytes;
use reqwest::Method;
use serde::Deserialize;

use super::HostCredential;
use super::http::{ATTEMPTS, backoff, failure, retryable, uri_encode};
use super::object::{ObjectMeta, ObjectStore};
use crate::error::{Result, SiteError};

/// A Google Cloud Storage bucket as an [`ObjectStore`] (spec 3.5 §7.8 "Hosts").
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use adk_site::host::gcs::GcsStore;
/// use adk_site::host::object::ObjectStoreHost;
///
/// # fn demo(credential: adk_site::host::HostCredential) {
/// let host = ObjectStoreHost::in_place(
///     Arc::new(GcsStore::new("www.example.com", credential.clone())),
///     Arc::new(GcsStore::new("example-site-releases", credential)),
/// )
/// .with_cache_control("public, max-age=300");
/// # let _ = host;
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct GcsStore {
    name: String,
    client: reqwest::Client,
    endpoint: Url,
    bucket: String,
    prefix: String,
    credential: HostCredential,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Rewrite {
    done: bool,
    #[serde(default)]
    rewrite_token: Option<String>,
}

impl GcsStore {
    /// The bucket `bucket`.
    pub fn new(bucket: impl Into<String>, credential: HostCredential) -> Self {
        Self {
            name: "gcs".into(),
            client: reqwest::Client::new(),
            endpoint: Url::parse("https://storage.googleapis.com").expect("static URL"),
            bucket: bucket.into(),
            prefix: String::new(),
            credential,
        }
    }

    /// Sends requests to `endpoint` (an emulator) instead of `https://storage.googleapis.com`.
    #[must_use]
    pub fn with_endpoint(mut self, endpoint: Url) -> Self {
        self.endpoint = endpoint;
        self
    }

    /// Keeps every object under `prefix/` in the bucket.
    #[must_use]
    pub fn with_prefix(mut self, prefix: &str) -> Self {
        let prefix = prefix.trim_matches('/');
        self.prefix = if prefix.is_empty() { String::new() } else { format!("{prefix}/") };
        self
    }

    /// Names the store in reports.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Sends requests through `client`.
    #[must_use]
    pub fn with_client(mut self, client: reqwest::Client) -> Self {
        self.client = client;
        self
    }

    fn url(&self, path: &str) -> Result<Url> {
        Url::parse(&format!("{}{path}", self.endpoint.as_str().trim_end_matches('/')))
            .map_err(|e| SiteError::host(&self.name, format!("bad URL: {e}")))
    }

    fn object(&self, key: &str) -> String {
        format!(
            "/storage/v1/b/{}/o/{}",
            uri_encode(&self.bucket, false),
            uri_encode(&format!("{}{key}", self.prefix), false)
        )
    }

    async fn send(
        &self,
        method: Method,
        url: Url,
        body: Option<(Bytes, String)>,
    ) -> Result<reqwest::Response> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            let token = self.credential.secret_for(&self.name, &url).await?;
            let mut request =
                self.client.request(method.clone(), url.clone()).bearer_auth(token.expose());
            request = match &body {
                Some((bytes, content_type)) => {
                    request.header("content-type", content_type.as_str()).body(bytes.clone())
                }
                None => request.header("content-length", "0"),
            };
            let result = request.send().await;
            if attempt < ATTEMPTS && retryable(&result) {
                tokio::time::sleep(backoff(attempt)).await;
                continue;
            }
            return result.map_err(|e| {
                SiteError::host(&self.name, format!("{method} {}: {}", url.path(), e.without_url()))
            });
        }
    }
}

#[async_trait]
impl ObjectStore for GcsStore {
    fn name(&self) -> &str {
        &self.name
    }

    async fn put(&self, key: &str, body: Bytes, meta: &ObjectMeta) -> Result<()> {
        let url = self.url(&format!(
            "/upload/storage/v1/b/{}/o?uploadType=multipart",
            uri_encode(&self.bucket, false)
        ))?;
        let mut resource = serde_json::json!({
            "name": format!("{}{key}", self.prefix),
            "contentType": meta.content_type,
        });
        if let Some(cache) = &meta.cache_control {
            resource["cacheControl"] = cache.clone().into();
        }
        let boundary = format!(
            "adk-site-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        );
        let mut multipart = Vec::with_capacity(body.len() + 512);
        multipart.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{resource}\r\n\
                 --{boundary}\r\nContent-Type: {}\r\n\r\n",
                meta.content_type
            )
            .as_bytes(),
        );
        multipart.extend_from_slice(&body);
        multipart.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let content_type = format!("multipart/related; boundary={boundary}");
        let response = self.send(Method::POST, url, Some((multipart.into(), content_type))).await?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(SiteError::host(&self.name, format!("upload {key}: {}", failure(response).await)))
        }
    }

    async fn get(&self, key: &str) -> Result<Option<Bytes>> {
        let url = self.url(&format!("{}?alt=media", self.object(key)))?;
        let response = self.send(Method::GET, url, None).await?;
        match response.status() {
            status if status.is_success() => response.bytes().await.map(Some).map_err(|e| {
                SiteError::host(&self.name, format!("GET {key}: {}", e.without_url()))
            }),
            reqwest::StatusCode::NOT_FOUND => Ok(None),
            _ => {
                Err(SiteError::host(&self.name, format!("GET {key}: {}", failure(response).await)))
            }
        }
    }

    async fn copy(&self, from: &str, to: &str) -> Result<()> {
        let mut token: Option<String> = None;
        loop {
            let mut path = format!(
                "{}/rewriteTo/b/{}/o/{}",
                self.object(from),
                uri_encode(&self.bucket, false),
                uri_encode(&format!("{}{to}", self.prefix), false)
            );
            if let Some(token) = &token {
                path.push_str(&format!("?rewriteToken={}", uri_encode(token, false)));
            }
            let response = self.send(Method::POST, self.url(&path)?, None).await?;
            if !response.status().is_success() {
                return Err(SiteError::host(
                    &self.name,
                    format!("copy {from} to {to}: {}", failure(response).await),
                ));
            }
            let rewrite: Rewrite = response.json().await.map_err(|e| {
                SiteError::host(&self.name, format!("copy {from} to {to}: {}", e.without_url()))
            })?;
            if rewrite.done {
                return Ok(());
            }
            token = Some(rewrite.rewrite_token.ok_or_else(|| {
                SiteError::host(&self.name, format!("copy {from} to {to}: no rewrite token"))
            })?);
        }
    }

    async fn delete(&self, key: &str) -> Result<()> {
        let response = self.send(Method::DELETE, self.url(&self.object(key))?, None).await?;
        if response.status().is_success() || response.status() == reqwest::StatusCode::NOT_FOUND {
            Ok(())
        } else {
            Err(SiteError::host(&self.name, format!("DELETE {key}: {}", failure(response).await)))
        }
    }
}
