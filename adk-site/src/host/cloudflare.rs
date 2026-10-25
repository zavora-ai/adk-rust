//! Cloudflare: releases in an R2 bucket behind a Worker that follows the pointer, and cache
//! purge through the Cloudflare API (spec 3.5 §7.8 "Hosts", §7.8.3 `PrefixPointer`, Q16;
//! feature `host-cloudflare`).
//!
//! R2 speaks the S3 API, so the store is an [`S3Store`] at
//! `https://<account>.r2.cloudflarestorage.com` in region `auto`, under an
//! [`ObjectStoreHost::prefix_pointer`]. The Worker bound to the bucket as `SITE` serves the
//! release the pointer names:
//!
//! ```js
//! let pointer = { dir: null, at: 0 };
//! export default {
//!   async fetch(request, env) {
//!     if (Date.now() - pointer.at > 5000) {
//!       const object = await env.SITE.get("current.pointer");
//!       pointer = { dir: object ? (await object.text()).trim() : null, at: Date.now() };
//!     }
//!     if (!pointer.dir) return new Response("Not found", { status: 404 });
//!     let key = new URL(request.url).pathname.slice(1);
//!     if (key === "" || key.endsWith("/")) key += "index.html";
//!     const object = await env.SITE.get(`${pointer.dir}/${key}`);
//!     if (!object) return new Response("Not found", { status: 404 });
//!     const headers = new Headers();
//!     object.writeHttpMetadata(headers);
//!     return new Response(object.body, { headers });
//!   },
//! };
//! ```
//!
//! [`CloudflarePurge`] purges the zone's cached copies of the changed URLs after each switch.
//! The R2 credential is an R2 API token's S3 keys as JSON (see [`HostCredential`]); the
//! purge credential is an API token with the zone's Cache Purge permission.

use std::sync::Arc;

use adk_core::Url;
use async_trait::async_trait;
use serde::Deserialize;

use super::HostCredential;
use super::http::failure;
use super::object::{CachePurge, ObjectStoreHost};
use super::s3::S3Store;
use crate::error::{Result, SiteError};

const NAME: &str = "cloudflare";

/// The R2 bucket `bucket` of account `account_id` as an [`S3Store`].
///
/// # Example
///
/// ```rust
/// use adk_site::host::cloudflare::r2_store;
/// # fn demo(credential: adk_site::host::HostCredential) {
/// let store = r2_store("0123abcd", "example-site", credential);
/// assert_eq!(store.bucket_url().as_str(), "https://0123abcd.r2.cloudflarestorage.com/example-site");
/// # }
/// ```
pub fn r2_store(account_id: &str, bucket: &str, credential: HostCredential) -> S3Store {
    let endpoint = Url::parse(&format!("https://{account_id}.r2.cloudflarestorage.com"))
        .unwrap_or_else(|_| Url::parse("https://r2.cloudflarestorage.com").expect("static URL"));
    S3Store::new(endpoint, "auto", bucket, credential).path_style(true).with_name("r2")
}

/// Releases in R2 behind a pointer-following Worker, purging `purge`'s zone after each
/// switch ([`Atomicity::PrefixPointer`](awp_types::Atomicity::PrefixPointer)).
///
/// # Example
///
/// ```rust
/// use adk_site::host::SiteHost;
/// use adk_site::host::cloudflare::{CloudflarePurge, r2_host};
/// # fn demo(r2: adk_site::host::HostCredential, api: adk_site::host::HostCredential) {
/// let purge = CloudflarePurge::new("zone-id", "https://example.com/".parse().unwrap(), api);
/// let host = r2_host("0123abcd", "example-site", r2, Some(purge));
/// assert_eq!(host.name(), "cloudflare");
/// # }
/// ```
pub fn r2_host(
    account_id: &str,
    bucket: &str,
    credential: HostCredential,
    purge: Option<CloudflarePurge>,
) -> ObjectStoreHost {
    let host = ObjectStoreHost::prefix_pointer(Arc::new(r2_store(account_id, bucket, credential)))
        .with_name(NAME);
    match purge {
        Some(purge) => host.with_cache_purge(Arc::new(purge)),
        None => host,
    }
}

