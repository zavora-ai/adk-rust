//! Azure Blob Storage, including the `$web` static website container (spec 3.5 §7.8 "Hosts",
//! feature `host-azure`).
//!
//! [`AzureBlobStore`] is an [`ObjectStore`]; put it under an
//! [`ObjectStoreHost`](super::object::ObjectStoreHost):
//!
//! | Layout | Serve with |
//! |--------|-----------|
//! | [`ObjectStoreHost::in_place`](super::object::ObjectStoreHost::in_place) | The account's static website (the `$web` container, [`AzureBlobStore::static_website`]); a private container keeps the releases |
//! | [`ObjectStoreHost::prefix_pointer`](super::object::ObjectStoreHost::prefix_pointer) | An edge that reads `current.pointer` and rewrites `/path` to `<pointer>/path` |
//!
//! The static website endpoint has no atomic switch, so the in-place layout declares
//! [`Atomicity::None`](awp_types::Atomicity::None). Requests are authorised per
//! [`AzureAuth`].

use std::fmt;

use adk_core::Url;
use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use bytes::Bytes;
use chrono::Utc;
use hmac::{Hmac, Mac};
use reqwest::Method;
use sha2::Sha256;

use super::HostCredential;
use super::http::{ATTEMPTS, backoff, failure, retryable, uri_encode};
use super::object::{ObjectMeta, ObjectStore};
use crate::error::{Result, SiteError};

/// The REST API version requests name.
pub const API_VERSION: &str = "2021-12-02";

/// How requests to the storage account are authorised; the [`HostCredential`]'s secret
/// holds the matching value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AzureAuth {
    /// Shared Key: the secret is the base64 account key, and each request is signed with it.
    SharedKey,
    /// Microsoft Entra ID: the secret is an OAuth 2.0 access token for
    /// `https://storage.azure.com/` with the Storage Blob Data Contributor role.
    Bearer,
    /// A shared access signature: the secret is its query string, appended to each URL.
    Sas,
}

/// An Azure Blob Storage container as an [`ObjectStore`] (spec 3.5 §7.8 "Hosts").
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use adk_site::host::azure::{AzureAuth, AzureBlobStore};
/// use adk_site::host::object::ObjectStoreHost;
///
/// # fn demo(credential: adk_site::host::HostCredential) {
/// let host = ObjectStoreHost::in_place(
///     Arc::new(AzureBlobStore::static_website("examplesite", AzureAuth::SharedKey, credential.clone())),
///     Arc::new(AzureBlobStore::new("examplesite", "site-releases", AzureAuth::SharedKey, credential)),
/// );
/// # let _ = host;
/// # }
/// ```
#[derive(Clone)]
pub struct AzureBlobStore {
    name: String,
    client: reqwest::Client,
    account: String,
    endpoint: Url,
    container: String,
    prefix: String,
    auth: AzureAuth,
    credential: HostCredential,
}

impl fmt::Debug for AzureBlobStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AzureBlobStore")
            .field("account", &self.account)
            .field("container", &self.container)
            .field("prefix", &self.prefix)
            .field("auth", &self.auth)
            .finish_non_exhaustive()
    }
}

impl AzureBlobStore {
    /// The container `container` of storage account `account`.
    pub fn new(
        account: impl Into<String>,
        container: impl Into<String>,
        auth: AzureAuth,
        credential: HostCredential,
    ) -> Self {
        let account = account.into();
        let endpoint = Url::parse(&format!("https://{account}.blob.core.windows.net"))
            .unwrap_or_else(|_| Url::parse("https://blob.core.windows.net").expect("static URL"));
        Self {
            name: "azure".into(),
            client: reqwest::Client::new(),
            account,
            endpoint,
            container: container.into(),
            prefix: String::new(),
            auth,
            credential,
        }
    }

    /// The account's static website container, `$web`.
    pub fn static_website(
        account: impl Into<String>,
        auth: AzureAuth,
        credential: HostCredential,
    ) -> Self {
        Self::new(account, "$web", auth, credential)
    }

    /// Sends requests to `endpoint` (Azurite: `http://127.0.0.1:10000/<account>`) instead
    /// of `https://<account>.blob.core.windows.net`.
    #[must_use]
    pub fn with_endpoint(mut self, endpoint: Url) -> Self {
        self.endpoint = endpoint;
        self
    }

    /// Keeps every blob under `prefix/` in the container.
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

    /// The URL of the container: what the credential's resource must cover.
    pub fn container_url(&self) -> Url {
        let mut url = self.endpoint.clone();
        url.set_path(&format!(
            "{}/{}",
            url.path().trim_end_matches('/'),
            uri_encode(&self.container, false)
        ));
        url
    }

    fn blob_url(&self, key: &str) -> Result<Url> {
        let url = format!(
            "{}/{}",
            self.container_url().as_str().trim_end_matches('/'),
            uri_encode(&format!("{}{key}", self.prefix), true)
        );
        Url::parse(&url).map_err(|e| SiteError::host(&self.name, format!("bad blob URL: {e}")))
    }

