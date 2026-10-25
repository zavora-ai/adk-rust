//! Netlify through its deploy API (spec 3.5 §7.8 "Hosts", feature `host-netlify`).
//!
//! A release becomes one Netlify deploy created from file digests: Netlify answers with the
//! digests it does not hold, only those files are uploaded, and the deploy goes live whole
//! when it is ready ([`Atomicity::Deploy`]). Previews are draft deploys. A rollback
//! verifies the earlier deploy's files through its permalink and creates a deploy of them
//! with the log as it stands, which uploads nothing.
//!
//! | API call | Use |
//! |----------|-----|
//! | `GET sites/{site}` | The site URL and the published deploy |
//! | `POST sites/{site}/deploys?title=` | A deploy from `{"files": {path: sha1}, "draft"}`; answers `required` |
//! | `PUT deploys/{deploy}/files/{path}` | Uploads a required file |
//! | `GET deploys/{deploy}` | Waits for `ready` |
//! | `GET deploys/{deploy}/files` | The digests a deploy holds, to carry the log forward |
//! | `GET sites/{site}/deploys` | Finds the deploy of a release by its title, for rollback |
//! | `POST sites/{site}/deploys/{deploy}/restore` | Publishes a ready deploy when auto-publishing is locked |
//!
//! Netlify limits API deploys to 3 a minute and 100 a day, so [`SiteHost::limits`]
//! declares 4 publishes an hour, the sustained rate. The credential is a personal access
//! token or OAuth token for the resource `https://api.netlify.com/api/v1/`.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use adk_core::Url;
use async_trait::async_trait;
use awp_types::{Atomicity, FileEntry, HostLimits, ReleaseManifest};
use bytes::Bytes;
use futures::future::BoxFuture;
use futures::{FutureExt, StreamExt, TryStreamExt};
use reqwest::Method;
use serde::Deserialize;
use sha1::Sha1;
use sha2::Digest as _;

use super::http::{ATTEMPTS, backoff, failure, retryable, uri_encode};
use super::keys::site_key;
use super::{HostCredential, HostTarget, PublishReport, SiteHost};
use crate::error::{Result, SiteError};
use crate::log::CHECKPOINT_PATH;
use crate::redirects::RedirectsFormat;
use crate::release::{
    StagedDelta, StagedRelease, file_digest, is_log_path, manifest_digest, manifest_path,
};

const NAME: &str = "netlify";
const TITLE: &str = "adk-site release";

#[derive(Debug, Deserialize)]
struct Site {
    #[serde(default)]
    ssl_url: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    published_deploy: Option<DeployRef>,
}

#[derive(Debug, Deserialize)]
struct DeployRef {
    id: String,
}

#[derive(Debug, Deserialize)]
struct Deploy {
    id: String,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    required: Option<Vec<String>>,
    #[serde(default)]
    deploy_ssl_url: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    error_message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DeployFile {
    path: String,
    sha: String,
}

/// `url` with a trailing `/`, so joining a relative path appends to it.
fn as_dir(mut url: Url) -> Url {
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    url
}

fn sha1_hex(bytes: &[u8]) -> String {
    hex::encode(Sha1::digest(bytes))
}

/// A Netlify site as a [`SiteHost`] (spec 3.5 §7.8 "Hosts").
///
/// # Example
///
/// ```rust,no_run
/// use adk_site::host::netlify::NetlifyHost;
/// use adk_site::host::SiteHost;
/// use awp_types::Atomicity;
///
/// # fn demo(credential: adk_site::host::HostCredential) {
/// let host = NetlifyHost::new("0f3a-site-id", credential);
/// assert_eq!(host.limits().atomic, Atomicity::Deploy);
/// # }
/// ```
#[derive(Debug)]
pub struct NetlifyHost {
    client: reqwest::Client,
    api: Url,
    site_id: String,
    credential: HostCredential,
    site_url: tokio::sync::RwLock<Option<Url>>,
    concurrency: usize,
    poll: Duration,
    timeout: Duration,
    lock: tokio::sync::Mutex<()>,
}

impl NetlifyHost {
    /// The Netlify site `site_id` (its API id or its `name.netlify.app` domain).
    pub fn new(site_id: impl Into<String>, credential: HostCredential) -> Self {
        Self {
            client: reqwest::Client::new(),
            api: Url::parse("https://api.netlify.com/api/v1/").expect("static URL"),
            site_id: site_id.into(),
            credential,
            site_url: tokio::sync::RwLock::new(None),
            concurrency: 8,
            poll: Duration::from_secs(1),
            timeout: Duration::from_secs(300),
            lock: tokio::sync::Mutex::new(()),
        }
    }

