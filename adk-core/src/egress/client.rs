//! [`GuardedClient`]: an HTTP client that enforces an [`EgressPolicy`].

use std::fmt;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use super::{EgressDenial, EgressPolicy, EgressTarget, HostResolver, SystemResolver};

/// A `reqwest` resolver that refuses any answer containing an address the policy refuses
/// ([`EgressTarget::Address`]).
///
/// One refused address refuses the whole answer, so a name whose records mix public and
/// internal addresses never connects. Set it with `ClientBuilder::dns_resolver` together with
/// `ClientBuilder::no_proxy`, since a proxy resolves the host itself; [`GuardedClient`] does
/// both, and checks IP literals and redirects, which a resolver never sees.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
///
/// use adk_core::egress::{EgressPolicy, EgressResolver};
///
/// let client = reqwest::Client::builder()
///     .no_proxy()
///     .dns_resolver(Arc::new(EgressResolver::new(EgressPolicy::public_only())))
///     .build()
///     .expect("the client builds");
/// # drop(client);
/// ```
#[derive(Debug, Clone)]
pub struct EgressResolver {
    policy: Arc<EgressPolicy>,
    lookup: Arc<dyn HostResolver>,
}

impl EgressResolver {
    /// A resolver checking the operating system's answers against `policy`.
    pub fn new(policy: EgressPolicy) -> Self {
        Self { policy: Arc::new(policy), lookup: Arc::new(SystemResolver) }
    }

    /// A resolver checking `lookup`'s answers against `policy`.
    pub fn with_lookup(policy: EgressPolicy, lookup: Arc<dyn HostResolver>) -> Self {
        Self { policy: Arc::new(policy), lookup }
    }
}

impl reqwest::dns::Resolve for EgressResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        let policy = Arc::clone(&self.policy);
        let lookup = Arc::clone(&self.lookup);
        Box::pin(async move {
            let addresses = lookup.resolve(&host).await?;
            if addresses.is_empty() {
                let message = format!("{host} resolves to no address");
                return Err(std::io::Error::new(std::io::ErrorKind::NotFound, message).into());
            }
            for &address in &addresses {
                if let Err(denial) = policy.decide(&EgressTarget::Address { host: &host, address })
                {
                    tracing::warn!(
                        egress.host = %host,
                        egress.address = %address,
                        egress.code = denial.code(),
                        "egress refused at resolution"
                    );
                    return Err(denial.into());
                }
            }
            let addresses = addresses.into_iter().map(|address| SocketAddr::new(address, 0));
            Ok(Box::new(addresses) as reqwest::dns::Addrs)
        })
    }
}

/// The guarded client could not be built.
///
/// No unguarded client is ever used in its place.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "the egress-guarded HTTP client could not be built ({reason}), so no request is sent \
     without its egress guard; fix the TLS setup the error names, such as a native root \
     certificate store with no valid certificate"
)]
pub struct EgressClientError {
    /// The underlying build error.
    pub reason: String,
}

/// An HTTP client that enforces an [`EgressPolicy`] on every request.
///
/// | Check | Where |
/// |-------|-------|
/// | Scheme, host name and port, IP literals | Before the request is built ([`EgressPolicy::check_url`]) |
/// | Every address a name resolves to | At connect time, through [`EgressResolver`]: the address checked is the address connected to |
/// | Every redirect hop | Scheme, host, port and literal again, then resolution again |
/// | Proxies | None: environment and system proxy settings are ignored, since a proxy resolves names itself |
///
/// A refusal from the resolver or a redirect surfaces as the request's `reqwest::Error`;
/// [`find_denial`](super::find_denial) recovers the [`EgressDenial`].
///
/// # Example
///
/// ```rust,no_run
/// use adk_core::egress::{EgressPolicy, GuardedClient, find_denial};
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let client = GuardedClient::new(EgressPolicy::allowlist(["api.github.com:443"])?)?;
/// assert!(client.get("https://example.com/").is_err());
/// match client.get("https://api.github.com/zen")?.send().await {
///     Ok(response) => println!("{}", response.status()),
///     Err(error) => match find_denial(&error) {
///         Some(denial) => println!("refused: {denial}"),
///         None => println!("failed: {error}"),
///     },
/// }
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct GuardedClient {
    client: reqwest::Client,
    policy: Arc<EgressPolicy>,
}

impl fmt::Debug for GuardedClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GuardedClient").field("policy", &self.policy.to_string()).finish()
    }
}

impl GuardedClient {
    /// A client with the default options of [`GuardedClientBuilder`].
    ///
    /// # Errors
    ///
    /// Returns [`EgressClientError`] when the TLS stack cannot be set up.
    pub fn new(policy: EgressPolicy) -> Result<Self, EgressClientError> {
        Self::builder(policy).build()
    }