    /// The Shared Key signature of a request (Blob service, version 2009-09-19 and later).
    fn shared_key(
        &self,
        key: &str,
        method: &Method,
        url: &Url,
        headers: &[(String, String)],
        content_length: usize,
    ) -> Result<String> {
        let header = |name: &str| {
            headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map_or("", |(_, v)| v.as_str())
        };
        let mut canonical: Vec<(String, String)> = headers
            .iter()
            .filter(|(k, _)| k.to_ascii_lowercase().starts_with("x-ms-"))
            .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_string()))
            .collect();
        canonical.sort();
        let canonical_headers: String =
            canonical.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
        let mut resource = format!("/{}{}", self.account, url.path());
        let mut query: Vec<(String, String)> =
            url.query_pairs().map(|(k, v)| (k.to_ascii_lowercase(), v.into_owned())).collect();
        query.sort();
        for (name, value) in query {
            resource.push_str(&format!("\n{name}:{value}"));
        }
        let length = if content_length == 0 { String::new() } else { content_length.to_string() };
        let string_to_sign = format!(
            "{method}\n\n\n{length}\n\n{}\n\n\n\n\n\n\n{canonical_headers}{resource}",
            header("content-type")
        );
        let key = STANDARD.decode(key.trim()).map_err(|_| {
            SiteError::host(&self.name, "the Shared Key credential is not a base64 account key")
        })?;
        let mut mac = Hmac::<Sha256>::new_from_slice(&key).expect("HMAC accepts any key length");
        mac.update(string_to_sign.as_bytes());
        Ok(format!("SharedKey {}:{}", self.account, STANDARD.encode(mac.finalize().into_bytes())))
    }

    async fn send(
        &self,
        method: Method,
        url: Url,
        body: Bytes,
        extra: &[(&str, String)],
    ) -> Result<reqwest::Response> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            let secret = self.credential.secret_for(&self.name, &url).await?;
            let mut headers: Vec<(String, String)> = extra
                .iter()
                .map(|(k, v)| ((*k).to_string(), v.clone()))
                .chain([
                    (
                        "x-ms-date".to_string(),
                        Utc::now().format("%a, %d %b %Y %H:%M:%S GMT").to_string(),
                    ),
                    ("x-ms-version".to_string(), API_VERSION.to_string()),
                ])
                .collect();
            let mut target = url.clone();
            match self.auth {
                AzureAuth::SharedKey => {
                    let signature =
                        self.shared_key(secret.expose(), &method, &url, &headers, body.len())?;
                    headers.push(("authorization".into(), signature));
                }
                AzureAuth::Bearer => {
                    headers.push(("authorization".into(), format!("Bearer {}", secret.expose())))
                }
                AzureAuth::Sas => target.set_query(Some(secret.expose().trim_start_matches('?'))),
            }
            let mut request = self.client.request(method.clone(), target).body(body.clone());
            for (name, value) in &headers {
                request = request.header(name.as_str(), value.as_str());
            }
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

    async fn copy_status(&self, url: &Url) -> Result<String> {
        let response = self.send(Method::HEAD, url.clone(), Bytes::new(), &[]).await?;
        if !response.status().is_success() {
            return Err(SiteError::host(&self.name, format!("copy status: {}", response.status())));
        }
        Ok(response
            .headers()
            .get("x-ms-copy-status")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("success")
            .to_string())
    }
}

#[async_trait]
impl ObjectStore for AzureBlobStore {
    fn name(&self) -> &str {
        &self.name
    }

    async fn put(&self, key: &str, body: Bytes, meta: &ObjectMeta) -> Result<()> {
        let mut headers = vec![
            ("content-type", meta.content_type.clone()),
            ("x-ms-blob-type", "BlockBlob".to_string()),
        ];
        if let Some(cache) = &meta.cache_control {
            headers.push(("x-ms-blob-cache-control", cache.clone()));
        }
        let response = self.send(Method::PUT, self.blob_url(key)?, body, &headers).await?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(SiteError::host(&self.name, format!("PUT {key}: {}", failure(response).await)))
        }
    }

    async fn get(&self, key: &str) -> Result<Option<Bytes>> {
        let response = self.send(Method::GET, self.blob_url(key)?, Bytes::new(), &[]).await?;
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
        let mut source = self.blob_url(from)?;
        if self.auth == AzureAuth::Sas {
            let secret = self.credential.secret_for(&self.name, &source).await?;
            source.set_query(Some(secret.expose().trim_start_matches('?')));
        }
        let target = self.blob_url(to)?;
        let response = self
            .send(
                Method::PUT,
                target.clone(),
                Bytes::new(),
                &[("x-ms-copy-source", source.to_string())],
            )
            .await?;
        if !response.status().is_success() {
            return Err(SiteError::host(
                &self.name,
                format!("copy {from} to {to}: {}", failure(response).await),
            ));
        }
        let mut status = response
            .headers()
            .get("x-ms-copy-status")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("success")
            .to_string();
        let mut polls = 0;
        while status == "pending" && polls < 120 {
            polls += 1;
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            status = self.copy_status(&target).await?;
        }
        if status == "success" {
            Ok(())
        } else {
            Err(SiteError::host(&self.name, format!("copy {from} to {to} ended {status}")))
        }
    }

    async fn delete(&self, key: &str) -> Result<()> {
        let response = self.send(Method::DELETE, self.blob_url(key)?, Bytes::new(), &[]).await?;
        if response.status().is_success() || response.status() == reqwest::StatusCode::NOT_FOUND {
            Ok(())
        } else {
            Err(SiteError::host(&self.name, format!("DELETE {key}: {}", failure(response).await)))
        }
    }
}
