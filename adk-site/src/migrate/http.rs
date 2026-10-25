//! The one HTTP client of the migration stages (spec 3.5 §7.3 "Outbound", §7.8 "Sources").
//!
//! | Rule | How |
//! |------|-----|
//! | Signed | Every crawler request is signed through a [`CrawlSigner`]: the site's Web Bot Auth key ([`HttpSignatureSigner`]); each redirect hop is a new request, signed again |
//! | Robots-respecting | `robots.txt` is read before any other request to an origin, and a disallowed URL is never requested |
//! | Rate-limited | One request at a time per origin, at least [`HttpClientBuilder::min_interval`] apart, longer when `Crawl-delay` says so |
//! | Egress-checked | [`EgressScope::Public`] refuses non-public addresses (also after DNS); [`EgressScope::Only`] allows the listed origins and nothing else |
//! | Recorded | Every request, sent or refused, is a [`ConnectionRecord`] |
//!
//! An origin map sends requests for a public origin to another address with the public `Host`
//! header: the private vantage point of a freeze or the loopback port of an isolated replay.

use std::collections::{BTreeSet, HashMap};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_auth::web_bot_auth::HttpSignatureSigner;
use adk_core::webhook_delivery::is_non_public;
use adk_core::{Clock, SystemClock};
use bytes::{Bytes, BytesMut};
use chrono::{DateTime, Utc};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use url::Url;

use super::error::{MigrateError, Result};
use super::robots::RobotsTxt;

/// Signs outgoing crawler requests (spec 3.5 §7.3 "Outbound").
///
/// [`HttpSignatureSigner`] implements it with the site's Web Bot Auth key; the site publishes
/// the matching key directory so the sites it reads can verify it.
pub trait CrawlSigner: Send + Sync + std::fmt::Debug {
    /// Adds the signature fields to `request`.
    ///
    /// # Errors
    ///
    /// Returns why the request could not be signed.
    fn sign(&self, request: &mut reqwest::Request) -> std::result::Result<(), String>;
}

impl CrawlSigner for HttpSignatureSigner {
    fn sign(&self, request: &mut reqwest::Request) -> std::result::Result<(), String> {
        self.sign_reqwest(request).map_err(|e| e.to_string())
    }
}

/// Who a request presents itself as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FetchProfile {
    /// The site's crawler: its own user agent, signed.
    Crawler,
    /// A browser user agent, unsigned: the second half of a cloaking check, run only on a
    /// proven domain (A-I9).
    Browser,
}

/// Which addresses requests may reach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EgressScope {
    /// Public addresses over `http` and `https`; loopback, private, link-local and other
    /// non-public addresses are refused, also when a name resolves to one.
    Public,
    /// Only these origins (`scheme://host:port`, after the origin map); everything else is
    /// refused before a connection is made.
    Only(BTreeSet<String>),
}

/// One request the client sent or refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionRecord {
    /// The URL connected to, after the origin map.
    pub url: String,
    /// Its origin.
    pub origin: String,
    /// Whether the request was sent.
    pub allowed: bool,
    /// Whether the host is a loopback address or `localhost`.
    pub loopback: bool,
    /// Why it was refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// When.
    pub at: DateTime<Utc>,
}

/// How to make one request.
#[derive(Debug, Clone)]
pub struct FetchOptions {
    /// Who the request presents itself as.
    pub profile: FetchProfile,
    /// Extra headers, such as `Authorization` for drafts.
    pub headers: HeaderMap,
    /// Redirects to follow; 0 returns the redirect itself.
    pub follow: u8,
}

impl FetchOptions {
    /// A signed crawler request that does not follow redirects.
    pub fn crawler() -> Self {
        Self { profile: FetchProfile::Crawler, headers: HeaderMap::new(), follow: 0 }
    }

    /// An unsigned browser request that does not follow redirects.
    pub fn browser() -> Self {
        Self { profile: FetchProfile::Browser, headers: HeaderMap::new(), follow: 0 }
    }

    /// Follows up to `hops` redirects, each a new, signed and checked request.
    #[must_use]
    pub fn following(mut self, hops: u8) -> Self {
        self.follow = hops;
        self
    }

    /// Adds a header.
    #[must_use]
    pub fn header(mut self, name: HeaderName, value: HeaderValue) -> Self {
        self.headers.insert(name, value);
        self
    }
}

