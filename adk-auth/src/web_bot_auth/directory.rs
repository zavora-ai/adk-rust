//! Key directory sources and the verifier's directory cache. Spec 3.5 §7.3.
//!
//! A verifier resolves the origin a `Signature-Agent` member names to its key directory at
//! `/.well-known/http-message-signatures-directory` (draft-ietf-webbotauth-httpsig-protocol-00
//! §5.5). [`HttpKeyDirectorySource`] fetches it under the draft's SSRF guidance (§6.7): https
//! only, public addresses only, no redirects, a timeout and a size limit.
//! [`InMemoryKeyDirectorySource`] serves directories from memory for tests and for keys
//! configured out of band.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use adk_core::Clock;
use adk_core::webhook_delivery::{EgressPolicy, is_non_public};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio::sync::{Mutex, RwLock};

use super::keys::{DirectoryKey, KeyDirectory};
use super::{DIRECTORY_MEDIA_TYPE, DIRECTORY_WELL_KNOWN_PATH, WebBotAuthError};

/// A key directory and how long it may be cached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedDirectory {
    /// The directory.
    pub directory: KeyDirectory,
    /// The HTTP cache lifetime of the response (RFC 9111 §4.2.1), when it declared one.
    pub max_age: Option<Duration>,
}

/// Resolves a directory origin to its key directory (draft §5.5).
///
/// `origin` is an ASCII-serialised origin such as `https://bot.example`. Implementations
/// return [`WebBotAuthError::DirectoryUnavailable`] or [`WebBotAuthError::MalformedDirectory`]
/// when no directory can be read, and [`WebBotAuthError::EgressRefused`] when policy forbids the
/// fetch. Spec 3.5 §7.3.
///
/// # Example
///
/// ```rust
/// use std::time::Duration;
/// use adk_auth::web_bot_auth::{Ed25519SigningKey, InMemoryKeyDirectorySource, KeyDirectory, KeyDirectorySource};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let key = Ed25519SigningKey::generate().unwrap();
/// let source = InMemoryKeyDirectorySource::new();
/// source.insert("https://bot.example", KeyDirectory::from_keys([&key]), Some(Duration::from_secs(300))).await;
/// let fetched = source.fetch("https://bot.example").await.unwrap();
/// assert!(fetched.directory.find(key.thumbprint()).is_some());
/// # }
/// ```
#[async_trait]
pub trait KeyDirectorySource: Send + Sync {
    /// Fetches the key directory published at `origin`.
    async fn fetch(&self, origin: &str) -> Result<FetchedDirectory, WebBotAuthError>;
}

/// Directories held in memory, keyed by origin.
#[derive(Debug, Default)]
pub struct InMemoryKeyDirectorySource {
    directories: RwLock<HashMap<String, FetchedDirectory>>,
    fetches: AtomicUsize,
}

impl InMemoryKeyDirectorySource {
    /// Creates an empty source.
    pub fn new() -> Self {
        Self::default()
    }

    /// Publishes `directory` at `origin` with an optional cache lifetime.
    pub async fn insert(&self, origin: &str, directory: KeyDirectory, max_age: Option<Duration>) {
        self.directories
            .write()
            .await
            .insert(origin.to_string(), FetchedDirectory { directory, max_age });
    }

    /// Removes the directory at `origin`, so later fetches fail as unavailable.
    pub async fn remove(&self, origin: &str) {
        self.directories.write().await.remove(origin);
    }

