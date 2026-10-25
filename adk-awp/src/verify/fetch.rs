//! Passive fetching for the verifier and the conformance kit: `GET` only (spec 3.5 §7.7,
//! A-I9; AWP 2.0 §15.1).

use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, Request, header};
use bytes::Bytes;
use tower::util::ServiceExt;

/// The largest response the verifier reads.
pub const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

/// A fetched response.
#[derive(Debug, Clone)]
pub struct Fetched {
    /// The HTTP status.
    pub status: u16,
    /// The response headers.
    pub headers: HeaderMap,
    /// The body.
    pub body: Bytes,
}

impl Fetched {
    /// The `Content-Type` without parameters, lowercased.
    pub fn media_type(&self) -> Option<String> {
        self.headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.split(';').next().unwrap_or_default().trim().to_ascii_lowercase())
    }

    /// The value of header `name`, if it is text.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|value| value.to_str().ok())
    }
}

/// Why a fetch failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("GET {url} failed: {reason}")]
pub struct FetchError {
    /// The URL.
    pub url: String,
    /// Why.
    pub reason: String,
}

/// Sends `GET` requests; the verifier and the conformance kit never send anything else (A-I9).
#[async_trait]
pub trait Fetcher: Send + Sync {
    /// Fetches `url` with `GET`, adding `headers`.
    ///
    /// # Errors
    ///
    /// [`FetchError`] when the request cannot be made; an HTTP error status is a response.
    async fn get(&self, url: &str, headers: &HeaderMap) -> Result<Fetched, FetchError>;
}

/// Signs outgoing requests, such as with Web Bot Auth (spec 3.5 §7.3 "Outbound").
#[async_trait]
pub trait RequestSigner: Send + Sync {
    /// Adds signature headers for a `GET` of `url` to `headers`.
    ///
    /// # Errors
    ///
    /// A message when the request cannot be signed.
    async fn sign(&self, url: &str, headers: &mut HeaderMap) -> Result<(), String>;
}

/// Fetches over HTTPS with `reqwest`, following no redirects, optionally signing each
/// request.
#[derive(Clone)]
pub struct HttpFetcher {
    client: reqwest::Client,
    signer: Option<Arc<dyn RequestSigner>>,
}

impl std::fmt::Debug for HttpFetcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpFetcher").field("signed", &self.signer.is_some()).finish()
    }
}

impl HttpFetcher {
    /// Creates a fetcher with a 30-second timeout that follows no redirects.
    ///
    /// # Errors
    ///
    /// [`FetchError`] when the client cannot be built.
    pub fn new() -> Result<Self, FetchError> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(30))
            .user_agent(concat!("adk-awp-verify/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| FetchError { url: String::new(), reason: error.to_string() })?;
        Ok(Self { client, signer: None })
    }

    /// Signs every request with `signer`.
    pub fn with_signer(mut self, signer: Arc<dyn RequestSigner>) -> Self {
        self.signer = Some(signer);
        self
    }
}

#[async_trait]
impl Fetcher for HttpFetcher {
    async fn get(&self, url: &str, headers: &HeaderMap) -> Result<Fetched, FetchError> {
        let failed = |reason: String| FetchError { url: url.to_owned(), reason };
        let mut headers = headers.clone();
        if let Some(signer) = &self.signer {
            signer.sign(url, &mut headers).await.map_err(failed)?;
        }
        let response = self
            .client
            .get(url)
            .headers(headers)
            .send()
            .await
            .map_err(|error| failed(error.to_string()))?;
        let status = response.status().as_u16();
        let mut response_headers = HeaderMap::new();
        for (name, value) in response.headers() {
            response_headers.append(name.clone(), value.clone());
        }
        let body = response.bytes().await.map_err(|error| failed(error.to_string()))?;
        if body.len() > MAX_RESPONSE_BYTES {
            return Err(failed("the response is too large".to_owned()));
        }
        Ok(Fetched { status, headers: response_headers, body })
    }
}

/// Fetches from an in-process Axum router, for tests and conformance runs against a router:
/// the URL's path and query go to the router, its host to the `Host` header.
#[derive(Clone)]
pub struct RouterFetcher {
    router: Router,
}

impl std::fmt::Debug for RouterFetcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RouterFetcher").finish_non_exhaustive()
    }
}

impl RouterFetcher {
    /// Fetches from `router`.
    pub fn new(router: Router) -> Self {
        Self { router }
    }
}

#[async_trait]
impl Fetcher for RouterFetcher {
    async fn get(&self, url: &str, headers: &HeaderMap) -> Result<Fetched, FetchError> {
        let failed = |reason: &str| FetchError { url: url.to_owned(), reason: reason.to_owned() };
        let rest = url.strip_prefix("https://").ok_or_else(|| failed("only https URLs"))?;
        let (host, path) = rest
            .split_once('/')
            .map_or((rest, "/".to_owned()), |(host, path)| (host, format!("/{path}")));
        let mut request = Request::get(path).body(Body::empty()).map_err(|_| failed("bad URL"))?;
        *request.headers_mut() = headers.clone();
        let host = HeaderValue::from_str(host).map_err(|_| failed("bad host"))?;
        request.headers_mut().insert(header::HOST, host);
        let response =
            self.router.clone().oneshot(request).await.map_err(|_| failed("router failed"))?;
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        let body = axum::body::to_bytes(response.into_body(), MAX_RESPONSE_BYTES)
            .await
            .map_err(|_| failed("the response is too large"))?;
        Ok(Fetched { status, headers, body })
    }
}