/// A response.
#[derive(Debug, Clone)]
pub struct Fetched {
    /// The public URL of the last hop.
    pub url: Url,
    /// Every hop's public URL and status, the last one included.
    pub hops: Vec<(Url, u16)>,
    /// The last hop's status.
    pub status: u16,
    /// Its headers.
    pub headers: HeaderMap,
    /// Its body, up to the client's limit.
    pub body: Bytes,
    /// Whether the body was cut at the limit.
    pub truncated: bool,
    /// A redirect's target, resolved and mapped back to the public origin.
    pub location: Option<Url>,
    /// The server's TLS certificate (DER), on `https`.
    pub peer_certificate: Option<Vec<u8>>,
}

impl Fetched {
    /// The media type without parameters, lower case.
    pub fn content_type(&self) -> Option<String> {
        self.headers
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.split(';').next().unwrap_or_default().trim().to_ascii_lowercase())
    }

    /// Whether the body is HTML.
    pub fn is_html(&self) -> bool {
        self.content_type().is_some_and(|t| t == "text/html" || t == "application/xhtml+xml")
    }

    /// The body as text, invalid UTF-8 replaced.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// Whether the status is a redirect.
    pub fn is_redirect(&self) -> bool {
        matches!(self.status, 301 | 302 | 303 | 307 | 308)
    }

    /// A header's value as text.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }
}

/// `scheme://host[:port]` of a URL, the port shown only when it is not the scheme's default.
pub fn origin_of(url: &Url) -> String {
    url.origin().ascii_serialization()
}

/// A Chrome user agent for the browser half of a cloaking check.
pub const BROWSER_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
     AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";

/// Builds an [`HttpClient`].
pub struct HttpClientBuilder {
    robots_token: String,
    user_agent: String,
    signer: Option<Arc<dyn CrawlSigner>>,
    egress: EgressScope,
    origin_map: Vec<(String, Url)>,
    min_interval: Duration,
    timeout: Duration,
    max_body: usize,
    clock: Arc<dyn Clock>,
}

impl HttpClientBuilder {
    /// Identifies the crawler as `robots_token` in `robots.txt` matching and in its user agent.
    #[must_use]
    pub fn robots_token(mut self, token: impl Into<String>) -> Self {
        self.robots_token = token.into();
        self
    }

    /// Sets the crawler's full user agent.
    #[must_use]
    pub fn user_agent(mut self, user_agent: impl Into<String>) -> Self {
        self.user_agent = user_agent.into();
        self
    }

    /// Signs crawler requests with `signer`.
    #[must_use]
    pub fn signer(mut self, signer: Arc<dyn CrawlSigner>) -> Self {
        self.signer = Some(signer);
        self
    }

    /// Sets which addresses requests may reach.
    #[must_use]
    pub fn egress(mut self, egress: EgressScope) -> Self {
        self.egress = egress;
        self
    }

    /// Sends requests for `public` (an origin) to `fetch` with `public`'s `Host` header.
    #[must_use]
    pub fn map_origin(mut self, public: &Url, fetch: Url) -> Self {
        self.origin_map.push((origin_of(public), fetch));
        self
    }

    /// The minimum time between two requests to one origin (default one second).
    #[must_use]
    pub fn min_interval(mut self, interval: Duration) -> Self {
        self.min_interval = interval;
        self
    }

    /// The timeout of one request (default 30 seconds).
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The largest body read (default 20 MiB).
    #[must_use]
    pub fn max_body(mut self, bytes: usize) -> Self {
        self.max_body = bytes;
        self
    }

    /// Stamps connection records with `clock`.
    #[must_use]
    pub fn clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Builds the client.
    ///
    /// # Errors
    ///
    /// [`MigrateError::Http`] when the TLS stack cannot be set up.
    pub fn build(self) -> Result<HttpClient> {
        let mut builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .tls_info(true)
            .timeout(self.timeout)
            .connect_timeout(self.timeout.min(Duration::from_secs(10)));
        if self.egress == EgressScope::Public {
            builder = builder.dns_resolver(Arc::new(PublicOnlyResolver));
        }
        let client = builder.build().map_err(|e| MigrateError::http("client", e))?;
        Ok(HttpClient {
            client,
            robots_token: self.robots_token,
            user_agent: self.user_agent,
            signer: self.signer,
            egress: self.egress,
            origin_map: self.origin_map,
            min_interval: self.min_interval,
            max_body: self.max_body,
            clock: self.clock,
            state: Mutex::new(ClientState::default()),
            robots: tokio::sync::Mutex::new(HashMap::new()),
        })
    }
}