/// Purges a Cloudflare zone's cache of published URLs (spec 3.5 §7.8.2 step 6).
///
/// Each path is purged as `site` joined with it; `/a/index.html` also purges `/a/`. URLs go
/// in batches of [`batch`](Self::with_batch) (30 by default, the API's per-request limit on
/// most plans); above [`max_urls`](Self::with_max_urls) (300 by default) the whole zone is
/// purged instead.
#[derive(Debug, Clone)]
pub struct CloudflarePurge {
    client: reqwest::Client,
    api: Url,
    zone_id: String,
    site: Url,
    credential: HostCredential,
    batch: usize,
    max_urls: usize,
}

#[derive(Deserialize)]
struct ApiResponse {
    success: bool,
    #[serde(default)]
    errors: Vec<serde_json::Value>,
}

impl CloudflarePurge {
    /// Purges zone `zone_id`, whose site is served at `site`.
    pub fn new(zone_id: impl Into<String>, site: Url, credential: HostCredential) -> Self {
        Self {
            client: reqwest::Client::new(),
            api: Url::parse("https://api.cloudflare.com/client/v4/").expect("static URL"),
            zone_id: zone_id.into(),
            site,
            credential,
            batch: 30,
            max_urls: 300,
        }
    }

    /// Sends requests to `api` instead of `https://api.cloudflare.com/client/v4/`.
    #[must_use]
    pub fn with_api(mut self, api: Url) -> Self {
        self.api = api;
        self
    }

    /// URLs per purge request.
    #[must_use]
    pub fn with_batch(mut self, urls: usize) -> Self {
        self.batch = urls.max(1);
        self
    }

    /// Purges everything above `urls` URLs.
    #[must_use]
    pub fn with_max_urls(mut self, urls: usize) -> Self {
        self.max_urls = urls.max(1);
        self
    }

    async fn send(&self, body: serde_json::Value) -> Result<()> {
        let url = self
            .api
            .join(&format!("zones/{}/purge_cache", self.zone_id))
            .map_err(|e| SiteError::host(NAME, format!("bad purge URL: {e}")))?;
        let token = self.credential.secret_for(NAME, &url).await?;
        let response = self
            .client
            .post(url)
            .bearer_auth(token.expose())
            .json(&body)
            .send()
            .await
            .map_err(|e| SiteError::host(NAME, e.without_url().to_string()))?;
        if !response.status().is_success() {
            return Err(SiteError::host(NAME, format!("purge: {}", failure(response).await)));
        }
        let answer: ApiResponse = response
            .json()
            .await
            .map_err(|e| SiteError::host(NAME, format!("purge answer: {}", e.without_url())))?;
        if answer.success {
            Ok(())
        } else {
            Err(SiteError::host(NAME, format!("purge refused: {:?}", answer.errors)))
        }
    }
}

#[async_trait]
impl CachePurge for CloudflarePurge {
    async fn purge(&self, paths: &[String]) -> Result<()> {
        let mut urls: Vec<String> = Vec::new();
        for path in paths {
            let relative = path.trim_start_matches('/');
            for candidate in
                [Some(relative), relative.strip_suffix("index.html")].into_iter().flatten()
            {
                let url = self
                    .site
                    .join(candidate)
                    .map_err(|e| SiteError::host(NAME, format!("bad URL for '{path}': {e}")))?;
                urls.push(url.to_string());
            }
        }
        urls.sort();
        urls.dedup();
        if urls.len() > self.max_urls {
            self.send(serde_json::json!({ "purge_everything": true })).await?;
        } else {
            for chunk in urls.chunks(self.batch) {
                self.send(serde_json::json!({ "files": chunk })).await?;
            }
        }
        tracing::info!(cloudflare.zone = %self.zone_id, urls = urls.len(), "cache purged");
        Ok(())
    }
}
