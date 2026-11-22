//! Claude's computer use toolset as governed ADK tools over a [`adk_core::ComputerActuator`].
//!
//! Each member of `computer_toolset_20260801` is one ADK tool, `computer.<member>`, so tool
//! policy, confirmations, standing approvals, the kill switch, run budgets and the action
//! ledger decide every action a model asks for before the actuator sees it. The members share
//! one ordered batch: after a member call fails or is refused or held, the rest of the model's
//! turn is answered with the halt text instead of running.
//!
//! The toolset acts only through the actuator an application hands it; this crate ships no
//! desktop actuator, so desktop control is always an explicit choice.

use std::sync::Arc;
use std::time::Duration;

use adk_core::egress::ToolEgress;
use adk_core::{
    ActuationOutput, AdkError, BatchGroup, ComputerAction, ComputerActionKind, ComputerActuator,
    InlineDataPart, MouseButton, ReadonlyContext, Result, ScreenPoint, ScrollDirection, Tool,
    ToolContext, ToolEffect, Toolset,
};
use async_trait::async_trait;
use serde_json::{Map, Value, json};

/// The `toolset_name` and batch group of the computer use members.
pub const COMPUTER_TOOLSET: &str = "computer";

/// The exact result text for each computer use call after the first failure in a batch.
pub const COMPUTER_BATCH_HALT_TEXT: &str =
    "Not executed: an earlier computer action in this turn failed.";

/// Declaration key naming the client toolset member a tool implements.
const MEMBER_KEY: &str = "x-adk-anthropic-toolset-member";

/// The longest `wait` or `hold_key` the toolset performs.
const MAX_DURATION: Duration = Duration::from_secs(300);

/// A member of Claude's computer use toolset.
///
/// # Example
///
/// ```rust
/// use adk_computer_use::ComputerUseMember;
///
/// assert_eq!(ComputerUseMember::ALL.len(), 17);
/// assert_eq!(ComputerUseMember::LeftClick.tool_name(), "computer.left_click");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ComputerUseMember {
    /// Capture the display.
    Screenshot,
    /// Capture a region at full resolution.
    Zoom,
    /// Left-click.
    LeftClick,
    /// Right-click.
    RightClick,
    /// Middle-click.
    MiddleClick,
    /// Double-click.
    DoubleClick,
    /// Triple-click.
    TripleClick,
    /// Press, drag and release.
    LeftClickDrag,
    /// Move the cursor.
    MouseMove,
    /// Press the left button.
    LeftMouseDown,
    /// Release the left button.
    LeftMouseUp,
    /// Report the cursor position.
    CursorPosition,
    /// Scroll.
    Scroll,
    /// Type text.
    Type,
    /// Press a key or chord.
    Key,
    /// Hold a key.
    HoldKey,
    /// Pause.
    Wait,
}

