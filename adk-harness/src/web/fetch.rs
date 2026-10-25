//! The governed fetch: one plain `GET` through the harness's own client, with the target
//! resolved and classified before connecting and at every redirect (spec 3.1 §7.20,
//! §9.2 I46, I48).

use std::fmt;
use std::net::SocketAddr;
use std::sync::Arc;

use futures::StreamExt as _;
use sha2::{Digest, Sha256};

use super::WebConfig;
use super::domain::{ParsedUrl, UrlRefusal, host_allowlisted, is_private_host, is_private_ip};
use super::html::{TextShape, html_to_text};
use super::screen::has_secret;

/// Most bytes of text delivered to the model; the rest is cut with a marker
/// (spec 3.1 §7.20 `WebConfig::max_bytes`).
pub const DELIVERED_TEXT_MAX: usize = 64 * 1024;
/// Most bytes a composed URL may carry outside its path (spec 3.1 §9.2 I48).
pub const NON_PATH_BUDGET: usize = 256;
/// Appended to text cut at [`DELIVERED_TEXT_MAX`].
pub const TRUNCATION_MARKER: &str = "\n[truncated: the page continues beyond the delivered text]";

/// Why a fetch did not happen or did not complete (spec 3.1 §7.20).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FetchRefusal {
    /// The URL breaks the grammar.
    Url(UrlRefusal),
    /// The URL is longer than `max_url_len`.
    UrlTooLong {
        /// The URL's length.
        len: usize,
        /// The cap.
        max: usize,
    },
    /// The query or fragment matches a secret pattern.
    SecretInQuery,
    /// The query and fragment exceed [`NON_PATH_BUDGET`].
    NonPathBudget {
        /// The bytes outside the path.
        bytes: usize,
        /// The budget.
        max: usize,
    },
    /// The host is, or resolves to, a loopback, link-local, private or mapped address.
    PrivateTarget {
        /// The host named.
        host: String,
        /// The address it resolved to, when resolution found it.
        address: Option<String>,
    },
    /// The host did not resolve.
    Unresolvable {
        /// The host named.
        host: String,
    },
    /// The request exceeded `timeout`.
    Timeout {
        /// The timeout in seconds.
        secs: u64,
    },
    /// The request failed below HTTP.
    Transport(String),
    /// More than `max_redirects` were followed.
    TooManyRedirects {
        /// The cap.
        max: u8,
    },
    /// A redirect status without a `Location` header.
    RedirectWithoutLocation {
        /// The status.
        status: u16,
    },
    /// A content type the tool does not deliver.
    UnsupportedType {
        /// The mime type.
        content_type: String,
    },
}

impl FetchRefusal {
    /// The error code of the refusal.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Url(_) => "harness.web.url",
            Self::UrlTooLong { .. } => "harness.web.url_too_long",
            Self::SecretInQuery => "harness.web.secret_in_query",
            Self::NonPathBudget { .. } => "harness.web.non_path_budget",
            Self::PrivateTarget { .. } => "harness.web.private_target",
            Self::Unresolvable { .. } => "harness.web.unresolvable",
            Self::Timeout { .. } => "harness.web.timeout",
            Self::Transport(_) => "harness.web.transport",
            Self::TooManyRedirects { .. } => "harness.web.redirects",
            Self::RedirectWithoutLocation { .. } => "harness.web.redirects",
            Self::UnsupportedType { .. } => "harness.web.unsupported_type",
        }
    }

    /// The `refusal` value of the `harness.web.fetched` record (spec 3.1 §9.3).
    pub fn audit_refusal(&self) -> &'static str {
        match self {
            Self::PrivateTarget { .. } => "privateTarget",
            Self::UrlTooLong { .. } => "urlTooLong",
            Self::SecretInQuery | Self::NonPathBudget { .. } => "secretInQuery",
            Self::Url(_) => "url",
            Self::Unresolvable { .. } => "unresolvable",
            Self::Timeout { .. } => "timeout",
            Self::Transport(_) => "transport",
            Self::TooManyRedirects { .. } | Self::RedirectWithoutLocation { .. } => "redirects",
            Self::UnsupportedType { .. } => "unsupportedType",
        }
    }

    /// Whether the record may carry the URL: a refusal about the URL's own content
    /// carries only its digest (spec 3.1 §9.2 I48).
    pub fn url_is_loggable(&self) -> bool {
        !matches!(self, Self::UrlTooLong { .. } | Self::SecretInQuery | Self::NonPathBudget { .. })
    }
}

