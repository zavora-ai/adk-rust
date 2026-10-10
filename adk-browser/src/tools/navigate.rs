//! Navigate tool for browser navigation.

use crate::session::BrowserSession;
use adk_core::{Result, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;

/// URL schemes the navigation tools accept unless configured otherwise.
///
/// `file:`, `javascript:`, `data:`, and browser-internal schemes such as `chrome:`
/// are excluded: they read the host filesystem, run script, or reach browser
/// settings rather than load a web page.
pub const DEFAULT_ALLOWED_SCHEMES: &[&str] = &["http", "https"];

/// The default scheme allowlist as owned strings.
pub(crate) fn default_allowed_schemes() -> Vec<String> {
    DEFAULT_ALLOWED_SCHEMES.iter().map(|scheme| (*scheme).to_string()).collect()
}

/// Host names that reach the local machine or a cloud metadata service without resolving to a
/// public address.
const PRIVATE_HOST_NAMES: &[&str] = &["localhost", "metadata.google.internal"];

/// How long a navigation waits for the preflight DNS lookup.
const RESOLVE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// What a model-supplied URL may point at.
#[derive(Debug, Clone)]
pub(crate) struct UrlPolicy {
    pub(crate) allowed_schemes: Vec<String>,
    pub(crate) allow_private_network: bool,
}

impl Default for UrlPolicy {
    fn default() -> Self {
        Self { allowed_schemes: default_allowed_schemes(), allow_private_network: false }
    }
}

impl UrlPolicy {
    /// Rejects a URL that does not parse, whose scheme is not allowed, or — unless private
    /// network access is enabled — whose host is loopback, private, link-local, or a cloud
    /// metadata endpoint.
    ///
    /// A host name is resolved and refused when any address it resolves to is non-public. A name
    /// that does not resolve here is let through, because the browser may resolve it differently
    /// (through a proxy, say); the browser's own redirects and subresources are not covered, so
    /// isolate the browser's network for a hard boundary.
    pub(crate) async fn check(&self, url: &str) -> Result<()> {
        let parsed = url::Url::parse(url)
            .map_err(|e| adk_core::AdkError::tool(format!("Invalid URL '{url}': {e}")))?;
        let scheme = parsed.scheme();
        if !self.allowed_schemes.iter().any(|candidate| candidate.eq_ignore_ascii_case(scheme)) {
            return Err(adk_core::AdkError::tool(format!(
                "URL scheme '{scheme}' is not allowed; allowed schemes: {}. Configure \
                 BrowserToolset::with_allowed_schemes to permit others.",
                self.allowed_schemes.join(", ")
            )));
        }
        if self.allow_private_network {
            return Ok(());
        }

        let refused = match parsed.host() {
            None => false,
            Some(url::Host::Ipv4(ip)) => is_private_ipv4(ip),
            Some(url::Host::Ipv6(ip)) => is_private_ipv6(ip),
            Some(url::Host::Domain(domain)) => {
                let domain = domain.trim_end_matches('.').to_ascii_lowercase();
                PRIVATE_HOST_NAMES.contains(&domain.as_str())
                    || domain.ends_with(".localhost")
                    || resolves_to_private(&domain, parsed.port_or_known_default()).await
            }
        };
        if refused {
            return Err(adk_core::AdkError::tool(format!(
                "URL '{url}' points at a private network address (loopback, private, \
                 link-local, or cloud metadata) and is not allowed. Configure \
                 BrowserToolset::with_private_network_access to permit it."
            )));
        }
        Ok(())
    }
}

/// Whether any address `domain` resolves to is non-public. Failure to resolve is not a refusal.
async fn resolves_to_private(domain: &str, port: Option<u16>) -> bool {
    let lookup = tokio::net::lookup_host((domain, port.unwrap_or(80)));
    match tokio::time::timeout(RESOLVE_TIMEOUT, lookup).await {
        Ok(Ok(addresses)) => addresses.into_iter().any(|address| match address.ip() {
            std::net::IpAddr::V4(ip) => is_private_ipv4(ip),
            std::net::IpAddr::V6(ip) => is_private_ipv6(ip),
        }),
        Ok(Err(_)) | Err(_) => false,
    }
}

fn is_private_ipv4(ip: std::net::Ipv4Addr) -> bool {
    let [first, second, ..] = ip.octets();
    ip.is_loopback()
        || ip.is_private()
        // 169.254.0.0/16, which holds the cloud metadata address 169.254.169.254.
        || ip.is_link_local()
        || ip.is_broadcast()
        // 0.0.0.0/8 reaches the local host on common platforms.
        || first == 0
        // 100.64.0.0/10 shared address space, which holds Alibaba Cloud's metadata service.
        || (first == 100 && (second & 0xc0) == 64)
}

fn is_private_ipv6(ip: std::net::Ipv6Addr) -> bool {
    let segments = ip.segments();
    // IPv4-mapped, IPv4-compatible, and NAT64 (64:ff9b::/96) addresses reach an IPv4 host.
    let embedded = ip.to_ipv4().or_else(|| {
        (segments[..6] == [0x64, 0xff9b, 0, 0, 0, 0])
            .then(|| std::net::Ipv4Addr::from(((segments[6] as u32) << 16) | segments[7] as u32))
    });
    if let Some(v4) = embedded {
        return is_private_ipv4(v4);
    }
    ip.is_loopback()
        || ip.is_unspecified()
        // fc00::/7 unique local, which holds AWS's IPv6 metadata address fd00:ec2::254.
        || (segments[0] & 0xfe00) == 0xfc00
        // fe80::/10 link-local and the deprecated fec0::/10 site-local.
        || (segments[0] & 0xffc0) == 0xfe80
        || (segments[0] & 0xffc0) == 0xfec0
}

/// Tool for navigating to URLs.
///
/// Only URLs whose scheme is in the allowlist are opened; the default is
/// [`DEFAULT_ALLOWED_SCHEMES`] (`http` and `https`). URLs that point at loopback,
/// private, link-local, or cloud metadata addresses are refused until
/// [`with_private_network_access`](Self::with_private_network_access) permits them.
pub struct NavigateTool {
    browser: Arc<BrowserSession>,
    policy: UrlPolicy,
}

impl NavigateTool {
    /// Create a new navigate tool with a shared browser session.
    ///
    /// The tool accepts [`DEFAULT_ALLOWED_SCHEMES`] until
    /// [`with_allowed_schemes`](Self::with_allowed_schemes) replaces them, and refuses
    /// private network addresses.
    pub fn new(browser: Arc<BrowserSession>) -> Self {
        Self { browser, policy: UrlPolicy::default() }
    }

    /// Permit or refuse URLs that point at a private network address.
    ///
    /// Refused by default: a page the model opens on `localhost`, a private range, or
    /// `169.254.169.254` reaches services the agent's operator never exposed, such as an
    /// admin console or a cloud metadata endpoint holding credentials. Enable this only for
    /// an agent meant to browse an internal network.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_browser::{BrowserSession, NavigateTool};
    /// use std::sync::Arc;
    ///
    /// let browser = Arc::new(BrowserSession::with_defaults());
    /// let tool = NavigateTool::new(browser).with_private_network_access(true);
    /// ```
    #[must_use]
    pub fn with_private_network_access(mut self, enabled: bool) -> Self {
        self.policy.allow_private_network = enabled;
        self
    }

    /// Replace the URL schemes this tool accepts.
    ///
    /// Schemes are compared case-insensitively. Allowing `file` exposes the host
    /// filesystem to the model, and allowing `javascript` or `data` lets it run
    /// script outside `browser_evaluate_js`.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_browser::{BrowserSession, NavigateTool};
    /// use std::sync::Arc;
    ///
    /// let browser = Arc::new(BrowserSession::with_defaults());
    /// let tool = NavigateTool::new(browser).with_allowed_schemes(["https"]);
    /// ```
    #[must_use]
    pub fn with_allowed_schemes<I, S>(mut self, schemes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.policy.allowed_schemes = schemes.into_iter().map(Into::into).collect();
        self
    }
}

#[async_trait]
impl Tool for NavigateTool {
    fn name(&self) -> &str {
        "browser_navigate"
    }

    fn description(&self) -> &str {
        "Navigate the browser to a specified URL. Use this to open web pages \
         (http and https URLs by default)."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL to navigate to (e.g., 'https://example.com')"
                }
            },
            "required": ["url"]
        }))
    }

    fn response_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "success": { "type": "boolean" },
                "url": { "type": "string" },
                "title": { "type": "string" }
            }
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let url = args
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| adk_core::AdkError::tool("Missing 'url' parameter"))?;

        self.policy.check(url).await?;

        // Navigate
        self.browser.navigate(url).await?;

        // Get result info
        let current_url = self.browser.current_url().await.unwrap_or_default();
        let title = self.browser.title().await.unwrap_or_default();

        // Include page context like interaction tools do
        match self.browser.page_context().await {
            Ok(page) => Ok(json!({
                "success": true,
                "url": current_url,
                "title": title,
                "page": page
            })),
            Err(e) => Ok(json!({
                "success": true,
                "url": current_url,
                "title": title,
                "page_context_error": e.to_string()
            })),
        }
    }
}

