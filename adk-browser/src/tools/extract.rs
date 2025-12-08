//! Extract tool for getting content from the page.

use crate::session::BrowserSession;
use adk_core::{Result, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;

/// Tool for extracting text content from elements.
pub struct ExtractTextTool {
    browser: Arc<BrowserSession>,
}

impl ExtractTextTool {
    /// Create a new extract text tool with a shared browser session.
    pub fn new(browser: Arc<BrowserSession>) -> Self {
        Self { browser }
    }
}

#[async_trait]
impl Tool for ExtractTextTool {
    fn name(&self) -> &str {
        "browser_extract_text"
    }

    fn description(&self) -> &str {
        "Extract text content from one or more elements on the page."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "selector": {
                    "type": "string",
                    "description": "CSS selector for the element(s) to extract text from"
                },
                "all": {
                    "type": "boolean",
                    "description": "If true, extract from all matching elements. If false, only first match (default: false)"
                }
            },
            "required": ["selector"]
        }))
    }

    fn response_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "success": { "type": "boolean" },
                "text": { "type": "string" },
                "texts": {
                    "type": "array",
                    "items": { "type": "string" }
                },
                "count": { "type": "integer" }
            }
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let selector = args
            .get("selector")
            .and_then(|v| v.as_str())
            .ok_or_else(|| adk_core::AdkError::Tool("Missing 'selector' parameter".to_string()))?;

        let all = args.get("all").and_then(|v| v.as_bool()).unwrap_or(false);

        if all {
            let elements = self.browser.find_elements(selector).await?;
            let mut texts = Vec::new();

            for element in elements {
                if let Ok(text) = element.text().await {
                    texts.push(text);
                }
            }

            Ok(json!({
                "success": true,
                "texts": texts,
                "count": texts.len()
            }))
        } else {
            let text = self.browser.get_text(selector).await?;

            Ok(json!({
                "success": true,
                "text": text
            }))
        }
    }
}

/// Tool for extracting attribute values.
pub struct ExtractAttributeTool {
    browser: Arc<BrowserSession>,
}

impl ExtractAttributeTool {
    pub fn new(browser: Arc<BrowserSession>) -> Self {
        Self { browser }
    }
}

#[async_trait]
impl Tool for ExtractAttributeTool {
    fn name(&self) -> &str {
        "browser_extract_attribute"
    }

    fn description(&self) -> &str {
        "Extract an attribute value from an element (e.g., href, src, value)."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "selector": {
                    "type": "string",
                    "description": "CSS selector for the element"
                },
                "attribute": {
                    "type": "string",
                    "description": "Name of the attribute to extract (e.g., 'href', 'src', 'value', 'class')"
                }
            },
            "required": ["selector", "attribute"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let selector = args
            .get("selector")
            .and_then(|v| v.as_str())
            .ok_or_else(|| adk_core::AdkError::Tool("Missing 'selector' parameter".to_string()))?;

        let attribute = args
            .get("attribute")
            .and_then(|v| v.as_str())
            .ok_or_else(|| adk_core::AdkError::Tool("Missing 'attribute' parameter".to_string()))?;

        let value = self.browser.get_attribute(selector, attribute).await?;

        Ok(json!({
            "success": true,
            "attribute": attribute,
            "value": value
        }))
    }
}

/// Tool for extracting links from the page.
pub struct ExtractLinksTool {
    browser: Arc<BrowserSession>,
}

impl ExtractLinksTool {
    pub fn new(browser: Arc<BrowserSession>) -> Self {
        Self { browser }
    }
}

#[async_trait]
impl Tool for ExtractLinksTool {
    fn name(&self) -> &str {
        "browser_extract_links"
    }

    fn description(&self) -> &str {
        "Extract all links from the page or a specific container."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "selector": {
                    "type": "string",
                    "description": "Optional CSS selector to limit link extraction to a container"
                },
                "include_text": {
                    "type": "boolean",
                    "description": "Include link text in results (default: true)"
                }
            }
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let container = args.get("selector").and_then(|v| v.as_str());
        let include_text = args.get("include_text").and_then(|v| v.as_bool()).unwrap_or(true);

        let link_selector = if let Some(sel) = container {
            format!("{} a[href]", sel)
        } else {
            "a[href]".to_string()
        };

        let elements = self.browser.find_elements(&link_selector).await?;
        let mut links = Vec::new();

        for element in elements {
            let href = element.attr("href").await.ok().flatten();
            let text = if include_text { element.text().await.ok() } else { None };

            if let Some(href) = href {
                links.push(json!({
                    "href": href,
                    "text": text
                }));
            }
        }

        Ok(json!({
            "success": true,
            "links": links,
            "count": links.len()
        }))
    }
}

/// Tool for getting page info (title, URL, etc.).
pub struct PageInfoTool {
    browser: Arc<BrowserSession>,
}

impl PageInfoTool {
    pub fn new(browser: Arc<BrowserSession>) -> Self {
        Self { browser }
    }
}

#[async_trait]
impl Tool for PageInfoTool {
    fn name(&self) -> &str {
        "browser_page_info"
    }

    fn description(&self) -> &str {
        "Get information about the current page (title, URL, etc.)."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {}
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        let url = self.browser.current_url().await?;
        let title = self.browser.title().await?;

        Ok(json!({
            "success": true,
            "url": url,
            "title": title
        }))
    }
}

/// Tool for getting the page HTML source.
pub struct PageSourceTool {
    browser: Arc<BrowserSession>,
}

impl PageSourceTool {
    pub fn new(browser: Arc<BrowserSession>) -> Self {
        Self { browser }
    }
}

#[async_trait]
impl Tool for PageSourceTool {
    fn name(&self) -> &str {
        "browser_page_source"
    }

    fn description(&self) -> &str {
        "Get the HTML source of the current page. Warning: may be large."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "max_length": {
                    "type": "integer",
                    "description": "Maximum characters to return (default: 50000)"
                }
            }
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let max_length = args.get("max_length").and_then(|v| v.as_u64()).unwrap_or(50000) as usize;

        let source = self.browser.page_source().await?;
        let truncated = source.len() > max_length;
        let html =
            if truncated { source.chars().take(max_length).collect::<String>() } else { source };

        Ok(json!({
            "success": true,
            "html": html,
            "truncated": truncated,
            "total_length": html.len()
        }))
    }
}
