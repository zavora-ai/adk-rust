//! JavaScript evaluation tool.

use crate::session::BrowserSession;
use adk_core::{Result, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;

/// Tool for executing JavaScript in the browser.
pub struct EvaluateJsTool {
    browser: Arc<BrowserSession>,
}

impl EvaluateJsTool {
    /// Create a new JavaScript evaluation tool with a shared browser session.
    pub fn new(browser: Arc<BrowserSession>) -> Self {
        Self { browser }
    }
}

#[async_trait]
impl Tool for EvaluateJsTool {
    fn name(&self) -> &str {
        "browser_evaluate_js"
    }

    fn description(&self) -> &str {
        "Execute JavaScript code in the browser and return the result. Use for complex interactions or data extraction."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "script": {
                    "type": "string",
                    "description": "JavaScript code to execute. Use 'return' to get a value back."
                },
                "async": {
                    "type": "boolean",
                    "description": "Whether the script is async (uses a callback). Default: false"
                }
            },
            "required": ["script"]
        }))
    }

    fn response_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "success": { "type": "boolean" },
                "result": {}
            }
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let script = args
            .get("script")
            .and_then(|v| v.as_str())
            .ok_or_else(|| adk_core::AdkError::Tool("Missing 'script' parameter".to_string()))?;

        let is_async = args.get("async").and_then(|v| v.as_bool()).unwrap_or(false);

        let result = if is_async {
            self.browser.execute_async_script(script).await?
        } else {
            self.browser.execute_script(script).await?
        };

        Ok(json!({
            "success": true,
            "result": result
        }))
    }
}

/// Tool for scrolling the page.
pub struct ScrollTool {
    browser: Arc<BrowserSession>,
}

impl ScrollTool {
    pub fn new(browser: Arc<BrowserSession>) -> Self {
        Self { browser }
    }
}

#[async_trait]
impl Tool for ScrollTool {
    fn name(&self) -> &str {
        "browser_scroll"
    }

    fn description(&self) -> &str {
        "Scroll the page in a direction or to a specific element."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "direction": {
                    "type": "string",
                    "enum": ["up", "down", "top", "bottom"],
                    "description": "Direction to scroll"
                },
                "selector": {
                    "type": "string",
                    "description": "CSS selector of element to scroll into view"
                },
                "amount": {
                    "type": "integer",
                    "description": "Pixels to scroll (for up/down). Default: 500"
                }
            }
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let direction = args.get("direction").and_then(|v| v.as_str());
        let selector = args.get("selector").and_then(|v| v.as_str());
        let amount = args.get("amount").and_then(|v| v.as_i64()).unwrap_or(500);

        if let Some(sel) = selector {
            // Scroll element into view
            let script = format!(
                "document.querySelector('{}').scrollIntoView({{ behavior: 'smooth', block: 'center' }})",
                sel.replace('\'', "\\'")
            );
            self.browser.execute_script(&script).await?;

            return Ok(json!({
                "success": true,
                "scrolled_to": sel
            }));
        }

        if let Some(dir) = direction {
            let script = match dir {
                "up" => format!("window.scrollBy(0, -{})", amount),
                "down" => format!("window.scrollBy(0, {})", amount),
                "top" => "window.scrollTo(0, 0)".to_string(),
                "bottom" => "window.scrollTo(0, document.body.scrollHeight)".to_string(),
                _ => return Err(adk_core::AdkError::Tool(format!("Invalid direction: {}", dir))),
            };

            self.browser.execute_script(&script).await?;

            return Ok(json!({
                "success": true,
                "scrolled": dir
            }));
        }

        Err(adk_core::AdkError::Tool("Must specify either 'direction' or 'selector'".to_string()))
    }
}

/// Tool for hovering over elements.
pub struct HoverTool {
    browser: Arc<BrowserSession>,
}

impl HoverTool {
    pub fn new(browser: Arc<BrowserSession>) -> Self {
        Self { browser }
    }
}

#[async_trait]
impl Tool for HoverTool {
    fn name(&self) -> &str {
        "browser_hover"
    }

    fn description(&self) -> &str {
        "Hover over an element to trigger hover effects or tooltips."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "selector": {
                    "type": "string",
                    "description": "CSS selector for the element to hover over"
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

        // Use JavaScript to trigger hover events
        let script = format!(
            r#"
            var element = document.querySelector('{}');
            if (element) {{
                var event = new MouseEvent('mouseover', {{
                    'view': window,
                    'bubbles': true,
                    'cancelable': true
                }});
                element.dispatchEvent(event);
                return true;
            }}
            return false;
            "#,
            selector.replace('\'', "\\'")
        );

        let result = self.browser.execute_script(&script).await?;

        if result.as_bool() == Some(true) {
            Ok(json!({
                "success": true,
                "hovered": selector
            }))
        } else {
            Err(adk_core::AdkError::Tool(format!("Element not found: {}", selector)))
        }
    }
}

/// Tool for handling alerts/dialogs.
pub struct AlertTool {
    browser: Arc<BrowserSession>,
}

impl AlertTool {
    pub fn new(browser: Arc<BrowserSession>) -> Self {
        Self { browser }
    }
}

#[async_trait]
impl Tool for AlertTool {
    fn name(&self) -> &str {
        "browser_handle_alert"
    }

    fn description(&self) -> &str {
        "Handle JavaScript alerts, confirms, and prompts."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["accept", "dismiss"],
                    "description": "Action to take on the alert"
                },
                "text": {
                    "type": "string",
                    "description": "Text to enter for prompt dialogs"
                }
            },
            "required": ["action"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| adk_core::AdkError::Tool("Missing 'action' parameter".to_string()))?;

        let _text = args.get("text").and_then(|v| v.as_str());

        // Note: Full alert handling requires thirtyfour's alert API
        // For now, provide a JavaScript-based approach
        let script = match action {
            "accept" => {
                r#"
                window.alert = function() { return true; };
                window.confirm = function() { return true; };
                window.prompt = function() { return ''; };
                return 'ok';
                "#
            }
            "dismiss" => {
                r#"
                window.alert = function() { return false; };
                window.confirm = function() { return false; };
                window.prompt = function() { return null; };
                return 'ok';
                "#
            }
            _ => return Err(adk_core::AdkError::Tool(format!("Invalid action: {}", action))),
        };

        self.browser.execute_script(script).await?;

        Ok(json!({
            "success": true,
            "action": action
        }))
    }
}