    /// How many times [`fetch`](KeyDirectorySource::fetch) has been called.
    pub fn fetch_count(&self) -> usize {
        self.fetches.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl KeyDirectorySource for InMemoryKeyDirectorySource {
    async fn fetch(&self, origin: &str) -> Result<FetchedDirectory, WebBotAuthError> {
        self.fetches.fetch_add(1, Ordering::SeqCst);
        self.directories.read().await.get(origin).cloned().ok_or_else(|| {
            WebBotAuthError::DirectoryUnavailable {
                origin: origin.to_string(),
                reason: "no directory is published at this origin".into(),
            }
        })
    }
}

/// Fetches key directories over HTTPS under the draft's SSRF guidance (§6.7).
///
/// Defaults: https only and public addresses only ([`EgressPolicy::default`]), checked on the
/// URL and again on every resolved address; no proxy; no redirects (§5.5); a 5 s timeout;
/// 64 KiB and 64 keys at most; the response must be `200 OK` with media type
/// `application/http-message-signatures-directory+json`.
///
/// # Example
///
/// ```rust
/// use std::time::Duration;
/// use adk_auth::web_bot_auth::HttpKeyDirectorySource;
///
/// let source = HttpKeyDirectorySource::builder()
///     .timeout(Duration::from_secs(3))
///     .max_bytes(32 * 1024)
///     .build()
///     .unwrap();
/// # let _ = source;
/// ```
#[derive(Debug, Clone)]
pub struct HttpKeyDirectorySource {
    client: reqwest::Client,
    egress: EgressPolicy,
    max_bytes: usize,
    max_keys: usize,
}

impl HttpKeyDirectorySource {
    /// Creates a source with the default limits.
    ///
    /// # Errors
    ///
    /// Returns [`WebBotAuthError::InvalidConfig`] when the HTTP client cannot be built.
    pub fn new() -> Result<Self, WebBotAuthError> {
        Self::builder().build()
    }

    /// Starts a builder with the default limits.
    pub fn builder() -> HttpKeyDirectorySourceBuilder {
        HttpKeyDirectorySourceBuilder {
            egress: EgressPolicy::default(),
            timeout: Duration::from_secs(5),
            max_bytes: 64 * 1024,
            max_keys: 64,
        }
    }

    fn check_egress(&self, origin: &str, url: &reqwest::Url) -> Result<(), WebBotAuthError> {
        let refused =
            |reason: String| WebBotAuthError::EgressRefused { origin: origin.to_string(), reason };
        match url.scheme() {
            "https" => {}
            "http" if self.egress.allow_http => {}
            other => return Err(refused(format!("scheme `{other}` is not allowed; use https"))),
        }
        if self.egress.allow_private {
            return Ok(());
        }
        let host = url.host_str().ok_or_else(|| refused("the origin has no host".into()))?;
        let unbracketed = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host);
        if let Ok(ip) = unbracketed.parse::<IpAddr>() {
            if is_non_public(ip) {
                return Err(refused(format!("{ip} is not a public address")));
            }
            return Ok(());
        }
        let name = host.trim_end_matches('.').to_ascii_lowercase();
        if name == "localhost" || name.ends_with(".localhost") {
            return Err(refused("localhost is not a public address".into()));
        }
        Ok(())
    }
}

/// Builds an [`HttpKeyDirectorySource`].
#[derive(Debug, Clone)]
pub struct HttpKeyDirectorySourceBuilder {
    egress: EgressPolicy,
    timeout: Duration,
    max_bytes: usize,
    max_keys: usize,
}

impl HttpKeyDirectorySourceBuilder {
    /// Sets the egress policy. [`EgressPolicy::allow_private_networks`] admits loopback,
    /// private addresses and plain http, for tests and in-cluster directories only.
    pub fn egress_policy(mut self, egress: EgressPolicy) -> Self {
        self.egress = egress;
        self
    }

    /// Sets the wall-clock limit for one fetch.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Sets the largest directory body accepted.
    pub fn max_bytes(mut self, max_bytes: usize) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    /// Sets the largest number of keys a directory may list.
    pub fn max_keys(mut self, max_keys: usize) -> Self {
        self.max_keys = max_keys;
        self
    }