/// Tool for going back in browser history.
pub struct BackTool {
    browser: Arc<BrowserSession>,
}

impl BackTool {
    pub fn new(browser: Arc<BrowserSession>) -> Self {
        Self { browser }
    }
}

#[async_trait]
impl Tool for BackTool {
    fn name(&self) -> &str {
        "browser_back"
    }

    fn description(&self) -> &str {
        "Go back to the previous page in browser history."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {}
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        self.browser.back().await?;

        let url = self.browser.current_url().await.unwrap_or_default();
        let title = self.browser.title().await.unwrap_or_default();

        // Include page context like interaction tools do
        match self.browser.page_context().await {
            Ok(page) => Ok(json!({
                "success": true,
                "url": url,
                "title": title,
                "page": page
            })),
            Err(e) => Ok(json!({
                "success": true,
                "url": url,
                "title": title,
                "page_context_error": e.to_string()
            })),
        }
    }
}

/// Tool for going forward in browser history.
pub struct ForwardTool {
    browser: Arc<BrowserSession>,
}

impl ForwardTool {
    pub fn new(browser: Arc<BrowserSession>) -> Self {
        Self { browser }
    }
}

#[async_trait]
impl Tool for ForwardTool {
    fn name(&self) -> &str {
        "browser_forward"
    }

