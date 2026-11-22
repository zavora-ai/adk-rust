//! A [`adk_core::ComputerActuator`] over the plain tools of a `computer-use-mcp` server.

use std::collections::HashSet;
use std::sync::Arc;

use adk_core::{
    ActuationOutput, AdkError, ComputerAction, ComputerActionKind, ComputerActuator, ErrorCategory,
    ErrorComponent, MouseButton, Result, ScreenPoint, Screenshot, ScrollDirection, Toolset,
};
use adk_tool::{McpToolset, SimpleToolContext};
use async_trait::async_trait;
use rmcp::{RoleClient, service::Service};
use serde_json::{Value, json};
use tokio::sync::RwLock;
use tracing::Instrument;

/// The screenshot width [`McpComputerActuator`] asks for unless told otherwise.
pub const DEFAULT_SCREENSHOT_WIDTH: u32 = 1280;

/// The longest `hold_key` computer-use-mcp performs, in seconds.
const MAX_HOLD_SECONDS: f64 = 10.0;

/// How screenshot pixels map to the server's logical screen coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Mapping {
    origin_x: f64,
    origin_y: f64,
    scale_x: f64,
    scale_y: f64,
}

impl Mapping {
    fn to_screen(self, point: ScreenPoint) -> [f64; 2] {
        [
            self.origin_x + point.x as f64 * self.scale_x,
            self.origin_y + point.y as f64 * self.scale_y,
        ]
    }

    fn to_image(self, [x, y]: [f64; 2]) -> ScreenPoint {
        ScreenPoint::new(
            ((x - self.origin_x) / self.scale_x).round() as i64,
            ((y - self.origin_y) / self.scale_y).round() as i64,
        )
    }

    /// The mapping a screenshot reply states, one line per axis:
    /// `screen_x = <origin> + image_x * <scale>`, or `screen_x = image_x * <scale>`.
    fn parse(text: &str) -> Option<Self> {
        let axis = |name: &str| -> Option<(f64, f64)> {
            let prefix = format!("screen_{name} = ");
            let rest = text.lines().find_map(|line| line.trim().strip_prefix(prefix.as_str()))?;
            let (origin, rest) = match rest.split_once(" + ") {
                Some((origin, rest)) => (origin.trim().parse::<f64>().ok()?, rest),
                None => (0.0, rest),
            };
            let scale = rest
                .trim()
                .strip_prefix(format!("image_{name} * ").as_str())?
                .parse::<f64>()
                .ok()?;
            (origin.is_finite() && scale.is_finite() && scale > 0.0).then_some((origin, scale))
        };
        let (origin_x, scale_x) = axis("x")?;
        let (origin_y, scale_y) = axis("y")?;
        Some(Self { origin_x, origin_y, scale_x, scale_y })
    }
}

/// Builds an [`McpComputerActuator`]; see [`McpComputerActuator::builder`].
pub struct McpComputerActuatorBuilder<S = ()>
where
    S: Service<RoleClient> + Send + Sync + 'static,
{
    toolset: Arc<McpToolset<S>>,
    screenshot_width: u32,
    display_id: Option<i64>,
}