#[derive(Default)]
struct ClientState {
    next_slot: HashMap<String, tokio::time::Instant>,
    delays: HashMap<String, Duration>,
    records: Vec<ConnectionRecord>,
}

/// The signed, robots-respecting, rate-limited, egress-checked client every migration
/// request goes through (spec 3.5 §7.3, §7.8).
///
/// # Example
///
/// ```rust
/// use std::collections::BTreeSet;
/// use adk_site::migrate::http::{EgressScope, HttpClient};
///
/// let client = HttpClient::builder()
///     .egress(EgressScope::Only(BTreeSet::from(["http://127.0.0.1:8080".to_string()])))
///     .build()
///     .unwrap();
/// assert!(client.connections().is_empty());
/// ```
pub struct HttpClient {
    client: reqwest::Client,
    robots_token: String,
    user_agent: String,
    signer: Option<Arc<dyn CrawlSigner>>,
    egress: EgressScope,
    origin_map: Vec<(String, Url)>,
    min_interval: Duration,
    max_body: usize,
    clock: Arc<dyn Clock>,
    state: Mutex<ClientState>,
    robots: tokio::sync::Mutex<HashMap<String, Arc<RobotsTxt>>>,
}

impl std::fmt::Debug for HttpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpClient")
            .field("robots_token", &self.robots_token)
            .field("signed", &self.signer.is_some())
            .field("egress", &self.egress)
            .field("min_interval", &self.min_interval)
            .finish_non_exhaustive()
    }
}

impl HttpClient {
    /// A builder: public egress, one request per second per origin, unsigned until
    /// [`HttpClientBuilder::signer`] is set.
    pub fn builder() -> HttpClientBuilder {
        HttpClientBuilder {
            robots_token: "adk-site".into(),
            user_agent: format!("adk-site/{} (AWP site migration)", env!("CARGO_PKG_VERSION")),
            signer: None,
            egress: EgressScope::Public,
            origin_map: Vec::new(),
            min_interval: Duration::from_secs(1),
            timeout: Duration::from_secs(30),
            max_body: 20 * 1024 * 1024,
            clock: Arc::new(SystemClock),
        }
    }

    /// The product token the crawler matches `robots.txt` groups with.
    pub fn robots_token(&self) -> &str {
        &self.robots_token
    }

    /// Whether crawler requests are signed.
    pub fn is_signed(&self) -> bool {
        self.signer.is_some()
    }

    /// Every request sent or refused, in order.
    pub fn connections(&self) -> Vec<ConnectionRecord> {
        self.state.lock().map(|s| s.records.clone()).unwrap_or_default()
    }

    /// The requests sent to anything other than a loopback address.
    pub fn outbound_connections(&self) -> Vec<ConnectionRecord> {
        self.connections().into_iter().filter(|r| r.allowed && !r.loopback).collect()
    }

    /// The `robots.txt` that applies to `url`'s origin, read once per origin.
    ///
    /// A missing file (4xx) allows everything; an unreadable one (5xx, no answer) disallows
    /// everything (RFC 9309 §2.3.1).
    pub async fn robots_for(&self, url: &Url) -> Arc<RobotsTxt> {
        let origin = origin_of(url);
        let mut cache = self.robots.lock().await;
        if let Some(robots) = cache.get(&origin) {
            return Arc::clone(robots);
        }
        let robots = match url.join("/robots.txt") {
            Ok(robots_url) => {
                match self.send_following(&robots_url, &FetchOptions::crawler(), 5).await {
                    Ok(fetched) if (200..300).contains(&fetched.status) => {
                        RobotsTxt::parse(&fetched.text())
                    }
                    Ok(fetched) if (400..500).contains(&fetched.status) => RobotsTxt::allow_all(),
                    _ => RobotsTxt::disallow_all(),
                }
            }
            Err(_) => RobotsTxt::disallow_all(),
        };
        let robots = Arc::new(robots);
        if let Some(delay) = robots.crawl_delay(&self.robots_token)
            && let Some(fetch) = self.fetch_url(url).ok().map(|(u, _)| origin_of(&u))
            && let Ok(mut state) = self.state.lock()
        {
            state.delays.insert(fetch, delay);
        }
        cache.insert(origin, Arc::clone(&robots));
        robots
    }

