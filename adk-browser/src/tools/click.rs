//! Click tool for interacting with page elements.

use crate::session::BrowserSession;
use adk_core::{Result, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;

/// Tool for clicking elements on the page.
pub struct ClickTool {
    browser: Arc<BrowserSession>,
}

impl ClickTool {
    /// Create a new click tool with a shared browser session.
    pub fn new(browser: Arc<BrowserSession>) -> Self {
        Self { browser }
    }
}

#[async_trait]
impl Tool for ClickTool {
    fn name(&self) -> &str {
        "browser_click"
    }

    fn description(&self) -> &str {
        "Click on an element on the page. Use CSS selectors to identify the element."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "selector": {
                    "type": "string",
                    "description": "CSS selector for the element to click (e.g., '#submit-btn', '.nav-link', 'button[type=submit]')"
                },
                "wait_timeout": {
                    "type": "integer",
                    "description": "Optional timeout in seconds to wait for element to be clickable (default: 10)"
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
                "clicked_element": { "type": "string" }
            }
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let selector = args
            .get("selector")
            .and_then(|v| v.as_str())
            .ok_or_else(|| adk_core::AdkError::Tool("Missing 'selector' parameter".to_string()))?;

        let wait_timeout = args.get("wait_timeout").and_then(|v| v.as_u64()).unwrap_or(10);

        // Wait for element to be clickable, then click
        let element = self.browser.wait_for_clickable(selector, wait_timeout).await?;

        element
            .click()
            .await
            .map_err(|e| adk_core::AdkError::Tool(format!("Click failed: {}", e)))?;

        // Get element info for response
        let tag_name = element.tag_name().await.unwrap_or_else(|_| "unknown".to_string());

        let text = element.text().await.unwrap_or_default();
        let element_info = if text.is_empty() {
            tag_name
        } else {
            format!("{}: {}", tag_name, text.chars().take(50).collect::<String>())
        };

        Ok(json!({
            "success": true,
            "clicked_element": element_info
        }))
    }
}

/// Tool for double-clicking elements.
pub struct DoubleClickTool {
    browser: Arc<BrowserSession>,
}

impl DoubleClickTool {
    pub fn new(browser: Arc<BrowserSession>) -> Self {
        Self { browser }
    }
}

#[async_trait]
impl Tool for DoubleClickTool {
    fn name(&self) -> &str {
        "browser_double_click"
    }

    fn description(&self) -> &str {
        "Double-click on an element on the page."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "selector": {
                    "type": "string",
                    "description": "CSS selector for the element to double-click"
                }
            },
            "required": ["selector"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let selector = args
            .get("selector")
            .and_then(|v| v.as_str())
            .ok_or_else(|| adk_core::AdkError::Tool("Missing 'selector' parameter".to_string()))?;

        let element = self.browser.find_element(selector).await?;

        // Execute double-click via JS
        self.browser
            .execute_script(&format!(
                "document.querySelector('{}').dispatchEvent(new MouseEvent('dblclick', {{'view': window, 'bubbles': true, 'cancelable': true}}))",
                selector.replace('\'', "\\'")
            ))
            .await?;

        let tag_name = element.tag_name().await.unwrap_or_else(|_| "unknown".to_string());

        Ok(json!({
            "success": true,
            "double_clicked_element": tag_name
        }))
    }
}