impl<S> McpComputerActuatorBuilder<S>
where
    S: Service<RoleClient> + Send + Sync + 'static,
{
    /// Sets the width of every screenshot, in pixels; the height follows the display's aspect
    /// ratio. [`DEFAULT_SCREENSHOT_WIDTH`] by default.
    pub fn screenshot_width(mut self, width: u32) -> Self {
        self.screenshot_width = width.max(1);
        self
    }

    /// Captures display `display_id` (from the server's `list_displays`) instead of the main
    /// display.
    pub fn display_id(mut self, display_id: i64) -> Self {
        self.display_id = Some(display_id);
        self
    }

    /// Lists the server's tools and takes one screenshot, which fixes the display size and the
    /// coordinate mapping.
    ///
    /// # Errors
    ///
    /// Returns an error when the server lists no `screenshot` tool, when the call fails, or
    /// when the screenshot is not a PNG that states its coordinate mapping.
    pub async fn connect(self) -> Result<McpComputerActuator<S>> {
        let tools = self
            .toolset
            .tools(Arc::new(SimpleToolContext::new("computer-use-actuator")))
            .await?
            .iter()
            .map(|tool| tool.name().to_string())
            .collect::<HashSet<_>>();
        if !tools.contains("screenshot") {
            return Err(AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::Unsupported,
                "computer_use.mcp.no_screenshot",
                "the MCP server lists no `screenshot` tool; connect McpComputerActuator to a \
                 computer-use-mcp server, whose every profile lists it",
            ));
        }
        let actuator = McpComputerActuator {
            toolset: self.toolset,
            tools,
            screenshot_width: self.screenshot_width,
            display_id: self.display_id,
            display: (0, 0),
            mapping: RwLock::new(Mapping {
                origin_x: 0.0,
                origin_y: 0.0,
                scale_x: 1.0,
                scale_y: 1.0,
            }),
        };
        let shot = actuator.screenshot().await?;
        Ok(McpComputerActuator { display: (shot.width, shot.height), ..actuator })
    }
}

/// A [`ComputerActuator`] that acts through the plain tools of a
/// [`computer-use-mcp`](https://github.com/zavora-ai/computer-use-mcp) server: `screenshot`,
/// `click`, `mouse_move`, `scroll`, `type`, `key`, `hold_key` and `cursor_position`, plus
/// `mouse_drag` or `left_click_drag` and `left_mouse_down`/`left_mouse_up` where the server's
/// profile lists them (`ax` and above).
///
/// Give it to [`ComputerUseToolset`](crate::ComputerUseToolset), so every action passes the
/// governed tool path — tool policy, confirmations, the kill switch, budgets and the action
/// ledger — before the server sees it. The server keeps its own guards: a person's input, Pause
/// and Stop on its status card, and its policy refuse an action, and the refusal reaches the
/// model as the tool's error.
///
/// Screenshots cover a whole display at [`screenshot_width`](McpComputerActuatorBuilder::screenshot_width)
/// pixels. Each screenshot reply states how its pixels map to screen points, and the actuator
/// converts every coordinate with the latest mapping, so the model works in screenshot pixels
/// as [`ComputerActuator`] requires. Zoom is not offered, and the toolset performs waits itself.
///
/// # Example
///
/// ```no_run
/// use std::sync::Arc;
/// use adk_computer_use::{ComputerUseToolset, McpComputerActuator};
/// use adk_tool::McpToolset;
///
/// async fn computer_tools(server: Arc<McpToolset>) -> adk_core::Result<ComputerUseToolset> {
///     let actuator = McpComputerActuator::builder(server).screenshot_width(1280).connect().await?;
///     Ok(ComputerUseToolset::new(Arc::new(actuator)))
/// }
/// ```
pub struct McpComputerActuator<S = ()>
where
    S: Service<RoleClient> + Send + Sync + 'static,
{
    toolset: Arc<McpToolset<S>>,
    tools: HashSet<String>,
    screenshot_width: u32,
    display_id: Option<i64>,
    display: (u32, u32),
    mapping: RwLock<Mapping>,
}

