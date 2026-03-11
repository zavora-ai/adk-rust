//! Navigate tool for browser navigation.

use crate::session::BrowserSession;
use adk_core::{Result, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;

/// Tool for navigating to URLs.
pub struct NavigateTool {
    browser: Arc<BrowserSession>,
}

impl NavigateTool {
    /// Create a new navigate tool with a shared browser session.
    pub fn new(browser: Arc<BrowserSession>) -> Self {
        Self { browser }
    }
}

#[async_trait]
impl Tool for NavigateTool {
    fn name(&self) -> &str {
        "browser_navigate"
    }

    fn description(&self) -> &str {
        "Navigate the browser to a specified URL. Use this to open web pages."
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
            .ok_or_else(|| adk_core::AdkError::Tool("Missing 'url' parameter".to_string()))?;

        // Validate URL
        url::Url::parse(url)
            .map_err(|e| adk_core::AdkError::Tool(format!("Invalid URL '{}': {}", url, e)))?;

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
