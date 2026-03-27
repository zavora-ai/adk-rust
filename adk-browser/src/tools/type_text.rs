//! Type tool for entering text into form fields.

use crate::session::BrowserSession;
use adk_core::{Result, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::{Value, json};
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
            .ok_or_else(|| adk_core::AdkError::tool("Missing 'selector' parameter"))?;

        let text = args
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| adk_core::AdkError::tool("Missing 'text' parameter"))?;

        let clear_first = args.get("clear_first").and_then(|v| v.as_bool()).unwrap_or(true);

        let press_enter = args.get("press_enter").and_then(|v| v.as_bool()).unwrap_or(false);

        // Wait for element
        let element = self.browser.wait_for_element(selector, 10).await?;

        // Clear if requested
        if clear_first {
            element
                .clear()
                .await
                .map_err(|e| adk_core::AdkError::tool(format!("Clear failed: {}", e)))?;
        }

        // Type the text
        element
            .send_keys(text)
            .await
            .map_err(|e| adk_core::AdkError::tool(format!("Type failed: {}", e)))?;

        // Press Enter if requested
        if press_enter {
            element
                .send_keys("\n")
                .await
                .map_err(|e| adk_core::AdkError::tool(format!("Enter key failed: {}", e)))?;
        }

        // Get the current value
        let field_value =
            element.attr("value").await.ok().flatten().unwrap_or_else(|| text.to_string());

        // Include page context so the agent knows the current state
        let context = self.browser.page_context().await.unwrap_or_default();

        Ok(json!({
            "success": true,
            "typed_text": text,
            "field_value": field_value,
            "page": context
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
            .ok_or_else(|| adk_core::AdkError::tool("Missing 'selector' parameter"))?;

        self.browser.clear(selector).await?;

        let context = self.browser.page_context().await.unwrap_or_default();

        Ok(json!({
            "success": true,
            "cleared": selector,
            "page": context
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
            .ok_or_else(|| adk_core::AdkError::tool("Missing 'selector' parameter"))?;

        let value = args.get("value").and_then(|v| v.as_str());
        let text = args.get("text").and_then(|v| v.as_str());
        let index = args.get("index").and_then(|v| v.as_u64());

        let escaped_selector = crate::escape::escape_js_string(selector);

        // Build the appropriate selector for the option
        let option_selector = if let Some(val) = value {
            let escaped_val = crate::escape::escape_js_string(val);
            format!("{selector} option[value='{escaped_val}']")
        } else if let Some(txt) = text {
            let escaped_txt = crate::escape::escape_js_string(txt);
            // Use XPath for text matching isn't available, use JS instead
            let script = format!(
                r#"
                var select = document.querySelector('{escaped_selector}');
                for (var i = 0; i < select.options.length; i++) {{
                    if (select.options[i].text === '{escaped_txt}') {{
                        select.selectedIndex = i;
                        select.dispatchEvent(new Event('change', {{ bubbles: true }}));
                        return true;
                    }}
                }}
                return false;
                "#,
            );

            let result = self.browser.execute_script(&script).await?;
            if result.as_bool() == Some(true) {
                let context = self.browser.page_context().await.unwrap_or_default();
                return Ok(json!({
                    "success": true,
                    "selected_text": txt,
                    "page": context
                }));
            } else {
                return Err(adk_core::AdkError::tool(format!(
                    "Option with text '{}' not found",
                    txt
                )));
            }
        } else if let Some(idx) = index {
            let script = format!(
                r#"
                var select = document.querySelector('{escaped_selector}');
                if (select && select.options.length > {idx}) {{
                    select.selectedIndex = {idx};
                    select.dispatchEvent(new Event('change', {{ bubbles: true }}));
                    return select.options[{idx}].text;
                }}
                return null;
                "#,
            );

            let result = self.browser.execute_script(&script).await?;
            if let Some(selected_text) = result.as_str() {
                let context = self.browser.page_context().await.unwrap_or_default();
                return Ok(json!({
                    "success": true,
                    "selected_text": selected_text,
                    "selected_index": idx,
                    "page": context
                }));
            } else {
                return Err(adk_core::AdkError::tool(format!("Option at index {} not found", idx)));
            }
        } else {
            return Err(adk_core::AdkError::tool("Must specify 'value', 'text', or 'index'"));
        };

        // Click the option
        self.browser.click(&option_selector).await?;

        let context = self.browser.page_context().await.unwrap_or_default();

        Ok(json!({
            "success": true,
            "selected_value": value,
            "page": context
        }))
    }
}