impl<S> McpComputerActuator<S>
where
    S: Service<RoleClient> + Send + Sync + 'static,
{
    /// A builder for an actuator over `toolset`, a connected computer-use-mcp server.
    pub fn builder(toolset: Arc<McpToolset<S>>) -> McpComputerActuatorBuilder<S> {
        McpComputerActuatorBuilder {
            toolset,
            screenshot_width: DEFAULT_SCREENSHOT_WIDTH,
            display_id: None,
        }
    }

    /// Calls `tool` and returns its result, or the server's error as an error.
    async fn call(&self, tool: &str, arguments: Value) -> Result<Value> {
        let Value::Object(arguments) = arguments else {
            return Err(invalid(format!("arguments for `{tool}` must be a JSON object")));
        };
        let value = self
            .toolset
            .call_tool_value(tool, arguments)
            .instrument(tracing::info_span!("computer_use.mcp_actuator", mcp.tool = tool))
            .await?;
        match value.get("error") {
            Some(error) => Err(failed(tool, error.as_str().unwrap_or(&error.to_string()))),
            None => Ok(value),
        }
    }

    async fn screenshot(&self) -> Result<Screenshot> {
        let mut arguments = json!({
            "full_screen": true,
            "width": self.screenshot_width,
            "quality": 0,
            // The server answers an unchanged screen with text unless asked for the image.
            "resend": true,
        });
        if let Some(display_id) = self.display_id {
            arguments["display_id"] = json!(display_id);
        }
        let value = self.call("screenshot", arguments).await?;
        let image = value
            .get("inline_data")
            .and_then(Value::as_array)
            .and_then(|parts| {
                parts.iter().find(|part| {
                    part.get("mime_type")
                        .and_then(Value::as_str)
                        .is_some_and(|mime| mime.starts_with("image/"))
                })
            })
            .ok_or_else(|| failed("screenshot", "the reply carried no image"))?;
        let data = serde_json::from_value::<Vec<u8>>(image["data"].clone()).map_err(|error| {
            failed("screenshot", format!("the image bytes are invalid: {error}"))
        })?;
        let shot = Screenshot::png(data).ok_or_else(|| {
            failed("screenshot", "the image is not a PNG; quality 0 asks for one")
        })?;
        let text = value.pointer("/response/output").and_then(Value::as_str).unwrap_or_default();
        let mapping = Mapping::parse(text).ok_or_else(|| {
            failed(
                "screenshot",
                "the reply states no `screen_x = ... image_x * ...` mapping; computer-use-mcp \
                 7.4 and later state one",
            )
        })?;
        *self.mapping.write().await = mapping;
        Ok(shot)
    }

    /// The cursor position in screen points.
    async fn cursor(&self) -> Result<[f64; 2]> {
        let value = self.call("cursor_position", json!({})).await?;
        let text = value.get("output").and_then(Value::as_str).unwrap_or_default();
        text.trim()
            .strip_prefix('(')
            .and_then(|rest| rest.strip_suffix(')'))
            .and_then(|pair| pair.split_once(','))
            .and_then(|(x, y)| Some([x.trim().parse().ok()?, y.trim().parse().ok()?]))
            .ok_or_else(|| failed("cursor_position", format!("unreadable position {text:?}")))
    }

    /// `at` in screen points, or the cursor position when `at` is `None`.
    async fn screen_point(&self, at: Option<ScreenPoint>) -> Result<[f64; 2]> {
        match at {
            Some(point) => Ok(self.mapping.read().await.to_screen(point)),
            None if self.tools.contains("cursor_position") => self.cursor().await,
            None => Err(unsupported("acting at the cursor needs the `cursor_position` tool")),
        }
    }

    async fn act(&self, tool: &str, arguments: Value) -> Result<ActuationOutput> {
        self.call(tool, arguments).await.map(|_| ActuationOutput::Done)
    }
}