    /// Starts a builder enforcing `policy`.
    pub fn builder(policy: EgressPolicy) -> GuardedClientBuilder {
        GuardedClientBuilder {
            policy,
            lookup: Arc::new(SystemResolver),
            timeout: None,
            connect_timeout: None,
            user_agent: None,
            redirects: 10,
            default_headers: reqwest::header::HeaderMap::new(),
        }
    }

    /// The policy the client enforces.
    pub fn policy(&self) -> &EgressPolicy {
        &self.policy
    }

    /// Starts a `method` request to `url` once the URL passes the policy.
    ///
    /// # Errors
    ///
    /// Returns why the URL is refused.
    pub fn request(
        &self,
        method: reqwest::Method,
        url: &str,
    ) -> Result<reqwest::RequestBuilder, EgressDenial> {
        let url = self.policy.check_url(url)?;
        Ok(self.client.request(method, url))
    }

    /// Starts a `GET` request; see [`request`](Self::request).
    ///
    /// # Errors
    ///
    /// Returns why the URL is refused.
    pub fn get(&self, url: &str) -> Result<reqwest::RequestBuilder, EgressDenial> {
        self.request(reqwest::Method::GET, url)
    }

    /// Starts a `POST` request; see [`request`](Self::request).
    ///
    /// # Errors
    ///
    /// Returns why the URL is refused.
    pub fn post(&self, url: &str) -> Result<reqwest::RequestBuilder, EgressDenial> {
        self.request(reqwest::Method::POST, url)
    }
}

/// Builds a [`GuardedClient`].
///
/// # Example
///
/// ```rust
/// use std::time::Duration;
///
/// use adk_core::egress::{EgressPolicy, GuardedClient};
///
/// let client = GuardedClient::builder(EgressPolicy::public_only())
///     .timeout(Duration::from_secs(10))
///     .redirects(0)
///     .user_agent("my-agent/1.0")
///     .build()
///     .expect("the client builds");
/// assert!(client.get("https://10.0.0.1/").is_err());
/// ```
pub struct GuardedClientBuilder {
    policy: EgressPolicy,
    lookup: Arc<dyn HostResolver>,
    timeout: Option<Duration>,
    connect_timeout: Option<Duration>,
    user_agent: Option<String>,
    redirects: usize,
    default_headers: reqwest::header::HeaderMap,
}

impl fmt::Debug for GuardedClientBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GuardedClientBuilder")
            .field("policy", &self.policy.to_string())
            .field("lookup", &self.lookup)
            .field("timeout", &self.timeout)
            .field("connect_timeout", &self.connect_timeout)
            .field("redirects", &self.redirects)
            .finish()
    }
}

impl GuardedClientBuilder {
    /// Resolves names through `lookup` instead of the operating system.
    #[must_use]
    pub fn resolver(mut self, lookup: Arc<dyn HostResolver>) -> Self {
        self.lookup = lookup;
        self
    }