impl fmt::Display for FetchRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Url(refusal) => write!(f, "{refusal}"),
            Self::UrlTooLong { len, max } => {
                write!(f, "the URL is {len} bytes long; the harness fetches URLs up to {max}")
            }
            Self::SecretInQuery => f.write_str(
                "the URL's query or fragment matches a secret pattern, so it is not sent; \
                 fetch the page without the credential",
            ),
            Self::NonPathBudget { bytes, max } => write!(
                f,
                "the URL carries {bytes} bytes outside its path; the harness sends at most {max} \
                 bytes of query and fragment"
            ),
            Self::PrivateTarget { host, address: Some(address) } => write!(
                f,
                "'{host}' resolves to {address}, a private or local address the harness never \
                 fetches"
            ),
            Self::PrivateTarget { host, address: None } => {
                write!(f, "'{host}' is a private or local target the harness never fetches")
            }
            Self::Unresolvable { host } => write!(f, "'{host}' did not resolve"),
            Self::Timeout { secs } => write!(f, "the request did not complete within {secs} s"),
            Self::Transport(message) => write!(f, "the request failed: {message}"),
            Self::TooManyRedirects { max } => {
                write!(f, "the page redirected more than {max} times")
            }
            Self::RedirectWithoutLocation { status } => {
                write!(f, "the server answered {status} without a Location header")
            }
            Self::UnsupportedType { content_type } => write!(
                f,
                "the response is '{content_type}', which the harness does not deliver; it \
                 delivers text, HTML, JSON and PDF"
            ),
        }
    }
}

impl std::error::Error for FetchRefusal {}

impl From<UrlRefusal> for FetchRefusal {
    fn from(refusal: UrlRefusal) -> Self {
        Self::Url(refusal)
    }
}

/// What a fetch delivered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FetchBody {
    /// Text, HTML converted to text, or JSON.
    Text(String),
    /// A PDF: the bytes were read and hashed; the content is a stub until a vision path
    /// exists (spec 3.1 §7.20 "Content").
    Pdf,
    /// A redirect to another host, left for a new call (spec 3.1 §7.20 "Redirects").
    Redirect {
        /// The redirect target.
        to: ParsedUrl,
        /// Whether the target's host is in the allowlist.
        allowlisted: bool,
    },
}

/// A completed fetch (spec 3.1 §7.20 "Delivery").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fetched {
    /// The URL asked for.
    pub url: ParsedUrl,
    /// The URL that answered, after same-host and allowlisted redirects.
    pub final_url: ParsedUrl,
    /// The HTTP status.
    pub status: u16,
    /// The mime type without parameters.
    pub content_type: String,
    /// Hex SHA-256 of the bytes read.
    pub sha256: String,
    /// Bytes read, after the wire cap.
    pub bytes: usize,
    /// Whether the body was cut on the wire or the text at delivery.
    pub truncated: bool,
    /// Redirects followed.
    pub redirects: u8,
    /// The delivered content.
    pub body: FetchBody,
}

/// Signs each request of the governed fetch before it is sent, again at every redirect hop,
/// since the authority and path change: the site's Web Bot Auth key under feature `website`
/// (spec 3.5 §7.3 "Outbound").
///
/// # Example
///
/// ```rust
/// use adk_harness::web::RequestSigner;
///
/// #[derive(Debug)]
/// struct Tag;
///
/// impl RequestSigner for Tag {
///     fn sign(&self, request: &mut reqwest::Request) -> Result<(), String> {
///         request.headers_mut().insert("x-signed", "1".parse().unwrap());
///         Ok(())
///     }
/// }
/// ```
pub trait RequestSigner: fmt::Debug + Send + Sync {
    /// Adds the signature fields to `request`.
    ///
    /// # Errors
    ///
    /// Returns why `request` cannot be signed; the fetch is then refused, never sent unsigned.
    fn sign(&self, request: &mut reqwest::Request) -> Result<(), String>;
}

/// The harness's fetch client (spec 3.1 §7.20).
///
/// # Example
///
/// ```rust
/// use adk_harness::web::{FetchRefusal, Fetcher, WebConfig};
///
/// let fetcher = Fetcher::new(WebConfig::default());
/// assert!(matches!(
///     fetcher.check_url("http://169.254.169.254/latest/meta-data").unwrap_err(),
///     FetchRefusal::PrivateTarget { .. }
/// ));
/// assert_eq!(
///     fetcher.check_url("https://docs.rs/?token=AKIAIOSFODNN7EXAMPLE").unwrap_err(),
///     FetchRefusal::SecretInQuery
/// );
/// assert!(fetcher.check_url("https://docs.rs/crate/adk-core").is_ok());
/// ```
#[derive(Clone, Debug)]
pub struct Fetcher {
    config: WebConfig,
    signer: Option<Arc<dyn RequestSigner>>,
}