    /// Requests `url` as `options` say, after `robots.txt` allows it.
    ///
    /// # Errors
    ///
    /// [`MigrateError::RobotsDisallowed`], [`MigrateError::Egress`] before any connection;
    /// [`MigrateError::Http`] when no answer arrives. Any HTTP status is an answer.
    pub async fn fetch(&self, url: &Url, options: &FetchOptions) -> Result<Fetched> {
        let mut current = url.clone();
        let mut hops = Vec::new();
        loop {
            let (target, _) = self.fetch_url(&current)?;
            if let Err(reason) = self.check_egress(&target) {
                self.record(&target, false, Some(reason.clone()));
                return Err(MigrateError::Egress { url: current.to_string(), reason });
            }
            self.check_robots(&current).await?;
            let mut fetched = self.send(&current, options).await?;
            hops.push((current.clone(), fetched.status));
            match fetched.location.clone() {
                Some(next)
                    if fetched.is_redirect() && hops.len() <= usize::from(options.follow) =>
                {
                    current = next;
                }
                _ => {
                    fetched.hops = hops;
                    return Ok(fetched);
                }
            }
        }
    }

    async fn check_robots(&self, url: &Url) -> Result<()> {
        let robots = self.robots_for(url).await;
        let path = match url.query() {
            Some(query) => format!("{}?{query}", url.path()),
            None => url.path().to_string(),
        };
        if robots.allows(&self.robots_token, &path) {
            Ok(())
        } else {
            Err(MigrateError::RobotsDisallowed { url: url.to_string() })
        }
    }

    /// Follows redirects without consulting `robots.txt`: for `robots.txt` itself.
    async fn send_following(&self, url: &Url, options: &FetchOptions, hops: u8) -> Result<Fetched> {
        let mut current = url.clone();
        for _ in 0..=hops {
            let fetched = self.send(&current, options).await?;
            match fetched.location.clone() {
                Some(next) if fetched.is_redirect() => current = next,
                _ => return Ok(fetched),
            }
        }
        Err(MigrateError::http(url, "too many redirects"))
    }

    /// The URL to connect to and the `Host` header to send, after the origin map.
    fn fetch_url(&self, url: &Url) -> Result<(Url, Option<String>)> {
        let origin = origin_of(url);
        let Some((_, target)) = self.origin_map.iter().find(|(public, _)| *public == origin) else {
            return Ok((url.clone(), None));
        };
        let mut mapped = target.clone();
        mapped.set_path(url.path());
        mapped.set_query(url.query());
        let host = match (url.host_str(), url.port()) {
            (Some(host), Some(port)) => Some(format!("{host}:{port}")),
            (Some(host), None) => Some(host.to_string()),
            _ => None,
        };
        Ok((mapped, host))
    }

    /// Maps a URL on a fetch origin back to its public origin.
    fn public_url(&self, url: Url) -> Url {
        let origin = origin_of(&url);
        for (public, target) in &self.origin_map {
            if origin_of(target) == origin
                && let Ok(base) = Url::parse(public)
                && let Ok(mapped) = base.join(&url[url::Position::BeforePath..])
            {
                return mapped;
            }
        }
        url
    }

    fn record(&self, url: &Url, allowed: bool, reason: Option<String>) {
        let loopback = match url.host() {
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            Some(url::Host::Domain(name)) => {
                let name = name.trim_end_matches('.').to_ascii_lowercase();
                name == "localhost" || name.ends_with(".localhost")
            }
            None => false,
        };
        let record = ConnectionRecord {
            url: url.to_string(),
            origin: origin_of(url),
            allowed,
            loopback,
            reason,
            at: self.clock.now(),
        };
        if let Ok(mut state) = self.state.lock() {
            state.records.push(record);
        }
    }

