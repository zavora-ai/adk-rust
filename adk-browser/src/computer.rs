//! A browser viewport as a [`ComputerActuator`].

use crate::session::BrowserSession;
use crate::viewport::{self, Viewport};
use adk_core::{ActuationOutput, ComputerAction, ComputerActionKind, ComputerActuator, Result};
use async_trait::async_trait;
use std::sync::Arc;

/// A [`ComputerActuator`] whose display is a WebDriver browser viewport.
///
/// Screenshots are the viewport, resized when needed to fit Claude's image limits, and every
/// coordinate is in their pixel space; the actuator maps it to CSS pixels, device pixel ratio
/// included. It supports every action, `zoom` included. Run the browser headless, in an
/// isolated profile, behind the network controls the task needs.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use adk_browser::{BrowserConfig, BrowserSession, WebDriverComputer};
/// use adk_core::ComputerActuator;
///
/// let browser = Arc::new(BrowserSession::new(BrowserConfig::new().viewport(1280, 800)));
/// let computer = WebDriverComputer::new(browser);
/// assert_eq!(computer.display_size(), Some((1280, 800)));
/// ```
pub struct WebDriverComputer {
    viewport: Viewport,
    display: (u32, u32),
}

impl WebDriverComputer {
    /// The actuator over `browser`'s viewport, which starts on the first action.
    pub fn new(browser: Arc<BrowserSession>) -> Self {
        let config = browser.config();
        let display = viewport::fit(config.viewport_width, config.viewport_height);
        Self { viewport: Viewport::new(browser), display }
    }

    /// The browser session the actuator drives.
    pub fn session(&self) -> &Arc<BrowserSession> {
        self.viewport.session()
    }
}

#[async_trait]
impl ComputerActuator for WebDriverComputer {
    fn display_size(&self) -> Option<(u32, u32)> {
        Some(self.display)
    }

    fn supports(&self, _kind: ComputerActionKind) -> bool {
        true
    }

    async fn perform(&self, action: ComputerAction) -> Result<ActuationOutput> {
        let viewport = &self.viewport;
        match action {
            ComputerAction::Screenshot => {
                return Ok(ActuationOutput::Image(viewport.screenshot().await?));
            }
            ComputerAction::Zoom { region } => {
                return Ok(ActuationOutput::Image(viewport.zoom(region).await?));
            }
            ComputerAction::CursorPosition => {
                return Ok(ActuationOutput::Cursor(viewport.cursor()));
            }
            ComputerAction::Click { button, count, at, modifiers } => {
                viewport.click(at, button, count, &viewport::modifiers(&modifiers)?).await?;
            }
            ComputerAction::Drag { from, to, modifiers } => {
                viewport.drag(from, to, &viewport::modifiers(&modifiers)?).await?;
            }
            ComputerAction::MoveTo { at } => viewport.move_to(at).await?,
            ComputerAction::MouseDown => viewport.button(None, true).await?,
            ComputerAction::MouseUp => viewport.button(None, false).await?,
            ComputerAction::Scroll { at, direction, amount, modifiers: _ } => {
                viewport.scroll(at, direction, amount).await?;
            }
            ComputerAction::Type { text } => viewport.type_text(&text).await?,
            ComputerAction::Key { keys, repeat } => viewport.press(&keys, repeat).await?,
            ComputerAction::HoldKey { keys, duration } => viewport.hold(&keys, duration).await?,
            ComputerAction::Wait { duration } => tokio::time::sleep(duration).await,
            other => {
                return Err(adk_core::AdkError::tool(format!(
                    "Error: the browser computer does not perform {:?}",
                    other.kind()
                )));
            }
        }
        Ok(ActuationOutput::Done)
    }
}