impl ComputerUseMember {
    /// Every member.
    pub const ALL: &'static [Self] = &[
        Self::Screenshot,
        Self::Zoom,
        Self::LeftClick,
        Self::RightClick,
        Self::MiddleClick,
        Self::DoubleClick,
        Self::TripleClick,
        Self::LeftClickDrag,
        Self::MouseMove,
        Self::LeftMouseDown,
        Self::LeftMouseUp,
        Self::CursorPosition,
        Self::Scroll,
        Self::Type,
        Self::Key,
        Self::HoldKey,
        Self::Wait,
    ];

    /// The member's wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Screenshot => "screenshot",
            Self::Zoom => "zoom",
            Self::LeftClick => "left_click",
            Self::RightClick => "right_click",
            Self::MiddleClick => "middle_click",
            Self::DoubleClick => "double_click",
            Self::TripleClick => "triple_click",
            Self::LeftClickDrag => "left_click_drag",
            Self::MouseMove => "mouse_move",
            Self::LeftMouseDown => "left_mouse_down",
            Self::LeftMouseUp => "left_mouse_up",
            Self::CursorPosition => "cursor_position",
            Self::Scroll => "scroll",
            Self::Type => "type",
            Self::Key => "key",
            Self::HoldKey => "hold_key",
            Self::Wait => "wait",
        }
    }

    /// The ADK tool name, `computer.<member>`.
    pub fn tool_name(self) -> String {
        format!("{COMPUTER_TOOLSET}.{}", self.as_str())
    }

    /// The actuator action kind the member needs.
    pub fn action_kind(self) -> ComputerActionKind {
        match self {
            Self::Screenshot => ComputerActionKind::Screenshot,
            Self::Zoom => ComputerActionKind::Zoom,
            Self::LeftClick
            | Self::RightClick
            | Self::MiddleClick
            | Self::DoubleClick
            | Self::TripleClick => ComputerActionKind::Click,
            Self::LeftClickDrag => ComputerActionKind::Drag,
            Self::MouseMove => ComputerActionKind::MoveTo,
            Self::LeftMouseDown => ComputerActionKind::MouseDown,
            Self::LeftMouseUp => ComputerActionKind::MouseUp,
            Self::CursorPosition => ComputerActionKind::CursorPosition,
            Self::Scroll => ComputerActionKind::Scroll,
            Self::Type => ComputerActionKind::Type,
            Self::Key => ComputerActionKind::Key,
            Self::HoldKey => ComputerActionKind::HoldKey,
            Self::Wait => ComputerActionKind::Wait,
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Screenshot => "Capture the full display as an image.",
            Self::Zoom => "Capture a region of the display at full resolution.",
            Self::LeftClick => "Left-click at a coordinate or at the cursor.",
            Self::RightClick => "Right-click at a coordinate or at the cursor.",
            Self::MiddleClick => "Middle-click at a coordinate or at the cursor.",
            Self::DoubleClick => "Double-click at a coordinate or at the cursor.",
            Self::TripleClick => "Triple-click at a coordinate or at the cursor.",
            Self::LeftClickDrag => "Press at one coordinate, drag to another, and release.",
            Self::MouseMove => "Move the cursor to a coordinate.",
            Self::LeftMouseDown => "Press the left mouse button at the cursor.",
            Self::LeftMouseUp => "Release the left mouse button at the cursor.",
            Self::CursorPosition => "Report the cursor position.",
            Self::Scroll => "Scroll at a coordinate or at the cursor.",
            Self::Type => "Type text at the keyboard focus.",
            Self::Key => "Press a key or key combination.",
            Self::HoldKey => "Hold a key or key combination down for a duration.",
            Self::Wait => "Pause before the next action.",
        }
    }
}

/// Claude's computer use toolset (`computer_toolset_20260801`) over an actuator.
///
/// The toolset offers the members the actuator supports, each as a governed tool. The model
/// adapter collapses them into one toolset entry that offers exactly those members, or into
/// the earlier `computer_20251124` tool for a model that takes it, which needs the display
/// size ([`with_display`](Self::with_display), or the actuator's own).
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_computer_use::ComputerUseToolset;
/// use adk_core::{ActuationOutput, ComputerAction, ComputerActuator, Result};
/// use async_trait::async_trait;
///
/// struct NoOp;
///
/// #[async_trait]
/// impl ComputerActuator for NoOp {
///     fn display_size(&self) -> Option<(u32, u32)> {
///         Some((1280, 800))
///     }
///
///     async fn perform(&self, _action: ComputerAction) -> Result<ActuationOutput> {
///         Ok(ActuationOutput::Done)
///     }
/// }
///
/// let toolset = ComputerUseToolset::new(Arc::new(NoOp));
/// // Zoom is withheld: the actuator does not support it.
/// assert_eq!(toolset.member_tools().len(), 16);
/// ```
pub struct ComputerUseToolset {
    actuator: Arc<dyn ComputerActuator>,
    display: Option<(u32, u32)>,
    withheld: Vec<ComputerUseMember>,
    egress: ToolEgress,
}

impl ComputerUseToolset {
    /// The toolset over `actuator`, offering every member it supports.
    pub fn new(actuator: Arc<dyn ComputerActuator>) -> Self {
        let display = actuator.display_size();
        Self { actuator, display, withheld: Vec::new(), egress: ToolEgress::Undeclared }
    }

    /// Sets the display size in screenshot pixels, which bounds coordinates and is declared
    /// for models that take an earlier computer use version.
    pub fn with_display(mut self, width: u32, height: u32) -> Self {
        self.display = Some((width, height));
        self
    }

    /// Withholds `member` from the model.
    pub fn without(mut self, member: ComputerUseMember) -> Self {
        self.withheld.push(member);
        self
    }

    /// What every acting member declares about its network use. Undeclared by default, which
    /// a run egress policy refuses; declare [`ToolEgress::Enforced`] when the environment's
    /// traffic passes through a proxy that enforces the run's policy.
    pub fn with_egress(mut self, egress: ToolEgress) -> Self {
        self.egress = egress;
        self
    }

    /// The members the toolset offers.
    pub fn members(&self) -> Vec<ComputerUseMember> {
        ComputerUseMember::ALL
            .iter()
            .copied()
            .filter(|member| !self.withheld.contains(member))
            .filter(|member| {
                *member == ComputerUseMember::Wait || self.actuator.supports(member.action_kind())
            })
            .collect()
    }

