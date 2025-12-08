//! Screenshot tool for capturing page images.

use crate::session::BrowserSession;
use adk_core::{Result, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;

/// Tool for taking screenshots of the page.
pub struct ScreenshotTool {
    browser: Arc<BrowserSession>,
}

impl ScreenshotTool {
    /// Create a new screenshot tool with a shared browser session.
    pub fn new(browser: Arc<BrowserSession>) -> Self {
        Self { browser }
    }
}

#[async_trait]
impl Tool for ScreenshotTool {
    fn name(&self) -> &str {
        "browser_screenshot"
    }

    fn description(&self) -> &str {
        "Take a screenshot of the current page or a specific element. Returns base64-encoded PNG image."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "selector": {
                    "type": "string",
                    "description": "Optional CSS selector to screenshot a specific element. If not provided, captures the full page."
                },
                "save_to_artifacts": {
                    "type": "boolean",
                    "description": "Whether to save the screenshot to artifacts (default: false)"
                },
                "artifact_name": {
                    "type": "string",
                    "description": "Name for the artifact if saving (default: 'screenshot.png')"
                }
            }
        }))
    }

    fn response_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "success": { "type": "boolean" },
                "base64_image": { "type": "string" },
                "saved_to_artifacts": { "type": "boolean" },
                "artifact_name": { "type": "string" }
            }
        }))
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let selector = args.get("selector").and_then(|v| v.as_str());
        let save_to_artifacts =
            args.get("save_to_artifacts").and_then(|v| v.as_bool()).unwrap_or(false);
        let artifact_name =
            args.get("artifact_name").and_then(|v| v.as_str()).unwrap_or("screenshot.png");

        // Take screenshot
        let base64_image = if let Some(sel) = selector {
            self.browser.screenshot_element(sel).await?
        } else {
            self.browser.screenshot().await?
        };

        // Optionally save to artifacts
        let mut saved = false;
        if save_to_artifacts {
            if let Some(artifacts) = ctx.artifacts() {
                use base64::Engine;
                let image_data =
                    base64::engine::general_purpose::STANDARD.decode(&base64_image).map_err(
                        |e| adk_core::AdkError::Tool(format!("Failed to decode base64: {}", e)),
                    )?;

                let part = adk_core::Part::InlineData {
                    mime_type: "image/png".to_string(),
                    data: image_data,
                };

                artifacts.save(artifact_name, &part).await?;
                saved = true;
            }
        }

        Ok(json!({
            "success": true,
            "base64_image": base64_image,
            "saved_to_artifacts": saved,
            "artifact_name": if saved { Some(artifact_name) } else { None }
        }))
    }
}