    fn check_egress(&self, url: &Url) -> std::result::Result<(), String> {
        match url.scheme() {
            "http" | "https" => {}
            other => return Err(format!("scheme '{other}' is not http or https")),
        }
        match &self.egress {
            EgressScope::Only(origins) => {
                let origin = origin_of(url);
                if origins.contains(&origin) {
                    Ok(())
                } else {
                    Err(format!("origin '{origin}' is not one this client may reach"))
                }
            }
            EgressScope::Public => match url.host() {
                Some(url::Host::Ipv4(ip)) if is_non_public(IpAddr::V4(ip)) => {
                    Err(format!("{ip} is not a public address"))
                }
                Some(url::Host::Ipv6(ip)) if is_non_public(IpAddr::V6(ip)) => {
                    Err(format!("{ip} is not a public address"))
                }
                Some(url::Host::Domain(name))
                    if name.eq_ignore_ascii_case("localhost")
                        || name.to_ascii_lowercase().ends_with(".localhost") =>
                {
                    Err("localhost is not a public address".into())
                }
                Some(_) => Ok(()),
                None => Err("the URL has no host".into()),
            },
        }
    }

    async fn wait_turn(&self, origin: &str) {
        let wait_until = {
            let Ok(mut state) = self.state.lock() else { return };
            let delay =
                state.delays.get(origin).copied().unwrap_or_default().max(self.min_interval);
            let now = tokio::time::Instant::now();
            let slot = state.next_slot.get(origin).copied().unwrap_or(now).max(now);
            state.next_slot.insert(origin.to_string(), slot + delay);
            slot
        };
        tokio::time::sleep_until(wait_until).await;
    }

    async fn send(&self, url: &Url, options: &FetchOptions) -> Result<Fetched> {
        let (target, host) = self.fetch_url(url)?;
        if let Err(reason) = self.check_egress(&target) {
            tracing::warn!(url = %target, reason = %reason, "migration request refused");
            self.record(&target, false, Some(reason.clone()));
            return Err(MigrateError::Egress { url: url.to_string(), reason });
        }
        self.record(&target, true, None);
        self.wait_turn(&origin_of(&target)).await;
        let user_agent = match options.profile {
            FetchProfile::Crawler => self.user_agent.as_str(),
            FetchProfile::Browser => BROWSER_USER_AGENT,
        };
        let mut request = self
            .client
            .get(target.clone())
            .header(reqwest::header::USER_AGENT, user_agent)
            .headers(options.headers.clone())
            .build()
            .map_err(|e| MigrateError::http(url, e))?;
        if let Some(host) = host
            && let Ok(value) = HeaderValue::from_str(&host)
        {
            request.headers_mut().insert(reqwest::header::HOST, value);
        }
        if options.profile == FetchProfile::Crawler
            && let Some(signer) = &self.signer
        {
            signer
                .sign(&mut request)
                .map_err(|e| MigrateError::http(url, format!("signing failed: {e}")))?;
        }
        let mut response =
            self.client.execute(request).await.map_err(|e| MigrateError::http(url, e))?;
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        let peer_certificate = response
            .extensions()
            .get::<reqwest::tls::TlsInfo>()
            .and_then(|info| info.peer_certificate())
            .map(<[u8]>::to_vec);
        let mut body = BytesMut::new();
        let mut truncated = false;
        while let Some(chunk) = response.chunk().await.map_err(|e| MigrateError::http(url, e))? {
            if body.len() + chunk.len() > self.max_body {
                body.extend_from_slice(&chunk[..self.max_body - body.len()]);
                truncated = true;
                break;
            }
            body.extend_from_slice(&chunk);
        }
        let location = headers
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| target.join(v).ok())
            .map(|u| self.public_url(u));
        tracing::debug!(url = %url, status, bytes = body.len(), "migration request");
        Ok(Fetched {
            url: url.clone(),
            hops: Vec::new(),
            status,
            headers,
            body: body.freeze(),
            truncated,
            location,
            peer_certificate,
        })
    }
}

/// Resolves names and drops every non-public address, so a public name cannot be pointed at
/// an internal service.
struct PublicOnlyResolver;

impl reqwest::dns::Resolve for PublicOnlyResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addresses: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .filter(|address| !is_non_public(address.ip()))
                .collect();
            if addresses.is_empty() {
                return Err(format!("'{host}' resolves to no public address").into());
            }
            Ok(Box::new(addresses.into_iter()) as reqwest::dns::Addrs)
        })
    }
}