    /// Sends API requests to `api` instead of `https://api.netlify.com/api/v1/`.
    #[must_use]
    pub fn with_api(mut self, api: Url) -> Self {
        self.api = api;
        self
    }

    /// Reads the production site at `url` (a custom domain) instead of the site's
    /// `ssl_url`.
    #[must_use]
    pub fn with_site_url(self, url: Url) -> Self {
        Self { site_url: tokio::sync::RwLock::new(Some(as_dir(url))), ..self }
    }

    /// Uploads at most `uploads` files at once; 8 by default.
    #[must_use]
    pub fn with_concurrency(mut self, uploads: usize) -> Self {
        self.concurrency = uploads.max(1);
        self
    }

    /// Polls a deploy every `interval` until it is ready, for at most `timeout`.
    #[must_use]
    pub fn with_polling(mut self, interval: Duration, timeout: Duration) -> Self {
        self.poll = interval;
        self.timeout = timeout;
        self
    }

    fn url(&self, path: &str) -> Result<Url> {
        self.api.join(path).map_err(|e| SiteError::host(NAME, format!("bad API URL '{path}': {e}")))
    }

    async fn call(
        &self,
        method: Method,
        path: &str,
        body: Option<RequestBody>,
    ) -> Result<reqwest::Response> {
        let url = self.url(path)?;
        let mut attempt = 0;
        loop {
            attempt += 1;
            let token = self.credential.secret_for(NAME, &url).await?;
            let mut request =
                self.client.request(method.clone(), url.clone()).bearer_auth(token.expose());
            request = match &body {
                Some(RequestBody::Json(value)) => request.json(value),
                Some(RequestBody::Bytes(bytes)) => {
                    request.header("content-type", "application/octet-stream").body(bytes.clone())
                }
                None => request,
            };
            let result = request.send().await;
            if attempt < ATTEMPTS && retryable(&result) {
                tokio::time::sleep(backoff(attempt)).await;
                continue;
            }
            let response = result.map_err(|e| {
                SiteError::host(NAME, format!("{method} {}: {}", url.path(), e.without_url()))
            })?;
            if response.status().is_success() {
                return Ok(response);
            }
            return Err(SiteError::host(
                NAME,
                format!("{method} {}: {}", url.path(), failure(response).await),
            ));
        }
    }

    async fn json<T: for<'de> Deserialize<'de>>(
        &self,
        method: Method,
        path: &str,
        body: Option<RequestBody>,
    ) -> Result<T> {
        self.call(method.clone(), path, body)
            .await?
            .json()
            .await
            .map_err(|e| SiteError::host(NAME, format!("{method} {path}: {}", e.without_url())))
    }

    async fn site(&self) -> Result<Site> {
        self.json(Method::GET, &format!("sites/{}", uri_encode(&self.site_id, false)), None).await
    }

    async fn deploy_files(&self, deploy: &str) -> Result<BTreeMap<String, String>> {
        let mut files = BTreeMap::new();
        for page in 1.. {
            let batch: Vec<DeployFile> = self
                .json(
                    Method::GET,
                    &format!(
                        "deploys/{}/files?page={page}&per_page=100",
                        uri_encode(deploy, false)
                    ),
                    None,
                )
                .await?;
            let last = batch.len() < 100;
            files.extend(batch.into_iter().map(|f| (f.path, f.sha)));
            if last {
                break;
            }
        }
        Ok(files)
    }

    async fn production_url(&self, site: Option<&Site>) -> Result<Url> {
        if let Some(url) = self.site_url.read().await.clone() {
            return Ok(url);
        }
        let fetched;
        let site = match site {
            Some(site) => site,
            None => {
                fetched = self.site().await?;
                &fetched
            }
        };
        let url = site
            .ssl_url
            .as_deref()
            .or(site.url.as_deref())
            .ok_or_else(|| SiteError::host(NAME, "the site has no URL yet"))?
            .parse::<Url>()
            .map_err(|e| SiteError::host(NAME, format!("bad site URL: {e}")))?;
        let url = as_dir(url);
        *self.site_url.write().await = Some(url.clone());
        Ok(url)
    }