impl Fetcher {
    /// A fetcher under `config`.
    pub fn new(config: WebConfig) -> Self {
        Self { config, signer: None }
    }

    /// Signs every request, every redirect hop included, with `signer` (spec 3.5 §7.3
    /// "Outbound").
    #[must_use]
    pub fn with_signer(mut self, signer: Arc<dyn RequestSigner>) -> Self {
        self.signer = Some(signer);
        self
    }

    /// The configuration.
    pub fn config(&self) -> &WebConfig {
        &self.config
    }

    /// Checks `url` before any request: grammar, length, non-path budget, secret
    /// patterns, and a private literal or name (spec 3.1 §9.2 I46, I48).
    ///
    /// # Errors
    ///
    /// Returns the first [`FetchRefusal`] the URL earns.
    pub fn check_url(&self, url: &str) -> Result<ParsedUrl, FetchRefusal> {
        if url.len() > self.config.max_url_len {
            return Err(FetchRefusal::UrlTooLong { len: url.len(), max: self.config.max_url_len });
        }
        let parsed = ParsedUrl::parse(url)?;
        let non_path = parsed.non_path_bytes();
        if non_path > NON_PATH_BUDGET {
            return Err(FetchRefusal::NonPathBudget { bytes: non_path, max: NON_PATH_BUDGET });
        }
        let tail = format!(
            "{} {}",
            parsed.query.as_deref().unwrap_or_default(),
            parsed.fragment.as_deref().unwrap_or_default()
        );
        if has_secret(&tail) || has_secret(&percent_decode(&tail)) {
            return Err(FetchRefusal::SecretInQuery);
        }
        if is_private_host(&parsed.host) {
            return Err(FetchRefusal::PrivateTarget { host: parsed.host.clone(), address: None });
        }
        Ok(parsed)
    }

    /// Resolves `url`'s host and refuses it when any address is private
    /// (spec 3.1 §9.2 I46).
    ///
    /// # Errors
    ///
    /// `PrivateTarget` for a private address, `Unresolvable` when nothing resolves.
    pub async fn resolve(&self, url: &ParsedUrl) -> Result<Vec<SocketAddr>, FetchRefusal> {
        if let Some(ip) = url.literal() {
            if is_private_ip(ip)
                && !(ip.is_loopback() && super::domain::fixture_loopback_permitted())
            {
                return Err(FetchRefusal::PrivateTarget {
                    host: url.host.clone(),
                    address: Some(ip.to_string()),
                });
            }
            return Ok(vec![SocketAddr::new(ip, url.port)]);
        }
        let lookup = tokio::time::timeout(
            self.config.timeout,
            tokio::net::lookup_host((url.host.as_str(), url.port)),
        )
        .await
        .map_err(|_| FetchRefusal::Timeout { secs: self.config.timeout.as_secs() })?
        .map_err(|_| FetchRefusal::Unresolvable { host: url.host.clone() })?;
        let addresses: Vec<SocketAddr> = lookup.collect();
        if addresses.is_empty() {
            return Err(FetchRefusal::Unresolvable { host: url.host.clone() });
        }
        let permitted = super::domain::fixture_loopback_permitted();
        if let Some(private) = addresses.iter().find(|address| {
            is_private_ip(address.ip()) && !(permitted && address.ip().is_loopback())
        }) {
            return Err(FetchRefusal::PrivateTarget {
                host: url.host.clone(),
                address: Some(private.ip().to_string()),
            });
        }
        Ok(addresses)
    }

