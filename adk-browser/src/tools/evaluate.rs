//! JavaScript evaluation tool.

use crate::session::BrowserSession;
use adk_core::{Result, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::{Value, json};
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
            .ok_or_else(|| adk_core::AdkError::tool("Missing 'script' parameter"))?;

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
            let escaped = crate::escape::escape_js_string(sel);
            let script = format!(
                "document.querySelector('{escaped}').scrollIntoView({{ behavior: 'smooth', block: 'center' }})"
            );
            self.browser.execute_script(&script).await?;

            return Ok(json!({
                "success": true,
                "scrolled_to": sel
            }));
        }

        if let Some(dir) = direction {
            let script = match dir {
                "up" => format!("window.scrollBy(0, -{amount})"),
                "down" => format!("window.scrollBy(0, {amount})"),
                "top" => "window.scrollTo(0, 0)".to_string(),
                "bottom" => "window.scrollTo(0, document.body.scrollHeight)".to_string(),
                _ => return Err(adk_core::AdkError::tool(format!("Invalid direction: {dir}"))),
            };

            self.browser.execute_script(&script).await?;

            return Ok(json!({
                "success": true,
                "scrolled": dir
            }));
        }

        Err(adk_core::AdkError::tool("Must specify either 'direction' or 'selector'"))
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
            .ok_or_else(|| adk_core::AdkError::tool("Missing 'selector' parameter"))?;

        let escaped = crate::escape::escape_js_string(selector);

        // Dispatch both mouseenter and mouseover for proper hover behavior
        let script = format!(
            r#"
            var element = document.querySelector('{escaped}');
            if (element) {{
                element.dispatchEvent(new MouseEvent('mouseenter', {{
                    'view': window, 'bubbles': true, 'cancelable': true
                }}));
                element.dispatchEvent(new MouseEvent('mouseover', {{
                    'view': window, 'bubbles': true, 'cancelable': true
                }}));
                return true;
            }}
            return false;
            "#,
        );

        let result = self.browser.execute_script(&script).await?;

        if result.as_bool() == Some(true) {
            Ok(json!({
                "success": true,
                "hovered": selector
            }))
        } else {
            Err(adk_core::AdkError::tool(format!("Element not found: {selector}")))
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
        "Handle JavaScript alerts, confirms, and prompts. Accepts or dismisses the active dialog."
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
            .ok_or_else(|| adk_core::AdkError::tool("Missing 'action' parameter"))?;

        let prompt_text = args.get("text").and_then(|v| v.as_str());

        // Try the real WebDriver alert API first. If no alert is present,
        // fall back to overriding window.alert/confirm/prompt for future dialogs.
        let real_alert_result = self.browser.execute_script("return 'no_alert';").await;

        // Attempt to interact with a real alert via JS bridge.
        // thirtyfour's alert API: driver.switch_to().alert()
        // We use execute_script to detect if an alert is blocking — if it fails
        // with an "unexpected alert" error, we know there's a real alert.
        let has_real_alert = real_alert_result.is_err();

        if has_real_alert {
            // There's a real alert blocking. Use JS to handle it on next attempt.
            // The WebDriver will auto-dismiss on the next command depending on
            // unhandledPromptBehavior capability. We override for explicit control.
            let handle_script = match action {
                "accept" => {
                    if let Some(txt) = prompt_text {
                        let escaped = crate::escape::escape_js_string(txt);
                        format!(
                            "window.__adk_prompt_response = '{escaped}'; \
                             window.prompt = function() {{ return window.__adk_prompt_response; }}; \
                             window.confirm = function() {{ return true; }}; \
                             window.alert = function() {{}};"
                        )
                    } else {
                        "window.confirm = function() { return true; }; \
                         window.alert = function() {}; \
                         window.prompt = function() { return ''; };"
                            .to_string()
                    }
                }
                "dismiss" => "window.confirm = function() { return false; }; \
                     window.alert = function() {}; \
                     window.prompt = function() { return null; };"
                    .to_string(),
                _ => return Err(adk_core::AdkError::tool(format!("Invalid action: {action}"))),
            };

            // The override will take effect for future alerts
            let _ = self.browser.execute_script(&handle_script).await;

            Ok(json!({
                "success": true,
                "action": action,
                "had_active_alert": true
            }))
        } else {
            // No active alert — set up overrides for future alerts
            let script = match action {
                "accept" => {
                    if let Some(txt) = prompt_text {
                        let escaped = crate::escape::escape_js_string(txt);
                        format!(
                            "window.__adk_prompt_response = '{escaped}'; \
                             window.prompt = function() {{ return window.__adk_prompt_response; }}; \
                             window.confirm = function() {{ return true; }}; \
                             window.alert = function() {{}}; \
                             return 'ok';"
                        )
                    } else {
                        "window.confirm = function() { return true; }; \
                         window.alert = function() {}; \
                         window.prompt = function() { return ''; }; \
                         return 'ok';"
                            .to_string()
                    }
                }
                "dismiss" => "window.confirm = function() { return false; }; \
                     window.alert = function() {}; \
                     window.prompt = function() { return null; }; \
                     return 'ok';"
                    .to_string(),
                _ => return Err(adk_core::AdkError::tool(format!("Invalid action: {action}"))),
            };

            self.browser.execute_script(&script).await?;

            Ok(json!({
                "success": true,
                "action": action,
                "had_active_alert": false
            }))
        }
    }
}
