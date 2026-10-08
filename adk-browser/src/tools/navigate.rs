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

/// Rejects a model-supplied URL that does not parse or whose scheme is not in `allowed`.
pub(crate) fn validate_url(url: &str, allowed: &[String]) -> Result<()> {
    let parsed = url::Url::parse(url)
        .map_err(|e| adk_core::AdkError::tool(format!("Invalid URL '{url}': {e}")))?;
    let scheme = parsed.scheme();
    if allowed.iter().any(|candidate| candidate.eq_ignore_ascii_case(scheme)) {
        return Ok(());
    }
    Err(adk_core::AdkError::tool(format!(
        "URL scheme '{scheme}' is not allowed; allowed schemes: {}. Configure \
         BrowserToolset::with_allowed_schemes to permit others.",
        allowed.join(", ")
    )))
}

/// Tool for navigating to URLs.
///
/// Only URLs whose scheme is in the allowlist are opened; the default is
/// [`DEFAULT_ALLOWED_SCHEMES`] (`http` and `https`).
pub struct NavigateTool {
    browser: Arc<BrowserSession>,
    allowed_schemes: Vec<String>,
}

impl NavigateTool {
    /// Create a new navigate tool with a shared browser session.
    ///
    /// The tool accepts [`DEFAULT_ALLOWED_SCHEMES`] until
    /// [`with_allowed_schemes`](Self::with_allowed_schemes) replaces them.
    pub fn new(browser: Arc<BrowserSession>) -> Self {
        Self { browser, allowed_schemes: default_allowed_schemes() }
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
        self.allowed_schemes = schemes.into_iter().map(Into::into).collect();
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

        validate_url(url, &self.allowed_schemes)?;

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