    /// Fetches `url` with one `GET` per hop, following only same-host and allowlisted
    /// redirects, reading at most `max_bytes` (never more than the configured cap).
    ///
    /// # Errors
    ///
    /// Returns the [`FetchRefusal`] of the URL, the resolution, the transport or the
    /// content type.
    pub async fn fetch(
        &self,
        url: &str,
        max_bytes: Option<usize>,
        shape: TextShape,
        allowlist: &[String],
    ) -> Result<Fetched, FetchRefusal> {
        let wire_cap = max_bytes.unwrap_or(self.config.max_bytes).min(self.config.max_bytes).max(1);
        let requested = self.check_url(url)?;
        let mut current = requested.clone();
        let mut redirects = 0u8;
        loop {
            let addresses = self.resolve(&current).await?;
            let response = self.get(&current, &addresses).await?;
            let status = response.status().as_u16();
            if (300..400).contains(&status) && status != 304 {
                let location = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or(FetchRefusal::RedirectWithoutLocation { status })?;
                let target = current.join(location)?;
                self.check_url(&target.as_string())?;
                let allowlisted = host_allowlisted(&target.host, allowlist);
                if target.host != current.host && !allowlisted {
                    return Ok(Fetched {
                        url: requested,
                        final_url: current,
                        status,
                        content_type: String::new(),
                        sha256: hex_sha256(&[]),
                        bytes: 0,
                        truncated: false,
                        redirects,
                        body: FetchBody::Redirect { to: target, allowlisted },
                    });
                }
                if redirects >= self.config.max_redirects {
                    return Err(FetchRefusal::TooManyRedirects { max: self.config.max_redirects });
                }
                redirects += 1;
                current = target;
                continue;
            }
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .map(|value| {
                    value.split(';').next().unwrap_or_default().trim().to_ascii_lowercase()
                })
                .unwrap_or_else(|| "application/octet-stream".to_string());
            let (bytes, cut_on_wire) = self.read_body(response, wire_cap).await?;
            let sha256 = hex_sha256(&bytes);
            let kind = classify(&content_type);
            let body = match kind {
                Kind::Html => {
                    FetchBody::Text(html_to_text(&String::from_utf8_lossy(&bytes), shape))
                }
                Kind::Text => FetchBody::Text(String::from_utf8_lossy(&bytes).into_owned()),
                Kind::Pdf => FetchBody::Pdf,
                Kind::Unsupported => return Err(FetchRefusal::UnsupportedType { content_type }),
            };
            let mut truncated = cut_on_wire;
            let body = match body {
                FetchBody::Text(text) if text.len() > DELIVERED_TEXT_MAX => {
                    truncated = true;
                    let end = text.floor_char_boundary(DELIVERED_TEXT_MAX);
                    FetchBody::Text(format!("{}{TRUNCATION_MARKER}", &text[..end]))
                }
                other => other,
            };
            return Ok(Fetched {
                url: requested,
                final_url: current,
                status,
                content_type,
                sha256,
                bytes: bytes.len(),
                truncated,
                redirects,
                body,
            });
        }
    }

    async fn get(
        &self,
        url: &ParsedUrl,
        addresses: &[SocketAddr],
    ) -> Result<reqwest::Response, FetchRefusal> {
        // One client per request: redirects are the tool's, there is no cookie store, no
        // proxy from the environment, and the connection goes only to the vetted
        // addresses (I46).
        let mut builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .timeout(self.config.timeout)
            .connect_timeout(self.config.timeout)
            .user_agent(self.config.user_agent.clone());
        if url.literal().is_none() {
            builder = builder.resolve_to_addrs(&url.host, addresses);
        }
        let client = builder.build().map_err(|error| FetchRefusal::Transport(error.to_string()))?;
        let mut request = client
            .get(url.as_string())
            .header(
                reqwest::header::ACCEPT,
                "text/html, application/xhtml+xml, text/*;q=0.9, application/json;q=0.8, \
                 application/pdf;q=0.5, */*;q=0.1",
            )
            .build()
            .map_err(|error| FetchRefusal::Transport(error.without_url().to_string()))?;
        if let Some(signer) = &self.signer {
            signer.sign(&mut request).map_err(|reason| {
                FetchRefusal::Transport(format!("the request could not be signed: {reason}"))
            })?;
        }
        client.execute(request).await.map_err(|error| {
            if error.is_timeout() {
                FetchRefusal::Timeout { secs: self.config.timeout.as_secs() }
            } else {
                FetchRefusal::Transport(error.without_url().to_string())
            }
        })
    }

