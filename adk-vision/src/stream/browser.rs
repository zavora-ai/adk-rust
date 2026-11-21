//! [`BrowserSource`]: `browser_screenshot` at an interval.

use std::sync::Arc;
use std::time::Duration;

use adk_core::{Tool, ToolContext};
use async_trait::async_trait;
use serde_json::{Value, json};

use super::capture::{Capture, interval_frames, observation_image};
use super::{FrameSource, FrameStream};
use crate::error::Result;
use crate::media::FrameSourceId;
use crate::traits::Sampling;

struct ScreenshotTool {
    tool: Arc<dyn Tool>,
    ctx: Arc<dyn ToolContext>,
    selector: Option<String>,
}

#[async_trait]
impl Capture for ScreenshotTool {
    async fn capture(&self) -> Result<Vec<u8>> {
        let args = match &self.selector {
            Some(selector) => json!({ "selector": selector }),
            None => Value::Object(serde_json::Map::new()),
        };
        let result = self.tool.execute(Arc::clone(&self.ctx), args).await?;
        Ok(observation_image(&result)?.0)
    }
}

/// A browser page, or one element of it, captured by `adk-browser`'s `browser_screenshot`
/// tool at an interval.
///
/// The tool is called directly with the context given, never with `save_to_artifacts`, so
/// screenshots stay ephemeral like every other frame; taking one changes nothing on the
/// page. `adk-vision` takes the tool as an `adk_core::Tool`, so it does not depend on
/// `adk-browser`.
///
/// # Example
///
/// ```
/// use std::sync::Arc;
/// use adk_vision::stream::{BrowserSource, FrameSource};
/// use adk_vision::FrameSourceId;
///
/// fn watch(screenshot: Arc<dyn adk_core::Tool>, ctx: Arc<dyn adk_core::ToolContext>) -> BrowserSource {
///     BrowserSource::new(FrameSourceId("crm".into()), screenshot, ctx).with_selector("#queue")
/// }
/// ```
pub struct BrowserSource {
    id: FrameSourceId,
    tool: Arc<dyn Tool>,
    ctx: Arc<dyn ToolContext>,
    selector: Option<String>,
    interval: Duration,
}

impl std::fmt::Debug for BrowserSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrowserSource")
            .field("id", &self.id)
            .field("tool", &self.tool.name())
            .field("selector", &self.selector)
            .field("interval", &self.interval)
            .finish_non_exhaustive()
    }
}

impl BrowserSource {
    /// Screenshots from `tool` (`browser_screenshot`) under `ctx`, one a second by default.
    pub fn new(id: FrameSourceId, tool: Arc<dyn Tool>, ctx: Arc<dyn ToolContext>) -> Self {
        Self { id, tool, ctx, selector: None, interval: Duration::from_secs(1) }
    }

    /// Captures only the element `selector` matches.
    pub fn with_selector(mut self, selector: impl Into<String>) -> Self {
        self.selector = Some(selector.into());
        self
    }

    /// Takes a screenshot every `interval`, or every sampling interval when that is longer.
    pub fn with_interval(mut self, interval: Duration) -> Self {
        self.interval = interval;
        self
    }
}

#[async_trait]
impl FrameSource for BrowserSource {
    fn id(&self) -> &FrameSourceId {
        &self.id
    }

    async fn frames(&self, sampling: Sampling) -> Result<FrameStream> {
        let capture = Arc::new(ScreenshotTool {
            tool: Arc::clone(&self.tool),
            ctx: Arc::clone(&self.ctx),
            selector: self.selector.clone(),
        });
        interval_frames(self.id.clone(), capture, self.interval, sampling)
    }
}