    /// GETs `path` under `base` (a site or deploy URL); `None` on 404.
    async fn read_public(&self, base: &Url, path: &str) -> Result<Option<Bytes>> {
        let key = site_key(NAME, path)?;
        let request = match key.strip_suffix("index.html") {
            Some(dir) => uri_encode(dir, true),
            None => uri_encode(&key, true),
        };
        let url = base
            .join(&request)
            .map_err(|e| SiteError::host(NAME, format!("bad URL for '{path}': {e}")))?;
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| SiteError::host(NAME, format!("GET {path}: {}", e.without_url())))?;
        match response.status() {
            status if status.is_success() => response
                .bytes()
                .await
                .map(Some)
                .map_err(|e| SiteError::host(NAME, format!("GET {path}: {}", e.without_url()))),
            reqwest::StatusCode::NOT_FOUND => Ok(None),
            _ => Err(SiteError::host(NAME, format!("GET {path}: {}", failure(response).await))),
        }
    }

    async fn wait_ready(&self, id: &str) -> Result<Deploy> {
        let start = Instant::now();
        loop {
            let deploy: Deploy =
                self.json(Method::GET, &format!("deploys/{}", uri_encode(id, false)), None).await?;
            match deploy.state.as_deref() {
                Some("ready" | "current") => return Ok(deploy),
                Some("error" | "rejected") => {
                    return Err(SiteError::host(
                        NAME,
                        format!("deploy {id} failed: {}", deploy.error_message.unwrap_or_default()),
                    ));
                }
                _ if start.elapsed() > self.timeout => {
                    return Err(SiteError::host(
                        NAME,
                        format!("deploy {id} was not ready within {:?}", self.timeout),
                    ));
                }
                _ => tokio::time::sleep(self.poll).await,
            }
        }
    }

    /// Creates a deploy of `files` (path → SHA-1), uploads the required ones in `order`
    /// with bytes from `bytes`, waits for it and publishes it unless it is a draft.
    async fn deploy(
        &self,
        title: &str,
        files: &BTreeMap<String, String>,
        order: &[String],
        bytes: &BTreeMap<String, Bytes>,
        draft: bool,
    ) -> Result<(Deploy, Vec<String>)> {
        let path = format!(
            "sites/{}/deploys?title={}",
            uri_encode(&self.site_id, false),
            uri_encode(title, false)
        );
        let body = serde_json::json!({ "files": files, "draft": draft, "async": false });
        let mut deploy: Deploy =
            self.json(Method::POST, &path, Some(RequestBody::Json(body))).await?;
        if deploy.required.is_none() {
            deploy = self.wait_required(&deploy.id).await?;
        }
        let required: BTreeSet<String> =
            deploy.required.clone().unwrap_or_default().into_iter().collect();
        let mut sent: BTreeSet<&str> = BTreeSet::new();
        let mut uploads: Vec<&String> = Vec::new();
        for path in order {
            let sha = &files[path];
            if required.contains(sha) && sent.insert(sha.as_str()) {
                uploads.push(path);
            }
        }
        let id = deploy.id.clone();
        let groups: [Vec<&String>; 4] = {
            let (mut site, mut manifests, mut tiles, mut checkpoint) =
                (vec![], vec![], vec![], vec![]);
            for path in uploads.iter().copied() {
                let trimmed = path.trim_start_matches('/');
                if !is_log_path(path) {
                    site.push(path);
                } else if trimmed == CHECKPOINT_PATH {
                    checkpoint.push(path);
                } else if trimmed.starts_with(crate::release::RELEASES_PREFIX) {
                    manifests.push(path);
                } else {
                    tiles.push(path);
                }
            }
            [site, manifests, tiles, checkpoint]
        };
        let mut uploaded = Vec::new();
        for group in groups {
            let futures: Vec<BoxFuture<'_, Result<()>>> = group
                .iter()
                .map(|path| {
                    let id = &id;
                    async move {
                        let body = bytes.get(*path).cloned().ok_or_else(|| {
                            SiteError::host(
                                NAME,
                                format!(
                                    "Netlify asked for '{path}', which this host cannot supply"
                                ),
                            )
                        })?;
                        let target = format!(
                            "deploys/{}/files/{}",
                            uri_encode(id, false),
                            uri_encode(path.trim_start_matches('/'), true)
                        );
                        self.call(Method::PUT, &target, Some(RequestBody::Bytes(body)))
                            .await
                            .map(drop)
                    }
                    .boxed()
                })
                .collect();
            futures::stream::iter(futures)
                .buffer_unordered(self.concurrency)
                .try_collect::<Vec<()>>()
                .await?;
            uploaded.extend(group.into_iter().cloned());
        }
        let ready = self.wait_ready(&id).await?;
        if !draft {
            let site = self.site().await?;
            if site.published_deploy.as_ref().map(|d| d.id.as_str()) != Some(id.as_str()) {
                self.call(
                    Method::POST,
                    &format!(
                        "sites/{}/deploys/{}/restore",
                        uri_encode(&self.site_id, false),
                        uri_encode(&id, false)
                    ),
                    None,
                )
                .await?;
            }
        }
        Ok((ready, uploaded))
    }