    /// Builds the source.
    ///
    /// # Errors
    ///
    /// Returns [`WebBotAuthError::InvalidConfig`] when the HTTP client cannot be built.
    pub fn build(self) -> Result<HttpKeyDirectorySource, WebBotAuthError> {
        let mut builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(self.timeout)
            .connect_timeout(self.timeout.min(Duration::from_secs(5)))
            .user_agent(concat!("adk-rust-web-bot-auth/", env!("CARGO_PKG_VERSION")));
        if !self.egress.allow_private {
            // A proxy would resolve the host itself, out of the resolver's reach.
            builder = builder.no_proxy().dns_resolver(Arc::new(PublicOnlyResolver));
        }
        let client = builder
            .build()
            .map_err(|e| WebBotAuthError::InvalidConfig { reason: e.to_string() })?;
        Ok(HttpKeyDirectorySource {
            client,
            egress: self.egress,
            max_bytes: self.max_bytes,
            max_keys: self.max_keys,
        })
    }
}

#[async_trait]
impl KeyDirectorySource for HttpKeyDirectorySource {
    async fn fetch(&self, origin: &str) -> Result<FetchedDirectory, WebBotAuthError> {
        let unavailable = |reason: String| WebBotAuthError::DirectoryUnavailable {
            origin: origin.to_string(),
            reason,
        };
        let malformed = |reason: String| WebBotAuthError::MalformedDirectory {
            origin: origin.to_string(),
            reason,
        };
        let url =
            reqwest::Url::parse(&format!("{origin}{DIRECTORY_WELL_KNOWN_PATH}")).map_err(|e| {
                WebBotAuthError::InvalidConfig {
                    reason: format!("`{origin}` is not an origin: {e}"),
                }
            })?;
        self.check_egress(origin, &url)?;
        let mut response = self
            .client
            .get(url)
            .header(reqwest::header::ACCEPT, DIRECTORY_MEDIA_TYPE)
            .send()
            .await
            .map_err(|error| {
                let refused = std::iter::successors(
                    Some(&error as &(dyn std::error::Error + 'static)),
                    |e| e.source(),
                )
                .find_map(|e| e.downcast_ref::<ResolverRefused>());
                match refused {
                    Some(refused) => WebBotAuthError::EgressRefused {
                        origin: origin.to_string(),
                        reason: refused.0.clone(),
                    },
                    None => unavailable(error.without_url().to_string()),
                }
            })?;
        let status = response.status();
        if status != reqwest::StatusCode::OK {
            return Err(unavailable(format!(
                "it answered {status}; a directory answers 200 OK and redirects are not followed"
            )));
        }
        let media_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .map(|value| value.trim().to_ascii_lowercase());
        if media_type.as_deref() != Some(DIRECTORY_MEDIA_TYPE) {
            return Err(malformed(format!(
                "media type is {}, not {DIRECTORY_MEDIA_TYPE}",
                media_type.as_deref().unwrap_or("absent")
            )));
        }
        if response.content_length().is_some_and(|length| length > self.max_bytes as u64) {
            return Err(malformed(format!("it is larger than {} bytes", self.max_bytes)));
        }
        let max_age = cache_lifetime(response.headers());
        let mut body = Vec::new();
        while let Some(chunk) =
            response.chunk().await.map_err(|e| unavailable(e.without_url().to_string()))?
        {
            if body.len() + chunk.len() > self.max_bytes {
                return Err(malformed(format!("it is larger than {} bytes", self.max_bytes)));
            }
            body.extend_from_slice(&chunk);
        }
        let directory = KeyDirectory::parse(&body, self.max_keys).map_err(malformed)?;
        Ok(FetchedDirectory { directory, max_age })
    }
}

/// The freshness lifetime of a response (RFC 9111 §4.2.1, less its `Age`): `no-store` and
/// `no-cache` give zero, then `max-age`, then `Expires` minus `Date`.
fn cache_lifetime(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let header = |name: reqwest::header::HeaderName| {
        headers.get(name).and_then(|value| value.to_str().ok()).map(str::to_string)
    };
    let mut lifetime = None;
    if let Some(cache_control) = header(reqwest::header::CACHE_CONTROL) {
        for directive in cache_control.split(',').map(|d| d.trim().to_ascii_lowercase()) {
            if directive == "no-store" || directive == "no-cache" {
                return Some(Duration::ZERO);
            }
            if lifetime.is_none() {
                lifetime = directive
                    .strip_prefix("max-age=")
                    .and_then(|v| v.trim_matches('"').parse::<u64>().ok())
                    .map(Duration::from_secs);
            }
        }
    }
    if lifetime.is_none()
        && let Some(expires) = header(reqwest::header::EXPIRES)
    {
        let date = header(reqwest::header::DATE).and_then(|d| httpdate::parse_http_date(&d).ok());
        lifetime = Some(match (httpdate::parse_http_date(&expires), date) {
            (Ok(expires), Some(date)) => expires.duration_since(date).unwrap_or_default(),
            (Ok(_), None) | (Err(_), _) => Duration::ZERO,
        });
    }
    let age = header(reqwest::header::AGE).and_then(|a| a.parse::<u64>().ok()).unwrap_or(0);
    lifetime.map(|lifetime| lifetime.saturating_sub(Duration::from_secs(age)))
}

/// Resolves host names and refuses any answer that contains a non-public address.
struct PublicOnlyResolver;

impl reqwest::dns::Resolve for PublicOnlyResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?
                .collect();
            if let Some(blocked) = addrs.iter().find(|addr| is_non_public(addr.ip())) {
                let reason = format!("{host} resolves to the non-public address {}", blocked.ip());
                return Err(
                    Box::new(ResolverRefused(reason)) as Box<dyn std::error::Error + Send + Sync>
                );
            }
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

#[derive(Debug)]
struct ResolverRefused(String);

impl std::fmt::Display for ResolverRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "egress refused: {}", self.0)
    }
}