#[async_trait]
impl<S> ComputerActuator for McpComputerActuator<S>
where
    S: Service<RoleClient> + Send + Sync + 'static,
{
    fn display_size(&self) -> Option<(u32, u32)> {
        Some(self.display)
    }

    fn supports(&self, kind: ComputerActionKind) -> bool {
        let listed = |tool: &str| self.tools.contains(tool);
        match kind {
            ComputerActionKind::Screenshot => true,
            ComputerActionKind::Click => listed("click"),
            ComputerActionKind::Drag => listed("mouse_drag") || listed("left_click_drag"),
            ComputerActionKind::MoveTo => listed("mouse_move"),
            ComputerActionKind::MouseDown => listed("left_mouse_down") && listed("cursor_position"),
            ComputerActionKind::MouseUp => listed("left_mouse_up") && listed("cursor_position"),
            ComputerActionKind::CursorPosition => listed("cursor_position"),
            ComputerActionKind::Scroll => listed("scroll"),
            ComputerActionKind::Type => listed("type"),
            ComputerActionKind::Key => listed("key"),
            ComputerActionKind::HoldKey => listed("hold_key"),
            // Zoom returns the region unscaled; the toolset performs waits itself.
            ComputerActionKind::Zoom | ComputerActionKind::Wait => false,
            _ => false,
        }
    }

    async fn perform(&self, action: ComputerAction) -> Result<ActuationOutput> {
        if !self.supports(action.kind()) {
            return Err(unsupported(format!(
                "the server lists no tool for {:?} actions",
                action.kind()
            )));
        }
        match action {
            ComputerAction::Screenshot => self.screenshot().await.map(ActuationOutput::Image),
            ComputerAction::Click { button, count, at, modifiers } => {
                let mut arguments = json!({
                    "coordinate": self.screen_point(at).await?,
                    "button": button_name(button),
                    "count": count,
                });
                with_modifiers(&mut arguments, &modifiers)?;
                self.act("click", arguments).await
            }
            ComputerAction::Drag { from, to, modifiers } => {
                let mapping = *self.mapping.read().await;
                let (from, to) = (mapping.to_screen(from), mapping.to_screen(to));
                if self.tools.contains("mouse_drag") {
                    let mut arguments = json!({ "path": [from, to], "button": "left" });
                    with_modifiers(&mut arguments, &modifiers)?;
                    self.act("mouse_drag", arguments).await
                } else if modifiers.is_empty() {
                    self.act(
                        "left_click_drag",
                        json!({ "start_coordinate": from, "coordinate": to }),
                    )
                    .await
                } else {
                    Err(unsupported(
                        "a drag with modifiers needs the `mouse_drag` tool (the `ax` profile)",
                    ))
                }
            }
            ComputerAction::MoveTo { at } => {
                let coordinate = self.mapping.read().await.to_screen(at);
                self.act("mouse_move", json!({ "coordinate": coordinate })).await
            }
            ComputerAction::MouseDown => {
                let coordinate = self.cursor().await?;
                self.act("left_mouse_down", json!({ "coordinate": coordinate })).await
            }
            ComputerAction::MouseUp => {
                let coordinate = self.cursor().await?;
                self.act("left_mouse_up", json!({ "coordinate": coordinate })).await
            }
            ComputerAction::CursorPosition => {
                let position = self.cursor().await?;
                Ok(ActuationOutput::Cursor(self.mapping.read().await.to_image(position)))
            }
            ComputerAction::Scroll { at, direction, amount, modifiers } => {
                let mut arguments = json!({
                    "coordinate": self.screen_point(at).await?,
                    "direction": direction_name(direction),
                    "amount": amount,
                });
                with_modifiers(&mut arguments, &modifiers)?;
                self.act("scroll", arguments).await
            }
            ComputerAction::Type { text } => self.act("type", json!({ "text": text })).await,
            ComputerAction::Key { keys, repeat } => {
                let mut arguments = json!({ "text": keys });
                if repeat > 1 {
                    arguments["repeat"] = json!(repeat);
                }
                self.act("key", arguments).await
            }
            ComputerAction::HoldKey { keys, duration } => {
                let seconds = duration.as_secs_f64();
                if !(seconds > 0.0 && seconds <= MAX_HOLD_SECONDS) {
                    return Err(invalid(format!(
                        "hold_key lasts {seconds} s; computer-use-mcp holds keys for more than 0 \
                         and at most {MAX_HOLD_SECONDS} s"
                    )));
                }
                let keys: Vec<&str> = keys.split('+').map(str::trim).collect();
                self.act("hold_key", json!({ "keys": keys, "duration": seconds })).await
            }
            other => Err(unsupported(format!("{:?} actions are not offered", other.kind()))),
        }
    }
}