    async fn wait_required(&self, id: &str) -> Result<Deploy> {
        let start = Instant::now();
        loop {
            let deploy: Deploy =
                self.json(Method::GET, &format!("deploys/{}", uri_encode(id, false)), None).await?;
            if deploy.required.is_some()
                || matches!(deploy.state.as_deref(), Some("ready" | "error"))
            {
                return Ok(deploy);
            }
            if start.elapsed() > self.timeout {
                return Err(SiteError::host(
                    NAME,
                    format!("deploy {id} was not prepared within {:?}", self.timeout),
                ));
            }
            tokio::time::sleep(self.poll).await;
        }
    }

    async fn publish_locked(
        &self,
        release: &StagedRelease,
        target: HostTarget,
    ) -> Result<PublishReport> {
        let start = Instant::now();
        let seq = release.manifest.seq;
        let site = self.site().await?;
        let current = match &site.published_deploy {
            Some(deploy) => self.deploy_files(&deploy.id).await?,
            None => BTreeMap::new(),
        };
        let manifest = format!("/{}", manifest_path(seq));
        if target == HostTarget::Production {
            if current.contains_key(&manifest) {
                return Err(SiteError::Sequence(format!(
                    "release {seq} is already published on this host"
                )));
            }
            if let Some(previous) = release.manifest.previous.as_ref()
                && current.contains_key(&format!("/{}", manifest_path(seq - 1)))
            {
                let base = self.production_url(Some(&site)).await?;
                if let Some(bytes) = self.read_public(&base, &manifest_path(seq - 1)).await?
                    && file_digest(&bytes) != *previous
                {
                    return Err(SiteError::Sequence(format!(
                        "release {seq} follows a manifest this host does not serve as release {}",
                        seq - 1
                    )));
                }
            }
        }
        let mut files: BTreeMap<String, String> = BTreeMap::new();
        let mut bytes: BTreeMap<String, Bytes> = BTreeMap::new();
        let mut order: Vec<String> = Vec::new();
        for path in release.files.keys() {
            let body = Bytes::from(release.read(path)?);
            files.insert(path.clone(), sha1_hex(&body));
            bytes.insert(path.clone(), body);
            order.push(path.clone());
        }
        files.insert(manifest.clone(), sha1_hex(&release.manifest_bytes));
        bytes.insert(manifest.clone(), release.manifest_bytes.clone());
        order.push(manifest);
        for (path, body) in release.log.iter().flat_map(|log| log.files()) {
            let path = format!("/{path}");
            files.insert(path.clone(), sha1_hex(body));
            bytes.insert(path.clone(), body.clone());
            order.push(path);
        }
        for (path, sha) in &current {
            if is_log_path(path) && !files.contains_key(path) {
                files.insert(path.clone(), sha.clone());
            }
        }
        let title = format!("{TITLE} {seq} {}", release.digest);
        let (_, uploaded) =
            self.deploy(&title, &files, &order, &bytes, target == HostTarget::Preview).await?;
        tracing::info!(
            host = NAME,
            site.release.seq = seq,
            uploaded = uploaded.len(),
            "release deployed"
        );
        Ok(PublishReport {
            host: NAME.into(),
            target,
            seq,
            uploaded,
            removed: release.removed.iter().cloned().collect(),
            elapsed: start.elapsed(),
        })
    }

    async fn find_deploy(&self, seq: u64) -> Result<Option<Deploy>> {
        let prefix = format!("{TITLE} {seq} ");
        for page in 1.. {
            let batch: Vec<Deploy> = self
                .json(
                    Method::GET,
                    &format!(
                        "sites/{}/deploys?page={page}&per_page=100",
                        uri_encode(&self.site_id, false)
                    ),
                    None,
                )
                .await?;
            let last = batch.len() < 100;
            if let Some(deploy) = batch.into_iter().find(|d| {
                d.title.as_deref().is_some_and(|t| t.starts_with(&prefix))
                    && matches!(d.state.as_deref(), Some("ready" | "current"))
            }) {
                return Ok(Some(deploy));
            }
            if last {
                return Ok(None);
            }
        }
        Ok(None)
    }

