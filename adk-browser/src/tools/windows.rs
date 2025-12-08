//! Window and tab management tools.

use crate::session::BrowserSession;
use adk_core::{AdkError, Result, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;

/// Tool for listing all windows/tabs.
pub struct ListWindowsTool {
    browser: Arc<BrowserSession>,
}

impl ListWindowsTool {
    pub fn new(browser: Arc<BrowserSession>) -> Self {
        Self { browser }
    }
}

#[async_trait]
impl Tool for ListWindowsTool {
    fn name(&self) -> &str {
        "browser_list_windows"
    }

    fn description(&self) -> &str {
        "List all open browser windows/tabs."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {}
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        let (windows, current) = self.browser.list_windows().await?;

        Ok(json!({
            "success": true,
            "windows": windows,
            "current_window": current,
            "count": windows.len()
        }))
    }
}

/// Tool for opening a new tab.
pub struct NewTabTool {
    browser: Arc<BrowserSession>,
}

impl NewTabTool {
    pub fn new(browser: Arc<BrowserSession>) -> Self {
        Self { browser }
    }
}

#[async_trait]
impl Tool for NewTabTool {
    fn name(&self) -> &str {
        "browser_new_tab"
    }

    fn description(&self) -> &str {
        "Open a new browser tab and switch to it."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "Optional URL to navigate to in the new tab"
                }
            }
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let url = args.get("url").and_then(|v| v.as_str());

        let handle = self.browser.new_tab().await?;

        if let Some(url) = url {
            self.browser.navigate(url).await?;
        }

        let current_url = self.browser.current_url().await.unwrap_or_default();

        Ok(json!({
            "success": true,
            "window_handle": handle,
            "url": current_url
        }))
    }
}

/// Tool for opening a new window.
pub struct NewWindowTool {
    browser: Arc<BrowserSession>,
}

impl NewWindowTool {
    pub fn new(browser: Arc<BrowserSession>) -> Self {
        Self { browser }
    }
}

#[async_trait]
impl Tool for NewWindowTool {
    fn name(&self) -> &str {
        "browser_new_window"
    }

    fn description(&self) -> &str {
        "Open a new browser window and switch to it."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "Optional URL to navigate to in the new window"
                }
            }
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let url = args.get("url").and_then(|v| v.as_str());

        let handle = self.browser.new_window().await?;

        if let Some(url) = url {
            self.browser.navigate(url).await?;
        }

        let current_url = self.browser.current_url().await.unwrap_or_default();

        Ok(json!({
            "success": true,
            "window_handle": handle,
            "url": current_url
        }))
    }
}

/// Tool for switching to a window/tab.
pub struct SwitchWindowTool {
    browser: Arc<BrowserSession>,
}

impl SwitchWindowTool {
    pub fn new(browser: Arc<BrowserSession>) -> Self {
        Self { browser }
    }
}

#[async_trait]
impl Tool for SwitchWindowTool {
    fn name(&self) -> &str {
        "browser_switch_window"
    }

    fn description(&self) -> &str {
        "Switch to a different browser window/tab by its handle."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "handle": {
                    "type": "string",
                    "description": "The window handle to switch to"
                }
            },
            "required": ["handle"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let handle = args
            .get("handle")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AdkError::Tool("Missing 'handle' parameter".to_string()))?;

        self.browser.switch_to_window(handle).await?;

        let url = self.browser.current_url().await.unwrap_or_default();
        let title = self.browser.title().await.unwrap_or_default();

        Ok(json!({
            "success": true,
            "switched_to": handle,
            "url": url,
            "title": title
        }))
    }
}

/// Tool for closing the current window/tab.
pub struct CloseWindowTool {
    browser: Arc<BrowserSession>,
}

impl CloseWindowTool {
    pub fn new(browser: Arc<BrowserSession>) -> Self {
        Self { browser }
    }
}

#[async_trait]
impl Tool for CloseWindowTool {
    fn name(&self) -> &str {
        "browser_close_window"
    }

    fn description(&self) -> &str {
        "Close the current browser window/tab."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {}
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        self.browser.close_window().await?;

        Ok(json!({
            "success": true,
            "message": "Window closed"
        }))
    }
}

/// Tool for maximizing the window.
pub struct MaximizeWindowTool {
    browser: Arc<BrowserSession>,
}

impl MaximizeWindowTool {
    pub fn new(browser: Arc<BrowserSession>) -> Self {
        Self { browser }
    }
}

#[async_trait]
impl Tool for MaximizeWindowTool {
    fn name(&self) -> &str {
        "browser_maximize_window"
    }

    fn description(&self) -> &str {
        "Maximize the browser window."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {}
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        self.browser.maximize_window().await?;

        Ok(json!({
            "success": true,
            "message": "Window maximized"
        }))
    }
}

/// Tool for minimizing the window.
pub struct MinimizeWindowTool {
    browser: Arc<BrowserSession>,
}

impl MinimizeWindowTool {
    pub fn new(browser: Arc<BrowserSession>) -> Self {
        Self { browser }
    }
}

#[async_trait]
impl Tool for MinimizeWindowTool {
    fn name(&self) -> &str {
        "browser_minimize_window"
    }

    fn description(&self) -> &str {
        "Minimize the browser window."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {}
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        self.browser.minimize_window().await?;

        Ok(json!({
            "success": true,
            "message": "Window minimized"
        }))
    }
}

/// Tool for setting window size.
pub struct SetWindowSizeTool {
    browser: Arc<BrowserSession>,
}

impl SetWindowSizeTool {
    pub fn new(browser: Arc<BrowserSession>) -> Self {
        Self { browser }
    }
}

#[async_trait]
impl Tool for SetWindowSizeTool {
    fn name(&self) -> &str {
        "browser_set_window_size"
    }

    fn description(&self) -> &str {
        "Set the browser window size and position."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "width": {
                    "type": "integer",
                    "description": "Window width in pixels"
                },
                "height": {
                    "type": "integer",
                    "description": "Window height in pixels"
                },
                "x": {
                    "type": "integer",
                    "description": "Window X position (default: 0)"
                },
                "y": {
                    "type": "integer",
                    "description": "Window Y position (default: 0)"
                }
            },
            "required": ["width", "height"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let width = args
            .get("width")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| AdkError::Tool("Missing 'width' parameter".to_string()))?
            as u32;

        let height = args
            .get("height")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| AdkError::Tool("Missing 'height' parameter".to_string()))?
            as u32;

        let x = args.get("x").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
        let y = args.get("y").and_then(|v| v.as_i64()).unwrap_or(0) as i32;

        self.browser.set_window_rect(x, y, width, height).await?;

        Ok(json!({
            "success": true,
            "width": width,
            "height": height,
            "x": x,
            "y": y
        }))
    }
}