    /// The member tools, one per offered member.
    pub fn member_tools(&self) -> Vec<Arc<dyn Tool>> {
        self.members()
            .into_iter()
            .map(|member| {
                Arc::new(ComputerUseTool {
                    member,
                    name: member.tool_name(),
                    actuator: self.actuator.clone(),
                    display: self.display,
                    egress: self.egress.clone(),
                }) as Arc<dyn Tool>
            })
            .collect()
    }
}

#[async_trait]
impl Toolset for ComputerUseToolset {
    fn name(&self) -> &str {
        "computer_use"
    }

    async fn tools(&self, _ctx: Arc<dyn ReadonlyContext>) -> Result<Vec<Arc<dyn Tool>>> {
        Ok(self.member_tools())
    }
}

/// One computer use member as a governed tool.
struct ComputerUseTool {
    member: ComputerUseMember,
    name: String,
    actuator: Arc<dyn ComputerActuator>,
    display: Option<(u32, u32)>,
    egress: ToolEgress,
}

fn invalid(member: ComputerUseMember, message: impl std::fmt::Display) -> AdkError {
    AdkError::tool(format!("Error: invalid input for {}: {message}", member.as_str()))
}

impl ComputerUseTool {
    fn point(&self, input: &Map<String, Value>, field: &str) -> Result<Option<ScreenPoint>> {
        let Some(value) = input.get(field) else { return Ok(None) };
        let pair = value.as_array().filter(|pair| pair.len() == 2);
        let (Some(x), Some(y)) =
            (pair.and_then(|pair| pair[0].as_i64()), pair.and_then(|pair| pair[1].as_i64()))
        else {
            return Err(invalid(
                self.member,
                format!("{field} must be [x, y] in screenshot pixels"),
            ));
        };
        if let Some((width, height)) = self.display
            && (x < 0 || y < 0 || x >= i64::from(width) || y >= i64::from(height))
        {
            return Err(AdkError::tool(format!(
                "Error: coordinate ({x}, {y}) is outside the {width}x{height} display"
            )));
        }
        Ok(Some(ScreenPoint::new(x, y)))
    }

    fn required_point(&self, input: &Map<String, Value>, field: &str) -> Result<ScreenPoint> {
        self.point(input, field)?
            .ok_or_else(|| invalid(self.member, format!("{field} is required")))
    }