    async fn rollback_locked(&self, to: &ReleaseManifest) -> Result<PublishReport> {
        let start = Instant::now();
        let not_found = |message: &str| SiteError::ReleaseNotFound {
            seq: to.seq,
            message: message.to_string(),
        };
        let stored = self
            .find_deploy(to.seq)
            .await?
            .ok_or_else(|| not_found("no deploy of it on Netlify"))?;
        let permalink: Url = as_dir(
            stored
                .deploy_ssl_url
                .as_deref()
                .ok_or_else(|| not_found("its deploy has no permalink"))?
                .parse()
                .map_err(|e| SiteError::host(NAME, format!("bad deploy URL: {e}")))?,
        );
        let read_manifest = |seq: u64| {
            let permalink = &permalink;
            async move {
                let bytes = self
                    .read_public(permalink, &manifest_path(seq))
                    .await?
                    .ok_or_else(|| not_found("its deploy does not serve the manifest"))?;
                serde_json::from_slice::<ReleaseManifest>(&bytes)
                    .map_err(|e| SiteError::serde(manifest_path(seq), e))
            }
        };
        let own = read_manifest(to.seq).await?;
        let (expected, _) = manifest_digest(to)?;
        let (actual, _) = manifest_digest(&own)?;
        if expected != actual {
            return Err(SiteError::DigestMismatch {
                path: format!("/{}", manifest_path(to.seq)),
                expected: expected.to_string(),
                actual: actual.to_string(),
            });
        }
        let mut chain = vec![own];
        while let Some(base) = chain.last().and_then(|m| m.base) {
            chain.push(read_manifest(base).await?);
        }
        let mut wanted: BTreeMap<String, FileEntry> = BTreeMap::new();
        for manifest in chain.iter().rev() {
            wanted = manifest.apply_to(&wanted);
        }
        let checks: Vec<BoxFuture<'_, Result<(String, Bytes)>>> = wanted
            .iter()
            .map(|(path, entry)| {
                let permalink = &permalink;
                async move {
                    let body = self.read_public(permalink, path).await?;
                    let actual = body
                        .as_ref()
                        .map_or_else(|| "missing".to_string(), |b| file_digest(b).to_string());
                    match body {
                        Some(body) if actual == entry.sha256.as_str() => Ok((path.clone(), body)),
                        _ => Err(SiteError::DigestMismatch {
                            path: path.clone(),
                            expected: entry.sha256.to_string(),
                            actual,
                        }),
                    }
                }
                .boxed()
            })
            .collect();
        let bytes: BTreeMap<String, Bytes> =
            futures::stream::iter(checks).buffer_unordered(self.concurrency).try_collect().await?;
        let site = self.site().await?;
        let current_id = site
            .published_deploy
            .as_ref()
            .map(|d| d.id.clone())
            .ok_or_else(|| not_found("nothing is published"))?;
        let current = self.deploy_files(&current_id).await?;
        let mut files: BTreeMap<String, String> =
            bytes.iter().map(|(path, body)| (path.clone(), sha1_hex(body))).collect();
        for (path, sha) in &current {
            if is_log_path(path) {
                files.insert(path.clone(), sha.clone());
            }
        }
        let order: Vec<String> = bytes.keys().cloned().collect();
        let title = format!("adk-site rollback to {}", to.seq);
        self.deploy(&title, &files, &order, &bytes, false).await?;
        tracing::warn!(host = NAME, site.rollback.to = to.seq, "rolled back");
        Ok(PublishReport {
            host: NAME.into(),
            target: HostTarget::Production,
            seq: to.seq,
            uploaded: order,
            removed: current
                .keys()
                .filter(|p| !is_log_path(p) && !files.contains_key(*p))
                .cloned()
                .collect(),
            elapsed: start.elapsed(),
        })
    }
}

enum RequestBody {
    Json(serde_json::Value),
    Bytes(Bytes),
}

#[async_trait]
impl SiteHost for NetlifyHost {
    fn name(&self) -> &str {
        NAME
    }

    async fn publish(&self, release: &StagedRelease, target: HostTarget) -> Result<PublishReport> {
        let _guard = self.lock.lock().await;
        self.publish_locked(release, target).await
    }

    async fn rollback(&self, to: &ReleaseManifest) -> Result<PublishReport> {
        let _guard = self.lock.lock().await;
        self.rollback_locked(to).await
    }

    async fn fetch(&self, path: &str) -> Result<Option<Bytes>> {
        let base = self.production_url(None).await?;
        self.read_public(&base, path).await
    }

    fn redirects_format(&self) -> RedirectsFormat {
        RedirectsFormat::Netlify
    }

    fn limits(&self) -> HostLimits {
        HostLimits {
            max_publishes_per_hour: Some(4),
            delta_upload: true,
            atomic: Atomicity::Deploy,
        }
    }

    async fn publish_delta(
        &self,
        delta: &StagedDelta,
        target: HostTarget,
    ) -> Result<PublishReport> {
        self.publish(delta.release(), target).await
    }
}
