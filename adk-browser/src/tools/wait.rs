//! Wait tools for synchronization.

use crate::session::BrowserSession;
use adk_core::{Result, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

/// Tool for waiting for an element to appear.
pub struct WaitForElementTool {
    browser: Arc<BrowserSession>,
}

impl WaitForElementTool {
    /// Create a new wait tool with a shared browser session.
    pub fn new(browser: Arc<BrowserSession>) -> Self {
        Self { browser }
    }
}

#[async_trait]
impl Tool for WaitForElementTool {
    fn name(&self) -> &str {
        "browser_wait_for_element"
    }

    fn description(&self) -> &str {
        "Wait for an element to appear on the page. Useful after navigation or dynamic content loading."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "selector": {
                    "type": "string",
                    "description": "CSS selector for the element to wait for"
                },
                "timeout": {
                    "type": "integer",
                    "description": "Maximum wait time in seconds (default: 30)"
                },
                "visible": {
                    "type": "boolean",
                    "description": "Wait for element to be visible, not just present (default: false)"
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
                "found": { "type": "boolean" },
                "element_text": { "type": "string" }
            }
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let selector = args
            .get("selector")
            .and_then(|v| v.as_str())
            .ok_or_else(|| adk_core::AdkError::Tool("Missing 'selector' parameter".to_string()))?;

        let timeout = args.get("timeout").and_then(|v| v.as_u64()).unwrap_or(30);

        let visible = args.get("visible").and_then(|v| v.as_bool()).unwrap_or(false);

        let element = if visible {
            self.browser.wait_for_clickable(selector, timeout).await?
        } else {
            self.browser.wait_for_element(selector, timeout).await?
        };

        let text = element.text().await.unwrap_or_default();
        let text_preview = text.chars().take(100).collect::<String>();

        Ok(json!({
            "success": true,
            "found": true,
            "element_text": text_preview
        }))
    }
}

/// Tool for waiting a fixed duration.
pub struct WaitTool;

impl WaitTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for WaitTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for WaitTool {
    fn name(&self) -> &str {
        "browser_wait"
    }

    fn description(&self) -> &str {
        "Wait for a specified duration. Use sparingly - prefer waiting for specific elements."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "seconds": {
                    "type": "number",
                    "description": "Number of seconds to wait (max: 30)"
                }
            },
            "required": ["seconds"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let seconds = args
            .get("seconds")
            .and_then(|v| v.as_f64())
            .ok_or_else(|| adk_core::AdkError::Tool("Missing 'seconds' parameter".to_string()))?;

        // Cap at 30 seconds
        let seconds = seconds.min(30.0);

        tokio::time::sleep(Duration::from_secs_f64(seconds)).await;

        Ok(json!({
            "success": true,
            "waited_seconds": seconds
        }))
    }
}

/// Tool for waiting for page to load.
pub struct WaitForPageLoadTool {
    browser: Arc<BrowserSession>,
}

impl WaitForPageLoadTool {
    pub fn new(browser: Arc<BrowserSession>) -> Self {
        Self { browser }
    }
}

#[async_trait]
impl Tool for WaitForPageLoadTool {
    fn name(&self) -> &str {
        "browser_wait_for_page_load"
    }

    fn description(&self) -> &str {
        "Wait for the page to finish loading (document.readyState === 'complete')."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "timeout": {
                    "type": "integer",
                    "description": "Maximum wait time in seconds (default: 30)"
                }
            }
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let timeout = args.get("timeout").and_then(|v| v.as_u64()).unwrap_or(30);

        let script = "return document.readyState";
        let start = std::time::Instant::now();

        loop {
            let result = self.browser.execute_script(script).await?;
            if result.as_str() == Some("complete") {
                break;
            }

            if start.elapsed().as_secs() > timeout {
                return Err(adk_core::AdkError::Tool(format!(
                    "Page load timeout after {}s",
                    timeout
                )));
            }

            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        let url = self.browser.current_url().await?;
        let title = self.browser.title().await?;

        Ok(json!({
            "success": true,
            "url": url,
            "title": title,
            "ready_state": "complete"
        }))
    }
}

/// Tool for waiting for text to appear.
pub struct WaitForTextTool {
    browser: Arc<BrowserSession>,
}

impl WaitForTextTool {
    pub fn new(browser: Arc<BrowserSession>) -> Self {
        Self { browser }
    }
}

#[async_trait]
impl Tool for WaitForTextTool {
    fn name(&self) -> &str {
        "browser_wait_for_text"
    }

    fn description(&self) -> &str {
        "Wait for specific text to appear anywhere on the page."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "text": {
                    "type": "string",
                    "description": "The text to wait for"
                },
                "timeout": {
                    "type": "integer",
                    "description": "Maximum wait time in seconds (default: 30)"
                }
            },
            "required": ["text"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let text = args
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| adk_core::AdkError::Tool("Missing 'text' parameter".to_string()))?;

        let timeout = args.get("timeout").and_then(|v| v.as_u64()).unwrap_or(30);

        let script =
            format!("return document.body.innerText.includes('{}')", text.replace('\'', "\\'"));

        let start = std::time::Instant::now();

        loop {
            let result = self.browser.execute_script(&script).await?;
            if result.as_bool() == Some(true) {
                return Ok(json!({
                    "success": true,
                    "found": true,
                    "text": text
                }));
            }

            if start.elapsed().as_secs() > timeout {
                return Err(adk_core::AdkError::Tool(format!(
                    "Text '{}' not found after {}s",
                    text, timeout
                )));
            }

            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}