impl std::error::Error for ResolverRefused {}

/// How the verifier caches key directories (draft Appendix C.4–C.5).
///
/// A directory is fresh for its HTTP cache lifetime clamped to `min_ttl..=max_ttl`, or
/// `default_ttl` when the response declares none. A failed refresh never evicts a cached
/// directory (draft §6.10): the stale copy keeps verifying for up to `stale_if_error` past its
/// freshness. A failed fetch with nothing cached is remembered for `negative_ttl`, so a burst of
/// requests naming a broken directory fetches it once.
///
/// # Example
///
/// ```rust
/// use std::time::Duration;
/// use adk_auth::web_bot_auth::DirectoryCachePolicy;
///
/// let policy = DirectoryCachePolicy { max_ttl: Duration::from_secs(3600), ..Default::default() };
/// assert_eq!(policy.min_ttl, Duration::from_secs(60));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectoryCachePolicy {
    /// Shortest time a directory is cached, and the least age before an unknown `keyid` may
    /// trigger a refetch. Default 60 s.
    pub min_ttl: Duration,
    /// Longest time a directory is cached. Default 24 h.
    pub max_ttl: Duration,
    /// Lifetime when the response declares none. Default 5 min.
    pub default_ttl: Duration,
    /// How long a stale directory keeps verifying while refreshes fail. Default 1 h.
    pub stale_if_error: Duration,
    /// How long a failed fetch is remembered. Default 30 s (the draft caps it at 5 min).
    pub negative_ttl: Duration,
    /// Most origins held at once; the oldest entry is evicted beyond it. Default 10,000.
    pub max_entries: usize,
}

impl Default for DirectoryCachePolicy {
    fn default() -> Self {
        Self {
            min_ttl: Duration::from_secs(60),
            max_ttl: Duration::from_secs(24 * 60 * 60),
            default_ttl: Duration::from_secs(5 * 60),
            stale_if_error: Duration::from_secs(60 * 60),
            negative_ttl: Duration::from_secs(30),
            max_entries: 10_000,
        }
    }
}