    /// Bounds one request, from connect to the end of the body.
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Bounds connection setup.
    #[must_use]
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = Some(timeout);
        self
    }

    /// Sets the `User-Agent` header.
    #[must_use]
    pub fn user_agent(mut self, user_agent: impl Into<String>) -> Self {
        self.user_agent = Some(user_agent.into());
        self
    }

    /// Follows at most `redirects` redirects, each checked (default 10); `0` returns redirect
    /// responses to the caller unfollowed.
    #[must_use]
    pub fn redirects(mut self, redirects: usize) -> Self {
        self.redirects = redirects;
        self
    }

    /// Sends `headers` with every request.
    #[must_use]
    pub fn default_headers(mut self, headers: reqwest::header::HeaderMap) -> Self {
        self.default_headers = headers;
        self
    }

    /// Builds the client.
    ///
    /// # Errors
    ///
    /// Returns [`EgressClientError`] when the TLS stack cannot be set up.
    pub fn build(self) -> Result<GuardedClient, EgressClientError> {
        let policy = Arc::new(self.policy);
        let redirect = match self.redirects {
            0 => reqwest::redirect::Policy::none(),
            max => {
                let policy = Arc::clone(&policy);
                reqwest::redirect::Policy::custom(move |attempt| {
                    if attempt.previous().len() > max {
                        let message = format!("more than {max} redirects");
                        return attempt.error(message);
                    }
                    match policy.check_parsed_url(attempt.url()) {
                        Ok(()) => attempt.follow(),
                        Err(denial) => {
                            tracing::warn!(
                                egress.code = denial.code(),
                                error = %denial,
                                "egress refused a redirect"
                            );
                            attempt.error(denial)
                        }
                    }
                })
            }
        };
        let resolver = EgressResolver { policy: Arc::clone(&policy), lookup: self.lookup };
        let mut builder = reqwest::Client::builder()
            .no_proxy()
            .dns_resolver(Arc::new(resolver))
            .redirect(redirect)
            .default_headers(self.default_headers);
        if let Some(timeout) = self.timeout {
            builder = builder.timeout(timeout);
        }
        if let Some(timeout) = self.connect_timeout {
            builder = builder.connect_timeout(timeout);
        }
        if let Some(user_agent) = self.user_agent {
            builder = builder.user_agent(user_agent);
        }
        let client = builder.build().map_err(|error| {
            let mut reason = error.to_string();
            let mut source = std::error::Error::source(&error);
            while let Some(cause) = source {
                reason.push_str(": ");
                reason.push_str(&cause.to_string());
                source = cause.source();
            }
            tracing::error!(error = %reason, "egress-guarded client failed to build");
            EgressClientError { reason }
        })?;
        Ok(GuardedClient { client, policy })
    }
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;
    use crate::egress::{AddressClass, StaticResolver, find_denial};

    /// Answers each request with the response `route` gives its path; returns the port.
    async fn serve(route: fn(&str, u16) -> String) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut buffer = [0u8; 1024];
                    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                        match stream.read(&mut buffer).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => request.extend_from_slice(&buffer[..n]),
                        }
                    }
                    let text = String::from_utf8_lossy(&request);
                    let path = text.split_whitespace().nth(1).unwrap_or("/").to_string();
                    let _ = stream.write_all(route(&path, port).as_bytes()).await;
                });
            }
        });
        port
    }

    fn redirect_to(location: &str) -> String {
        format!("HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\n\r\n")
    }

    fn route(path: &str, port: u16) -> String {
        match path {
            "/to-other" => redirect_to(&format!("http://other.test:{port}/ok")),
            "/to-metadata" => redirect_to("http://169.254.169.254/latest/meta-data"),
            "/to-literal" => redirect_to(&format!("http://127.0.0.1:{port}/ok")),
            "/to-self" => redirect_to(&format!("http://app.test:{port}/ok")),
            _ => "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_string(),
        }
    }

    fn local_client() -> GuardedClient {
        let loopback: IpAddr = "127.0.0.1".parse().unwrap();
        // Names may resolve to loopback; the literal is admitted on port 1 only.
        let policy =
            EgressPolicy::allowlist(["app.test", "127.0.0.1:1"]).unwrap().with_plain_http();
        let resolver = StaticResolver::new()
            .with_host("app.test", [loopback])
            .with_host("other.test", [loopback]);
        GuardedClient::builder(policy).resolver(Arc::new(resolver)).build().unwrap()
    }

    #[tokio::test]
    async fn an_admitted_name_reaches_its_checked_address() {
        let port = serve(route).await;
        let client = local_client();
        let response =
            client.get(&format!("http://app.test:{port}/to-self")).unwrap().send().await.unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.text().await.unwrap(), "ok");
    }

    #[tokio::test]
    async fn every_redirect_hop_is_checked() {
        let port = serve(route).await;
        let client = local_client();
        let cases = [
            ("/to-other", "egress.host_not_allowed"),
            ("/to-metadata", "egress.metadata_address"),
            ("/to-literal", "egress.literal_not_allowed"),
        ];
        for (path, code) in cases {
            let error = client
                .get(&format!("http://app.test:{port}{path}"))
                .unwrap()
                .send()
                .await
                .expect_err(path);
            let denial = find_denial(&error).unwrap_or_else(|| panic!("{path}: {error:?}"));
            assert_eq!(denial.code(), code, "{path}");
        }
    }

    #[tokio::test]
    async fn names_resolving_to_refused_addresses_never_connect() {
        let resolver = StaticResolver::new()
            .with_host("internal.test", ["10.0.0.1".parse().unwrap()])
            .with_host(
                "mixed.test",
                ["93.184.216.34".parse().unwrap(), "127.0.0.1".parse().unwrap()],
            )
            .with_host("metadata.test", ["169.254.169.254".parse().unwrap()]);
        let client = GuardedClient::builder(EgressPolicy::public_only())
            .resolver(Arc::new(resolver))
            .build()
            .unwrap();
        for (host, expected) in
            [("internal.test", AddressClass::Private), ("mixed.test", AddressClass::Loopback)]
        {
            let error =
                client.get(&format!("https://{host}/")).unwrap().send().await.expect_err(host);
            match find_denial(&error) {
                Some(EgressDenial::AddressNotAllowed { class, .. }) => assert_eq!(*class, expected),
                other => panic!("{host}: {other:?} from {error:?}"),
            }
        }
        let error = client.get("https://metadata.test/").unwrap().send().await.unwrap_err();
        assert!(matches!(find_denial(&error), Some(EgressDenial::MetadataAddress { .. })));
    }

    #[test]
    fn refused_urls_build_no_request() {
        let client = GuardedClient::new(EgressPolicy::allowlist(["api.example.com"]).unwrap())
            .expect("the client builds");
        for url in [
            "https://10.0.0.1/",
            "https://93.184.216.34/",
            "http://api.example.com/",
            "https://other.example.com/",
            "file:///etc/passwd",
        ] {
            assert!(client.get(url).is_err(), "{url}");
        }
        assert!(client.get("https://api.example.com/").is_ok());
    }
}