    fn description(&self) -> &str {
        "Go forward to the next page in browser history."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {}
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        self.browser.forward().await?;

        let url = self.browser.current_url().await.unwrap_or_default();
        let title = self.browser.title().await.unwrap_or_default();

        // Include page context like interaction tools do
        match self.browser.page_context().await {
            Ok(page) => Ok(json!({
                "success": true,
                "url": url,
                "title": title,
                "page": page
            })),
            Err(e) => Ok(json!({
                "success": true,
                "url": url,
                "title": title,
                "page_context_error": e.to_string()
            })),
        }
    }
}

/// Tool for refreshing the current page.
pub struct RefreshTool {
    browser: Arc<BrowserSession>,
}

impl RefreshTool {
    pub fn new(browser: Arc<BrowserSession>) -> Self {
        Self { browser }
    }
}

#[async_trait]
impl Tool for RefreshTool {
    fn name(&self) -> &str {
        "browser_refresh"
    }

    fn description(&self) -> &str {
        "Refresh the current page."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {}
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        self.browser.refresh().await?;

        let url = self.browser.current_url().await.unwrap_or_default();
        let title = self.browser.title().await.unwrap_or_default();

        // Include page context like interaction tools do
        match self.browser.page_context().await {
            Ok(page) => Ok(json!({
                "success": true,
                "url": url,
                "title": title,
                "page": page
            })),
            Err(e) => Ok(json!({
                "success": true,
                "url": url,
                "title": title,
                "page_context_error": e.to_string()
            })),
        }
    }
}