fn button_name(button: MouseButton) -> &'static str {
    match button {
        MouseButton::Left => "left",
        MouseButton::Right => "right",
        MouseButton::Middle => "middle",
    }
}

fn direction_name(direction: ScrollDirection) -> &'static str {
    match direction {
        ScrollDirection::Up => "up",
        ScrollDirection::Down => "down",
        ScrollDirection::Left => "left",
        ScrollDirection::Right => "right",
    }
}

/// Adds `modifiers`, in the server's names, to `arguments`.
fn with_modifiers(arguments: &mut Value, modifiers: &[String]) -> Result<()> {
    if modifiers.is_empty() {
        return Ok(());
    }
    let names = modifiers
        .iter()
        .map(|modifier| match modifier.to_ascii_lowercase().as_str() {
            "shift" => Ok("shift"),
            "ctrl" | "control" => Ok("ctrl"),
            "alt" | "option" => Ok("alt"),
            "super" | "cmd" | "command" | "meta" | "win" => Ok("cmd"),
            other => Err(invalid(format!(
                "unknown modifier {other:?}; expected shift, ctrl, alt or super"
            ))),
        })
        .collect::<Result<Vec<_>>>()?;
    arguments["modifiers"] = json!(names);
    Ok(())
}

fn failed(tool: &str, detail: impl std::fmt::Display) -> AdkError {
    AdkError::new(
        ErrorComponent::Tool,
        ErrorCategory::InvalidInput,
        "computer_use.mcp.action_failed",
        format!("computer-use-mcp `{tool}` failed: {detail}"),
    )
}

fn invalid(message: impl Into<String>) -> AdkError {
    AdkError::new(
        ErrorComponent::Tool,
        ErrorCategory::InvalidInput,
        "computer_use.mcp.invalid_action",
        message,
    )
}

fn unsupported(message: impl Into<String>) -> AdkError {
    AdkError::new(
        ErrorComponent::Tool,
        ErrorCategory::Unsupported,
        "computer_use.mcp.unsupported_action",
        message,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_both_mapping_forms() {
        let display = "1280x800 | screen: display 1 at 0,0 sized 2560x1600 (ScreenCaptureKit)\n\
                       screen_x = 0 + image_x * 2.0000\n\
                       screen_y = 0 + image_y * 2.0000\n\
                       a display capture scales uniformly, so this mapping is exact.";
        assert_eq!(
            Mapping::parse(display),
            Some(Mapping { origin_x: 0.0, origin_y: 0.0, scale_x: 2.0, scale_y: 2.0 })
        );
        let offset = "screen_x = -1512 + image_x * 1.1813\nscreen_y = 25 + image_y * 1.1813";
        assert_eq!(
            Mapping::parse(offset),
            Some(Mapping { origin_x: -1512.0, origin_y: 25.0, scale_x: 1.1813, scale_y: 1.1813 })
        );
        let bare = "screen_x = image_x * 1.5000\nscreen_y = image_y * 1.5000";
        assert_eq!(
            Mapping::parse(bare),
            Some(Mapping { origin_x: 0.0, origin_y: 0.0, scale_x: 1.5, scale_y: 1.5 })
        );
        assert_eq!(Mapping::parse("screen_x = 0 + image_x * 0\nscreen_y = 0 + image_y * 1"), None);
        assert_eq!(Mapping::parse("1280x800"), None);
    }

    #[test]
    fn maps_points_both_ways() {
        let mapping = Mapping { origin_x: -1512.0, origin_y: 25.0, scale_x: 2.0, scale_y: 2.0 };
        assert_eq!(mapping.to_screen(ScreenPoint::new(100, 50)), [-1312.0, 125.0]);
        assert_eq!(mapping.to_image([-1312.0, 125.0]), ScreenPoint::new(100, 50));
    }
}