#[derive(Debug, Clone)]
enum CacheEntry {
    Directory {
        directory: Arc<KeyDirectory>,
        fetched_at: DateTime<Utc>,
        fresh_until: DateTime<Utc>,
    },
    Failed {
        error: WebBotAuthError,
        until: DateTime<Utc>,
        stale: Option<Box<CacheEntry>>,
    },
}

/// The verifier's per-origin directory cache: fresh hits, coalesced fetches, stale-on-error and
/// negative caching.
pub(crate) struct DirectoryCache {
    source: Arc<dyn KeyDirectorySource>,
    policy: DirectoryCachePolicy,
    clock: Arc<dyn Clock>,
    entries: RwLock<HashMap<String, CacheEntry>>,
    inflight: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl std::fmt::Debug for DirectoryCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DirectoryCache").field("policy", &self.policy).finish_non_exhaustive()
    }
}

impl DirectoryCache {
    pub(crate) fn new(
        source: Arc<dyn KeyDirectorySource>,
        policy: DirectoryCachePolicy,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            source,
            policy,
            clock,
            entries: RwLock::new(HashMap::new()),
            inflight: Mutex::new(HashMap::new()),
        }
    }

    /// The key `keyid` from the directory at `origin`.
    pub(crate) async fn key(
        &self,
        origin: &str,
        keyid: &str,
    ) -> Result<DirectoryKey, WebBotAuthError> {
        if let Lookup::Done(result) = self.lookup(origin, keyid).await {
            return result;
        }
        // Concurrent misses for one origin share a single fetch (draft Appendix C.3).
        let lock = {
            let mut inflight = self.inflight.lock().await;
            Arc::clone(inflight.entry(origin.to_string()).or_default())
        };
        let result = {
            let _guard = lock.lock().await;
            // Another task may have refreshed the entry while this one waited.
            match self.lookup(origin, keyid).await {
                Lookup::Done(result) => result,
                Lookup::Fetch => self.refresh(origin, keyid).await,
            }
        };
        let mut inflight = self.inflight.lock().await;
        if inflight.get(origin).is_some_and(|l| Arc::strong_count(l) <= 2) {
            inflight.remove(origin);
        }
        result
    }

    async fn lookup(&self, origin: &str, keyid: &str) -> Lookup {
        let now = self.clock.now();
        let entries = self.entries.read().await;
        match entries.get(origin) {
            Some(CacheEntry::Directory { directory, fetched_at, fresh_until })
                if now < *fresh_until =>
            {
                match directory.find(keyid) {
                    Some(key) => Lookup::Done(Ok(key.clone())),
                    // An unknown keyid may be a key added since; refetch once the entry is old
                    // enough that a stream of invented keyids cannot force a fetch per request.
                    None if now - *fetched_at >= chrono_duration(self.policy.min_ttl) => {
                        Lookup::Fetch
                    }
                    None => Lookup::Done(Err(unknown_key(origin, keyid))),
                }
            }
            Some(CacheEntry::Failed { error, until, stale }) if now < *until => {
                match stale.as_deref().and_then(|stale| self.usable_stale(stale, now)) {
                    Some(directory) => Lookup::Done(
                        directory.find(keyid).cloned().ok_or_else(|| unknown_key(origin, keyid)),
                    ),
                    None => Lookup::Done(Err(error.clone())),
                }
            }
            Some(CacheEntry::Directory { .. } | CacheEntry::Failed { .. }) | None => Lookup::Fetch,
        }
    }

    fn usable_stale(&self, entry: &CacheEntry, now: DateTime<Utc>) -> Option<Arc<KeyDirectory>> {
        match entry {
            CacheEntry::Directory { directory, fresh_until, .. }
                if now < *fresh_until + chrono_duration(self.policy.stale_if_error) =>
            {
                Some(Arc::clone(directory))
            }
            CacheEntry::Directory { .. } | CacheEntry::Failed { .. } => None,
        }
    }

    async fn refresh(&self, origin: &str, keyid: &str) -> Result<DirectoryKey, WebBotAuthError> {
        let fetched = self.source.fetch(origin).await;
        let now = self.clock.now();
        let mut entries = self.entries.write().await;
        match fetched {
            Ok(fetched) => {
                let ttl = fetched
                    .max_age
                    .unwrap_or(self.policy.default_ttl)
                    .clamp(self.policy.min_ttl, self.policy.max_ttl.max(self.policy.min_ttl));
                let directory = Arc::new(fetched.directory);
                let key = directory.find(keyid).cloned();
                // A directory that resolves is evidence and replaces what was cached (draft §4.4).
                self.insert(
                    &mut entries,
                    origin,
                    CacheEntry::Directory {
                        directory,
                        fetched_at: now,
                        fresh_until: now + chrono_duration(ttl),
                    },
                    now,
                );
                tracing::debug!(
                    directory.origin = origin,
                    cache.ttl_secs = ttl.as_secs(),
                    "key directory fetched"
                );
                key.ok_or_else(|| unknown_key(origin, keyid))
            }
            Err(error) => {
                // A failed fetch is not evidence and never evicts a cached directory (draft §6.10).
                let stale = match entries.remove(origin) {
                    Some(entry @ CacheEntry::Directory { .. }) => Some(Box::new(entry)),
                    Some(CacheEntry::Failed { stale, .. }) => stale,
                    None => None,
                };
                let usable = stale.as_deref().and_then(|entry| self.usable_stale(entry, now));
                tracing::warn!(directory.origin = origin, error = %error, stale = usable.is_some(), "key directory fetch failed");
                let result = match &usable {
                    Some(directory) => {
                        directory.find(keyid).cloned().ok_or_else(|| unknown_key(origin, keyid))
                    }
                    None => Err(error.clone()),
                };
                let until = now + chrono_duration(self.policy.negative_ttl);
                self.insert(&mut entries, origin, CacheEntry::Failed { error, until, stale }, now);
                result
            }
        }
    }

    fn insert(
        &self,
        entries: &mut HashMap<String, CacheEntry>,
        origin: &str,
        entry: CacheEntry,
        now: DateTime<Utc>,
    ) {
        if entries.len() >= self.policy.max_entries && !entries.contains_key(origin) {
            entries.retain(|_, entry| match entry {
                CacheEntry::Directory { fresh_until, .. } => *fresh_until > now,
                CacheEntry::Failed { until, .. } => *until > now,
            });
            if entries.len() >= self.policy.max_entries {
                let oldest = entries
                    .iter()
                    .min_by_key(|(_, entry)| match entry {
                        CacheEntry::Directory { fetched_at, .. } => *fetched_at,
                        CacheEntry::Failed { until, .. } => *until,
                    })
                    .map(|(origin, _)| origin.clone());
                if let Some(oldest) = oldest {
                    entries.remove(&oldest);
                }
            }
        }
        entries.insert(origin.to_string(), entry);
    }
}