    async fn read_body(
        &self,
        response: reqwest::Response,
        cap: usize,
    ) -> Result<(Vec<u8>, bool), FetchRefusal> {
        let mut bytes = Vec::with_capacity(cap.min(256 * 1024));
        let mut stream = response.bytes_stream();
        let mut cut = false;
        let read = async {
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|error| {
                    if error.is_timeout() {
                        FetchRefusal::Timeout { secs: self.config.timeout.as_secs() }
                    } else {
                        FetchRefusal::Transport(error.without_url().to_string())
                    }
                })?;
                if bytes.len() + chunk.len() > cap {
                    bytes.extend_from_slice(&chunk[..cap - bytes.len()]);
                    cut = true;
                    break;
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok::<(), FetchRefusal>(())
        };
        tokio::time::timeout(self.config.timeout, read)
            .await
            .map_err(|_| FetchRefusal::Timeout { secs: self.config.timeout.as_secs() })??;
        Ok((bytes, cut))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Html,
    Text,
    Pdf,
    Unsupported,
}

fn classify(content_type: &str) -> Kind {
    match content_type {
        "text/html" | "application/xhtml+xml" => Kind::Html,
        "application/pdf" => Kind::Pdf,
        "application/json" | "application/xml" | "application/javascript" => Kind::Text,
        other if other.starts_with("text/") => Kind::Text,
        other if other.ends_with("+json") || other.ends_with("+xml") => Kind::Text,
        _ => Kind::Unsupported,
    }
}

/// Hex SHA-256 of `bytes`.
pub fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Decodes `%XX` escapes and `+` in `text`, so a secret sent percent-encoded is still
/// seen; invalid escapes are kept as they are.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => match u8::from_str_radix(&text[i + 1..i + 3], 16) {
                Ok(byte) => {
                    out.push(byte);
                    i += 3;
                    continue;
                }
                Err(_) => out.push(b'%'),
            },
            b'+' => out.push(b' '),
            byte => out.push(byte),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_url_applies_every_pre_request_rule() {
        let fetcher = Fetcher::new(WebConfig { max_url_len: 64, ..WebConfig::default() });
        assert!(matches!(
            fetcher.check_url(&format!("https://docs.rs/{}", "a".repeat(80))).unwrap_err(),
            FetchRefusal::UrlTooLong { len: 96, max: 64 }
        ));
        let fetcher = Fetcher::new(WebConfig::default());
        assert!(matches!(fetcher.check_url("ftp://docs.rs/").unwrap_err(), FetchRefusal::Url(_)));
        let long_query = format!("https://docs.rs/?q={}", "b".repeat(300));
        assert!(matches!(
            fetcher.check_url(&long_query).unwrap_err(),
            FetchRefusal::NonPathBudget { bytes: 302, max: 256 }
        ));
        assert_eq!(
            fetcher.check_url("https://docs.rs/#Bearer%20abcdefghijklmnopqrstuvwxyz").unwrap_err(),
            FetchRefusal::SecretInQuery
        );
        assert_eq!(
            fetcher
                .check_url("https://docs.rs/path?x=1#ghp_abcdefghijklmnopqrstuvwxyz0123456789ABCD")
                .unwrap_err(),
            FetchRefusal::SecretInQuery
        );
        for private in ["http://localhost:3000/", "http://10.0.0.5/", "http://[fd00::1]/"] {
            assert!(
                matches!(
                    fetcher.check_url(private).unwrap_err(),
                    FetchRefusal::PrivateTarget { .. }
                ),
                "{private}"
            );
        }
        assert!(fetcher.check_url("https://docs.rs/crate/adk-core?version=3.0").is_ok());
    }

    #[test]
    fn content_types_classify_as_the_spec_lists() {
        assert_eq!(classify("text/html"), Kind::Html);
        assert_eq!(classify("text/plain"), Kind::Text);
        assert_eq!(classify("text/markdown"), Kind::Text);
        assert_eq!(classify("application/json"), Kind::Text);
        assert_eq!(classify("application/ld+json"), Kind::Text);
        assert_eq!(classify("application/pdf"), Kind::Pdf);
        assert_eq!(classify("image/png"), Kind::Unsupported);
        assert_eq!(classify("application/octet-stream"), Kind::Unsupported);
    }

    #[tokio::test]
    async fn resolve_refuses_private_literals_and_private_resolutions() {
        let fetcher = Fetcher::new(WebConfig::default());
        let literal = ParsedUrl::parse("http://127.0.0.1:8080/").unwrap();
        assert!(matches!(
            fetcher.resolve(&literal).await.unwrap_err(),
            FetchRefusal::PrivateTarget { address: Some(_), .. }
        ));
        // `localhost` is refused by name before resolution; a name that resolves to
        // loopback is refused after it.
        let loopback_name = ParsedUrl::parse("http://localhost./").unwrap();
        assert!(matches!(
            fetcher.resolve(&loopback_name).await.unwrap_err(),
            FetchRefusal::PrivateTarget { .. }
        ));
        let public = ParsedUrl::parse("http://93.184.216.34/").unwrap();
        assert_eq!(fetcher.resolve(&public).await.unwrap().len(), 1);
    }
}
