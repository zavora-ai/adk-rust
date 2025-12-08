//! Type tool for entering text into form fields.

use crate::session::BrowserSession;
use adk_core::{Result, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;

/// Tool for typing text into input fields.
pub struct TypeTool {
    browser: Arc<BrowserSession>,
}

impl TypeTool {
    /// Create a new type tool with a shared browser session.
    pub fn new(browser: Arc<BrowserSession>) -> Self {
        Self { browser }
    }
}

#[async_trait]
impl Tool for TypeTool {
    fn name(&self) -> &str {
        "browser_type"
    }

    fn description(&self) -> &str {
        "Type text into an input field or text area. Can optionally clear the field first."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "selector": {
                    "type": "string",
                    "description": "CSS selector for the input element (e.g., '#username', 'input[name=email]')"
                },
                "text": {
                    "type": "string",
                    "description": "The text to type into the field"
                },
                "clear_first": {
                    "type": "boolean",
                    "description": "Whether to clear the field before typing (default: true)"
                },
                "press_enter": {
                    "type": "boolean",
                    "description": "Whether to press Enter after typing (default: false)"
                }
            },
            "required": ["selector", "text"]
        }))
    }

    fn response_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "success": { "type": "boolean" },
                "typed_text": { "type": "string" },
                "field_value": { "type": "string" }
            }
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let selector = args
            .get("selector")
            .and_then(|v| v.as_str())
            .ok_or_else(|| adk_core::AdkError::Tool("Missing 'selector' parameter".to_string()))?;

        let text = args
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| adk_core::AdkError::Tool("Missing 'text' parameter".to_string()))?;

        let clear_first = args.get("clear_first").and_then(|v| v.as_bool()).unwrap_or(true);

        let press_enter = args.get("press_enter").and_then(|v| v.as_bool()).unwrap_or(false);

        // Wait for element
        let element = self.browser.wait_for_element(selector, 10).await?;

        // Clear if requested
        if clear_first {
            element
                .clear()
                .await
                .map_err(|e| adk_core::AdkError::Tool(format!("Clear failed: {}", e)))?;
        }

        // Type the text
        element
            .send_keys(text)
            .await
            .map_err(|e| adk_core::AdkError::Tool(format!("Type failed: {}", e)))?;

        // Press Enter if requested
        if press_enter {
            element
                .send_keys("\n")
                .await
                .map_err(|e| adk_core::AdkError::Tool(format!("Enter key failed: {}", e)))?;
        }

        // Get the current value
        let field_value =
            element.attr("value").await.ok().flatten().unwrap_or_else(|| text.to_string());

        Ok(json!({
            "success": true,
            "typed_text": text,
            "field_value": field_value
        }))
    }
}

/// Tool for clearing input fields.
pub struct ClearTool {
    browser: Arc<BrowserSession>,
}

impl ClearTool {
    pub fn new(browser: Arc<BrowserSession>) -> Self {
        Self { browser }
    }
}

#[async_trait]
impl Tool for ClearTool {
    fn name(&self) -> &str {
        "browser_clear"
    }

    fn description(&self) -> &str {
        "Clear the contents of an input field or text area."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "selector": {
                    "type": "string",
                    "description": "CSS selector for the input element to clear"
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

        self.browser.clear(selector).await?;

        Ok(json!({
            "success": true,
            "cleared": selector
        }))
    }
}

/// Tool for selecting options from dropdown menus.
pub struct SelectTool {
    browser: Arc<BrowserSession>,
}

impl SelectTool {
    pub fn new(browser: Arc<BrowserSession>) -> Self {
        Self { browser }
    }
}

#[async_trait]
impl Tool for SelectTool {
    fn name(&self) -> &str {
        "browser_select"
    }

    fn description(&self) -> &str {
        "Select an option from a dropdown/select element by value, text, or index."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "selector": {
                    "type": "string",
                    "description": "CSS selector for the select element"
                },
                "value": {
                    "type": "string",
                    "description": "The value attribute of the option to select"
                },
                "text": {
                    "type": "string",
                    "description": "The visible text of the option to select"
                },
                "index": {
                    "type": "integer",
                    "description": "The index of the option to select (0-based)"
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

        let value = args.get("value").and_then(|v| v.as_str());
        let text = args.get("text").and_then(|v| v.as_str());
        let index = args.get("index").and_then(|v| v.as_u64());

        // Build the appropriate selector for the option
        let option_selector = if let Some(val) = value {
            format!("{} option[value='{}']", selector, val)
        } else if let Some(txt) = text {
            // Use XPath for text matching isn't available, use JS instead
            let script = format!(
                r#"
                var select = document.querySelector('{}');
                for (var i = 0; i < select.options.length; i++) {{
                    if (select.options[i].text === '{}') {{
                        select.selectedIndex = i;
                        select.dispatchEvent(new Event('change', {{ bubbles: true }}));
                        return true;
                    }}
                }}
                return false;
                "#,
                selector.replace('\'', "\\'"),
                txt.replace('\'', "\\'")
            );

            let result = self.browser.execute_script(&script).await?;
            if result.as_bool() == Some(true) {
                return Ok(json!({
                    "success": true,
                    "selected_text": txt
                }));
            } else {
                return Err(adk_core::AdkError::Tool(format!(
                    "Option with text '{}' not found",
                    txt
                )));
            }
        } else if let Some(idx) = index {
            let script = format!(
                r#"
                var select = document.querySelector('{}');
                if (select && select.options.length > {}) {{
                    select.selectedIndex = {};
                    select.dispatchEvent(new Event('change', {{ bubbles: true }}));
                    return select.options[{}].text;
                }}
                return null;
                "#,
                selector.replace('\'', "\\'"),
                idx,
                idx,
                idx
            );

            let result = self.browser.execute_script(&script).await?;
            if let Some(selected_text) = result.as_str() {
                return Ok(json!({
                    "success": true,
                    "selected_text": selected_text,
                    "selected_index": idx
                }));
            } else {
                return Err(adk_core::AdkError::Tool(format!("Option at index {} not found", idx)));
            }
        } else {
            return Err(adk_core::AdkError::Tool(
                "Must specify 'value', 'text', or 'index'".to_string(),
            ));
        };

        // Click the option
        self.browser.click(&option_selector).await?;

        Ok(json!({
            "success": true,
            "selected_value": value
        }))
    }
}