enum Lookup {
    Done(Result<DirectoryKey, WebBotAuthError>),
    Fetch,
}

fn unknown_key(origin: &str, keyid: &str) -> WebBotAuthError {
    WebBotAuthError::UnknownKey { origin: origin.to_string(), keyid: keyid.to_string() }
}

fn chrono_duration(duration: Duration) -> chrono::Duration {
    chrono::Duration::from_std(duration).unwrap_or(chrono::Duration::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderMap, HeaderValue};

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(*name, HeaderValue::from_str(value).unwrap());
        }
        map
    }

    #[test]
    fn cache_lifetime_reads_cache_control_expires_and_age() {
        assert_eq!(
            cache_lifetime(&headers(&[("cache-control", "public, max-age=86400")])),
            Some(Duration::from_secs(86_400))
        );
        assert_eq!(
            cache_lifetime(&headers(&[("cache-control", "max-age=600, no-cache")])),
            Some(Duration::ZERO)
        );
        assert_eq!(
            cache_lifetime(&headers(&[("cache-control", "max-age=600"), ("age", "100")])),
            Some(Duration::from_secs(500))
        );
        assert_eq!(
            cache_lifetime(&headers(&[
                ("date", "Sun, 25 Oct 2026 02:00:00 GMT"),
                ("expires", "Sun, 25 Oct 2026 03:00:00 GMT"),
            ])),
            Some(Duration::from_secs(3600))
        );
        assert_eq!(cache_lifetime(&headers(&[("expires", "0")])), Some(Duration::ZERO));
        assert_eq!(cache_lifetime(&headers(&[])), None);
    }

    #[tokio::test]
    async fn resolver_refuses_names_that_resolve_to_loopback() {
        use reqwest::dns::Resolve;
        use std::str::FromStr;
        let name = reqwest::dns::Name::from_str("localhost").unwrap();
        let error = match PublicOnlyResolver.resolve(name).await {
            Ok(_) => panic!("localhost must be refused"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("non-public"), "{error}");
    }

    #[test]
    fn defaults_follow_the_draft_ssrf_guidance() {
        let builder = HttpKeyDirectorySource::builder();
        assert_eq!(builder.egress, EgressPolicy { allow_private: false, allow_http: false });
        assert_eq!(builder.timeout, Duration::from_secs(5));
        assert_eq!(builder.max_bytes, 64 * 1024);
        assert_eq!(builder.max_keys, 64);
        assert_eq!(
            DirectoryCachePolicy::default(),
            DirectoryCachePolicy {
                min_ttl: Duration::from_secs(60),
                max_ttl: Duration::from_secs(86_400),
                default_ttl: Duration::from_secs(300),
                stale_if_error: Duration::from_secs(3600),
                negative_ttl: Duration::from_secs(30),
                max_entries: 10_000,
            }
        );
    }

    #[tokio::test]
    async fn the_cache_evicts_the_oldest_origin_beyond_max_entries() {
        let clock = adk_core::ManualClock::new(chrono::Utc::now());
        let source = Arc::new(InMemoryKeyDirectorySource::new());
        let key = super::super::Ed25519SigningKey::generate().unwrap();
        for origin in ["https://a.example", "https://b.example", "https://c.example"] {
            source.insert(origin, KeyDirectory::from_keys([&key]), None).await;
        }
        let policy = DirectoryCachePolicy { max_entries: 2, ..DirectoryCachePolicy::default() };
        let cache = DirectoryCache::new(source.clone(), policy, Arc::new(clock.clone()));
        for origin in ["https://a.example", "https://b.example", "https://c.example"] {
            cache.key(origin, key.thumbprint()).await.unwrap();
            clock.advance(Duration::from_secs(1));
        }
        assert_eq!(cache.entries.read().await.len(), 2);
        assert!(!cache.entries.read().await.contains_key("https://a.example"));
        cache.key("https://a.example", key.thumbprint()).await.unwrap();
        assert_eq!(source.fetch_count(), 4);
    }

    #[test]
    fn egress_refuses_non_public_origins() {
        let source = HttpKeyDirectorySource::new().unwrap();
        for origin in [
            "https://127.0.0.1",
            "https://10.0.0.1",
            "https://169.254.169.254",
            "https://[::1]",
            "https://localhost",
            "https://api.localhost",
            "http://bot.example",
        ] {
            let url = reqwest::Url::parse(&format!("{origin}{DIRECTORY_WELL_KNOWN_PATH}")).unwrap();
            assert!(
                matches!(
                    source.check_egress(origin, &url),
                    Err(WebBotAuthError::EgressRefused { .. })
                ),
                "{origin} must be refused"
            );
        }
        let url = reqwest::Url::parse("https://bot.example/.well-known/x").unwrap();
        assert!(source.check_egress("https://bot.example", &url).is_ok());
    }
}
