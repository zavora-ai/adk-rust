//! Advanced action tools (drag-drop, context-click, focus, etc.)

use crate::session::BrowserSession;
use adk_core::{AdkError, Result, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;

/// Tool for drag and drop operations.
pub struct DragAndDropTool {
    browser: Arc<BrowserSession>,
}

impl DragAndDropTool {
    pub fn new(browser: Arc<BrowserSession>) -> Self {
        Self { browser }
    }
}

#[async_trait]
impl Tool for DragAndDropTool {
    fn name(&self) -> &str {
        "browser_drag_and_drop"
    }

    fn description(&self) -> &str {
        "Drag an element and drop it onto another element."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "source_selector": {
                    "type": "string",
                    "description": "CSS selector for the element to drag"
                },
                "target_selector": {
                    "type": "string",
                    "description": "CSS selector for the drop target"
                }
            },
            "required": ["source_selector", "target_selector"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let source = args
            .get("source_selector")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AdkError::Tool("Missing 'source_selector' parameter".to_string()))?;

        let target = args
            .get("target_selector")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AdkError::Tool("Missing 'target_selector' parameter".to_string()))?;

        self.browser.drag_and_drop(source, target).await?;

        Ok(json!({
            "success": true,
            "dragged_from": source,
            "dropped_on": target
        }))
    }
}

/// Tool for right-click (context click).
pub struct RightClickTool {
    browser: Arc<BrowserSession>,
}

impl RightClickTool {
    pub fn new(browser: Arc<BrowserSession>) -> Self {
        Self { browser }
    }
}

#[async_trait]
impl Tool for RightClickTool {
    fn name(&self) -> &str {
        "browser_right_click"
    }

    fn description(&self) -> &str {
        "Right-click (context click) on an element to open context menu."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "selector": {
                    "type": "string",
                    "description": "CSS selector for the element to right-click"
                }
            },
            "required": ["selector"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let selector = args
            .get("selector")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AdkError::Tool("Missing 'selector' parameter".to_string()))?;

        self.browser.right_click(selector).await?;

        Ok(json!({
            "success": true,
            "right_clicked": selector
        }))
    }
}

/// Tool for focusing an element.
pub struct FocusTool {
    browser: Arc<BrowserSession>,
}

impl FocusTool {
    pub fn new(browser: Arc<BrowserSession>) -> Self {
        Self { browser }
    }
}

#[async_trait]
impl Tool for FocusTool {
    fn name(&self) -> &str {
        "browser_focus"
    }

    fn description(&self) -> &str {
        "Focus on an element (useful for inputs before typing)."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "selector": {
                    "type": "string",
                    "description": "CSS selector for the element to focus"
                }
            },
            "required": ["selector"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let selector = args
            .get("selector")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AdkError::Tool("Missing 'selector' parameter".to_string()))?;

        self.browser.focus_element(selector).await?;

        Ok(json!({
            "success": true,
            "focused": selector
        }))
    }
}

/// Tool for checking element state (visible, enabled, selected).
pub struct ElementStateTool {
    browser: Arc<BrowserSession>,
}

impl ElementStateTool {
    pub fn new(browser: Arc<BrowserSession>) -> Self {
        Self { browser }
    }
}

#[async_trait]
impl Tool for ElementStateTool {
    fn name(&self) -> &str {
        "browser_element_state"
    }

    fn description(&self) -> &str {
        "Check the state of an element (displayed, enabled, selected, clickable)."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "selector": {
                    "type": "string",
                    "description": "CSS selector for the element"
                }
            },
            "required": ["selector"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let selector = args
            .get("selector")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AdkError::Tool("Missing 'selector' parameter".to_string()))?;

        let state = self.browser.get_element_state(selector).await?;

        Ok(json!({
            "success": true,
            "selector": selector,
            "is_displayed": state.is_displayed,
            "is_enabled": state.is_enabled,
            "is_selected": state.is_selected,
            "is_clickable": state.is_clickable
        }))
    }
}

/// Tool for pressing keyboard keys.
pub struct PressKeyTool {
    browser: Arc<BrowserSession>,
}

impl PressKeyTool {
    pub fn new(browser: Arc<BrowserSession>) -> Self {
        Self { browser }
    }
}

#[async_trait]
impl Tool for PressKeyTool {
    fn name(&self) -> &str {
        "browser_press_key"
    }

    fn description(&self) -> &str {
        "Press a keyboard key (Enter, Escape, Tab, etc.) optionally on a specific element."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "key": {
                    "type": "string",
                    "description": "Key to press: Enter, Escape, Tab, Backspace, Delete, ArrowUp, ArrowDown, ArrowLeft, ArrowRight, etc."
                },
                "selector": {
                    "type": "string",
                    "description": "Optional CSS selector for the target element"
                },
                "modifiers": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional modifier keys: Ctrl, Alt, Shift, Meta"
                }
            },
            "required": ["key"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let key = args
            .get("key")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AdkError::Tool("Missing 'key' parameter".to_string()))?;

        let selector = args.get("selector").and_then(|v| v.as_str());
        let modifiers: Vec<&str> = args
            .get("modifiers")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();

        self.browser.press_key(key, selector, &modifiers).await?;

        Ok(json!({
            "success": true,
            "key_pressed": key,
            "modifiers": modifiers,
            "target": selector
        }))
    }
}

/// Tool for uploading files.
pub struct FileUploadTool {
    browser: Arc<BrowserSession>,
}

impl FileUploadTool {
    pub fn new(browser: Arc<BrowserSession>) -> Self {
        Self { browser }
    }
}

#[async_trait]
impl Tool for FileUploadTool {
    fn name(&self) -> &str {
        "browser_file_upload"
    }

    fn description(&self) -> &str {
        "Upload a file to a file input element."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "selector": {
                    "type": "string",
                    "description": "CSS selector for the file input element"
                },
                "file_path": {
                    "type": "string",
                    "description": "Absolute path to the file to upload"
                }
            },
            "required": ["selector", "file_path"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let selector = args
            .get("selector")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AdkError::Tool("Missing 'selector' parameter".to_string()))?;

        let file_path = args
            .get("file_path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AdkError::Tool("Missing 'file_path' parameter".to_string()))?;

        self.browser.upload_file(selector, file_path).await?;

        Ok(json!({
            "success": true,
            "uploaded_file": file_path,
            "to_element": selector
        }))
    }
}

/// Tool for printing page to PDF.
pub struct PrintToPdfTool {
    browser: Arc<BrowserSession>,
}

impl PrintToPdfTool {
    pub fn new(browser: Arc<BrowserSession>) -> Self {
        Self { browser }
    }
}

#[async_trait]
impl Tool for PrintToPdfTool {
    fn name(&self) -> &str {
        "browser_print_to_pdf"
    }

    fn description(&self) -> &str {
        "Print the current page to PDF and return as base64."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "landscape": {
                    "type": "boolean",
                    "description": "Print in landscape orientation (default: false)"
                },
                "scale": {
                    "type": "number",
                    "description": "Scale factor (default: 1.0)"
                }
            }
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let landscape = args.get("landscape").and_then(|v| v.as_bool()).unwrap_or(false);
        let scale = args.get("scale").and_then(|v| v.as_f64()).unwrap_or(1.0);

        let pdf_base64 = self.browser.print_to_pdf(landscape, scale).await?;

        Ok(json!({
            "success": true,
            "pdf_base64": pdf_base64
        }))
    }
}