    fn text(&self, input: &Map<String, Value>) -> Result<String> {
        input
            .get("text")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| invalid(self.member, "text is required"))
    }

    fn duration(&self, input: &Map<String, Value>) -> Result<Duration> {
        let seconds = input
            .get("duration")
            .and_then(Value::as_f64)
            .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
            .ok_or_else(|| invalid(self.member, "duration must be a number of seconds"))?;
        Ok(Duration::from_secs_f64(seconds).min(MAX_DURATION))
    }

    fn action(&self, input: &Map<String, Value>) -> Result<ComputerAction> {
        let modifiers = || -> Vec<String> {
            input
                .get("text")
                .and_then(Value::as_str)
                .map(|chord| {
                    chord
                        .split('+')
                        .map(str::trim)
                        .filter(|key| !key.is_empty())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default()
        };
        let click = |button: MouseButton, count: u8| -> Result<ComputerAction> {
            Ok(ComputerAction::Click {
                button,
                count,
                at: self.point(input, "coordinate")?,
                modifiers: modifiers(),
            })
        };
        Ok(match self.member {
            ComputerUseMember::Screenshot => ComputerAction::Screenshot,
            ComputerUseMember::Zoom => {
                let region: Vec<i64> = input
                    .get("region")
                    .and_then(Value::as_array)
                    .map(|region| region.iter().filter_map(Value::as_i64).collect())
                    .unwrap_or_default();
                let [x0, y0, x1, y1] = region[..] else {
                    return Err(invalid(self.member, "region must be [x0, y0, x1, y1]"));
                };
                if x1 <= x0 || y1 <= y0 {
                    return Err(invalid(
                        self.member,
                        "region must have a positive width and height",
                    ));
                }
                ComputerAction::Zoom { region: [x0, y0, x1, y1] }
            }
            ComputerUseMember::LeftClick => click(MouseButton::Left, 1)?,
            ComputerUseMember::RightClick => click(MouseButton::Right, 1)?,
            ComputerUseMember::MiddleClick => click(MouseButton::Middle, 1)?,
            ComputerUseMember::DoubleClick => click(MouseButton::Left, 2)?,
            ComputerUseMember::TripleClick => click(MouseButton::Left, 3)?,
            ComputerUseMember::LeftClickDrag => ComputerAction::Drag {
                from: self.required_point(input, "start_coordinate")?,
                to: self.required_point(input, "coordinate")?,
                modifiers: modifiers(),
            },
            ComputerUseMember::MouseMove => {
                ComputerAction::MoveTo { at: self.required_point(input, "coordinate")? }
            }
            ComputerUseMember::LeftMouseDown => ComputerAction::MouseDown,
            ComputerUseMember::LeftMouseUp => ComputerAction::MouseUp,
            ComputerUseMember::CursorPosition => ComputerAction::CursorPosition,
            ComputerUseMember::Scroll => {
                let direction = match input.get("scroll_direction").and_then(Value::as_str) {
                    Some("up") => ScrollDirection::Up,
                    Some("down") => ScrollDirection::Down,
                    Some("left") => ScrollDirection::Left,
                    Some("right") => ScrollDirection::Right,
                    _ => {
                        return Err(invalid(
                            self.member,
                            "scroll_direction must be up, down, left or right",
                        ));
                    }
                };
                let amount = input.get("scroll_amount").and_then(Value::as_u64).unwrap_or(3);
                ComputerAction::Scroll {
                    at: self.point(input, "coordinate")?,
                    direction,
                    amount: amount.clamp(1, 100) as u32,
                    modifiers: modifiers(),
                }
            }
            ComputerUseMember::Type => ComputerAction::Type { text: self.text(input)? },
            ComputerUseMember::Key => ComputerAction::Key {
                keys: self.text(input)?,
                repeat: input.get("repeat").and_then(Value::as_u64).unwrap_or(1).clamp(1, 100)
                    as u32,
            },
            ComputerUseMember::HoldKey => {
                ComputerAction::HoldKey { keys: self.text(input)?, duration: self.duration(input)? }
            }
            ComputerUseMember::Wait => ComputerAction::Wait { duration: self.duration(input)? },
        })
    }
}

#[async_trait]
impl Tool for ComputerUseTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        self.member.description()
    }

    fn declaration(&self) -> Value {
        let mut member = json!({ "toolset": COMPUTER_TOOLSET, "member": self.member.as_str() });
        if let Some((width, height)) = self.display {
            member["display_width_px"] = json!(width);
            member["display_height_px"] = json!(height);
        }
        json!({ "name": self.name, "description": self.description(), MEMBER_KEY: member })
    }

    fn is_read_only(&self) -> bool {
        self.member.action_kind().is_observation()
    }

    fn effect(&self) -> ToolEffect {
        if self.is_read_only() { ToolEffect::ReadOnly } else { ToolEffect::NonIdempotent }
    }

    fn batch_group(&self) -> Option<BatchGroup> {
        Some(BatchGroup::new(COMPUTER_TOOLSET, COMPUTER_BATCH_HALT_TEXT))
    }

    fn egress(&self, _args: &Value) -> ToolEgress {
        match self.member {
            ComputerUseMember::Screenshot
            | ComputerUseMember::Zoom
            | ComputerUseMember::CursorPosition
            | ComputerUseMember::Wait => ToolEgress::None,
            _ => self.egress.clone(),
        }
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let input = match args {
            Value::Object(input) => input,
            Value::Null => Map::new(),
            _ => return Err(invalid(self.member, "input must be an object")),
        };
        let action = self.action(&input)?;
        if let ComputerAction::Wait { duration } = action
            && !self.actuator.supports(ComputerActionKind::Wait)
        {
            tokio::time::sleep(duration).await;
            return Ok(Value::String("OK".into()));
        }
        tracing::debug!(computer.member = self.member.as_str(), "performing computer action");
        Ok(match self.actuator.perform(action).await? {
            ActuationOutput::Done => Value::String("OK".into()),
            ActuationOutput::Cursor(point) => {
                Value::String(format!("X={}, Y={}", point.x, point.y))
            }
            ActuationOutput::Image(image) => {
                let caption = if self.member == ComputerUseMember::Zoom {
                    "Zoomed region captured."
                } else {
                    "Screenshot captured."
                };
                let inline = InlineDataPart {
                    mime_type: image.mime_type,
                    data: image.data,
                    uri: None,
                    annotations: None,
                };
                json!({ "response": caption, "inline_data": [inline] })
            }
            other => {
                return Err(AdkError::tool(format!(
                    "Error: the actuator returned an unexpected result for {}: {other:?}",
                    self.member.as_str()
                )));
            }
        })
    }
}
