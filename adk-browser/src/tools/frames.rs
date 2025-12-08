//! Frame/iframe management tools.

use crate::session::BrowserSession;
use adk_core::{AdkError, Result, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;

/// Tool for switching to a frame by index.
pub struct SwitchToFrameTool {
    browser: Arc<BrowserSession>,
}

impl SwitchToFrameTool {
    pub fn new(browser: Arc<BrowserSession>) -> Self {
        Self { browser }
    }
}

#[async_trait]
impl Tool for SwitchToFrameTool {
    fn name(&self) -> &str {
        "browser_switch_to_frame"
    }

    fn description(&self) -> &str {
        "Switch to an iframe by index number or CSS selector."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "index": {
                    "type": "integer",
                    "description": "Frame index (0-based)"
                },
                "selector": {
                    "type": "string",
                    "description": "CSS selector for the iframe element"
                }
            }
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let index = args.get("index").and_then(|v| v.as_u64());
        let selector = args.get("selector").and_then(|v| v.as_str());

        if let Some(idx) = index {
            self.browser.switch_to_frame_by_index(idx as u16).await?;
            Ok(json!({
                "success": true,
                "switched_to_frame": idx
            }))
        } else if let Some(sel) = selector {
            self.browser.switch_to_frame_by_selector(sel).await?;
            Ok(json!({
                "success": true,
                "switched_to_frame": sel
            }))
        } else {
            Err(AdkError::Tool("Must provide either 'index' or 'selector'".to_string()))
        }
    }
}

/// Tool for switching to the parent frame.
pub struct SwitchToParentFrameTool {
    browser: Arc<BrowserSession>,
}

impl SwitchToParentFrameTool {
    pub fn new(browser: Arc<BrowserSession>) -> Self {
        Self { browser }
    }
}

#[async_trait]
impl Tool for SwitchToParentFrameTool {
    fn name(&self) -> &str {
        "browser_switch_to_parent_frame"
    }

    fn description(&self) -> &str {
        "Switch to the parent frame (exit current iframe)."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {}
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        self.browser.switch_to_parent_frame().await?;

        Ok(json!({
            "success": true,
            "message": "Switched to parent frame"
        }))
    }
}

/// Tool for switching to the default/main content.
pub struct SwitchToDefaultContentTool {
    browser: Arc<BrowserSession>,
}

impl SwitchToDefaultContentTool {
    pub fn new(browser: Arc<BrowserSession>) -> Self {
        Self { browser }
    }
}

#[async_trait]
impl Tool for SwitchToDefaultContentTool {
    fn name(&self) -> &str {
        "browser_switch_to_default_content"
    }

    fn description(&self) -> &str {
        "Switch back to the main page content (exit all iframes)."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {}
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        self.browser.switch_to_default_content().await?;

        Ok(json!({
            "success": true,
            "message": "Switched to default content"
        }))
    }
}
