//! Provider-neutral computer actuation: the actions a computer-use model asks for, and the
//! trait an environment implements to perform them.
//!
//! An actuator acts only when a tool calls it, and the tools that call it are dispatched
//! through the governed tool path, so tool policy, confirmations, the kill switch, budgets
//! and the action ledger decide every action before it reaches the environment.
//!
//! Every coordinate is in the pixel space of the screenshots the actuator returns, so a
//! model that reads a screenshot names positions in the same frame the actuator acts in.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::Result;

/// A position in screenshot pixels, with the origin at the top left.
///
/// # Example
///
/// ```rust
/// use adk_core::ScreenPoint;
///
/// let point = ScreenPoint::new(512, 384);
/// assert_eq!((point.x, point.y), (512, 384));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ScreenPoint {
    /// Pixels from the left edge.
    pub x: i64,
    /// Pixels from the top edge.
    pub y: i64,
}

impl ScreenPoint {
    /// The point at `x`, `y`.
    pub fn new(x: i64, y: i64) -> Self {
        Self { x, y }
    }
}

/// A mouse button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    /// The primary button.
    Left,
    /// The secondary button.
    Right,
    /// The wheel button.
    Middle,
}

/// A scroll direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScrollDirection {
    /// Towards the top.
    Up,
    /// Towards the bottom.
    Down,
    /// Towards the left edge.
    Left,
    /// Towards the right edge.
    Right,
}

/// One action on a computer.
///
/// Key names follow the xdotool convention (`Return`, `ctrl+s`, `alt+Tab`); modifiers are
/// `shift`, `ctrl`, `alt` and `super`.
///
/// # Example
///
/// ```rust
/// use adk_core::{ComputerAction, ComputerActionKind, MouseButton, ScreenPoint};
///
/// let click = ComputerAction::Click {
///     button: MouseButton::Left,
///     count: 1,
///     at: Some(ScreenPoint::new(10, 20)),
///     modifiers: vec!["shift".into()],
/// };
/// assert_eq!(click.kind(), ComputerActionKind::Click);
/// assert!(!click.kind().is_observation());
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ComputerAction {
    /// Capture the whole display.
    Screenshot,
    /// Capture `[x0, y0, x1, y1]` at full resolution, scaled to the screenshot size.
    Zoom {
        /// Top-left and bottom-right corners.
        region: [i64; 4],
    },
    /// Press and release a button `count` times, at `at` or at the cursor.
    Click {
        /// The button.
        button: MouseButton,
        /// 1 for a click, 2 for a double click, 3 for a triple click.
        count: u8,
        /// Where to click; the cursor position when `None`.
        at: Option<ScreenPoint>,
        /// Modifier keys held during the click.
        modifiers: Vec<String>,
    },
    /// Press the left button at `from`, move to `to`, release.
    Drag {
        /// Where the drag starts.
        from: ScreenPoint,
        /// Where it ends.
        to: ScreenPoint,
        /// Modifier keys held during the drag.
        modifiers: Vec<String>,
    },
    /// Move the cursor.
    MoveTo {
        /// The new cursor position.
        at: ScreenPoint,
    },
    /// Press the left button at the cursor.
    MouseDown,
    /// Release the left button at the cursor.
    MouseUp,
    /// Report the cursor position.
    CursorPosition,
    /// Scroll `amount` wheel notches, at `at` or at the cursor.
    Scroll {
        /// Where to scroll; the cursor position when `None`.
        at: Option<ScreenPoint>,
        /// Which way.
        direction: ScrollDirection,
        /// Wheel notches.
        amount: u32,
        /// Modifier keys held while scrolling.
        modifiers: Vec<String>,
    },
    /// Type literal text at the keyboard focus.
    Type {
        /// The text.
        text: String,
    },
    /// Press a key or `+`-joined chord, `repeat` times.
    Key {
        /// The key or chord.
        keys: String,
        /// How many times.
        repeat: u32,
    },
    /// Hold a key or chord down.
    HoldKey {
        /// The key or chord.
        keys: String,
        /// How long.
        duration: Duration,
    },
    /// Pause before the next action.
    Wait {
        /// How long.
        duration: Duration,
    },
}

/// The kind of a [`ComputerAction`], without its parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ComputerActionKind {
    /// [`ComputerAction::Screenshot`].
    Screenshot,
    /// [`ComputerAction::Zoom`].
    Zoom,
    /// [`ComputerAction::Click`].
    Click,
    /// [`ComputerAction::Drag`].
    Drag,
    /// [`ComputerAction::MoveTo`].
    MoveTo,
    /// [`ComputerAction::MouseDown`].
    MouseDown,
    /// [`ComputerAction::MouseUp`].
    MouseUp,
    /// [`ComputerAction::CursorPosition`].
    CursorPosition,
    /// [`ComputerAction::Scroll`].
    Scroll,
    /// [`ComputerAction::Type`].
    Type,
    /// [`ComputerAction::Key`].
    Key,
    /// [`ComputerAction::HoldKey`].
    HoldKey,
    /// [`ComputerAction::Wait`].
    Wait,
}

impl ComputerActionKind {
    /// Whether the action only reads the screen or cursor and changes nothing.
    pub fn is_observation(self) -> bool {
        matches!(self, Self::Screenshot | Self::Zoom | Self::CursorPosition)
    }
}

impl ComputerAction {
    /// The action's kind.
    pub fn kind(&self) -> ComputerActionKind {
        match self {
            Self::Screenshot => ComputerActionKind::Screenshot,
            Self::Zoom { .. } => ComputerActionKind::Zoom,
            Self::Click { .. } => ComputerActionKind::Click,
            Self::Drag { .. } => ComputerActionKind::Drag,
            Self::MoveTo { .. } => ComputerActionKind::MoveTo,
            Self::MouseDown => ComputerActionKind::MouseDown,
            Self::MouseUp => ComputerActionKind::MouseUp,
            Self::CursorPosition => ComputerActionKind::CursorPosition,
            Self::Scroll { .. } => ComputerActionKind::Scroll,
            Self::Type { .. } => ComputerActionKind::Type,
            Self::Key { .. } => ComputerActionKind::Key,
            Self::HoldKey { .. } => ComputerActionKind::HoldKey,
            Self::Wait { .. } => ComputerActionKind::Wait,
        }
    }
}

/// A captured image of the display or of a region of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screenshot {
    /// The image's MIME type, `image/png` or `image/jpeg`.
    pub mime_type: String,
    /// The encoded image.
    pub data: Vec<u8>,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl Screenshot {
    /// A PNG image, with its size read from the PNG header.
    ///
    /// Returns `None` when `data` is not a PNG.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::Screenshot;
    ///
    /// let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    /// png.extend_from_slice(&1280u32.to_be_bytes());
    /// png.extend_from_slice(&800u32.to_be_bytes());
    /// let shot = Screenshot::png(png).unwrap();
    /// assert_eq!((shot.width, shot.height), (1280, 800));
    /// ```
    pub fn png(data: Vec<u8>) -> Option<Self> {
        if data.len() < 24 || &data[..8] != b"\x89PNG\r\n\x1a\n" || &data[12..16] != b"IHDR" {
            return None;
        }
        let width = u32::from_be_bytes([data[16], data[17], data[18], data[19]]);
        let height = u32::from_be_bytes([data[20], data[21], data[22], data[23]]);
        Some(Self { mime_type: "image/png".to_string(), data, width, height })
    }

    /// Whether the image fits the limits Claude Opus 4.7 and later models accept without a
    /// resize: 2,576 pixels on the long edge and 4,784 visual tokens, one token per 28×28
    /// pixel tile.
    pub fn fits_current_claude_limits(&self) -> bool {
        let tiles = self.width.div_ceil(28) as u64 * self.height.div_ceil(28) as u64;
        self.width.max(self.height) <= 2_576 && tiles <= 4_784
    }
}

/// What an action produced.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ActuationOutput {
    /// The action ran and has nothing to report.
    Done,
    /// An image: the result of a screenshot or a zoom.
    Image(Screenshot),
    /// A position, from a cursor query.
    Cursor(ScreenPoint),
}

/// An environment that performs [`ComputerAction`]s: a virtual desktop, a remote desktop
/// session, or a browser viewport.
///
/// Implementations act on whatever they are connected to, so give an agent one only through
/// tools that run on the governed path, and only in an isolated environment with no
/// credentials it does not need.
///
/// # Example
///
/// ```rust
/// use adk_core::{ActuationOutput, ComputerAction, ComputerActionKind, ComputerActuator, Result};
/// use async_trait::async_trait;
///
/// /// An actuator that records actions and performs none.
/// struct Recorder(std::sync::Mutex<Vec<ComputerAction>>);
///
/// #[async_trait]
/// impl ComputerActuator for Recorder {
///     fn display_size(&self) -> Option<(u32, u32)> {
///         Some((1280, 800))
///     }
///
///     fn supports(&self, kind: ComputerActionKind) -> bool {
///         kind != ComputerActionKind::Zoom
///     }
///
///     async fn perform(&self, action: ComputerAction) -> Result<ActuationOutput> {
///         self.0.lock().unwrap().push(action);
///         Ok(ActuationOutput::Done)
///     }
/// }
/// ```
#[async_trait]
pub trait ComputerActuator: Send + Sync {
    /// The size of the screenshots the actuator returns, when it is known before the first
    /// one: the coordinate space of every action.
    fn display_size(&self) -> Option<(u32, u32)>;

    /// Whether the actuator can perform actions of `kind`. Tools withhold the rest from the
    /// model. Every kind but [`ComputerActionKind::Zoom`] by default.
    fn supports(&self, kind: ComputerActionKind) -> bool {
        kind != ComputerActionKind::Zoom
    }

    /// Performs one action.
    ///
    /// # Errors
    ///
    /// Returns an error when the action cannot be performed: a coordinate outside the
    /// display, an unknown key name, or a failure of the environment.
    async fn perform(&self, action: ComputerAction) -> Result<ActuationOutput>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_size_comes_from_the_header() {
        assert!(Screenshot::png(b"not a png".to_vec()).is_none());
        let mut data = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        data.extend_from_slice(&3024u32.to_be_bytes());
        data.extend_from_slice(&1964u32.to_be_bytes());
        let shot = Screenshot::png(data).unwrap();
        assert_eq!((shot.width, shot.height), (3024, 1964));
        assert!(!shot.fits_current_claude_limits());
        let fitting = Screenshot { width: 1280, height: 800, ..shot };
        assert!(fitting.fits_current_claude_limits());
    }

    #[test]
    fn actions_serialize_with_their_kind() {
        let action = ComputerAction::Scroll {
            at: None,
            direction: ScrollDirection::Down,
            amount: 3,
            modifiers: Vec::new(),
        };
        let json = serde_json::to_value(&action).unwrap();
        assert_eq!(json["type"], "scroll");
        assert_eq!(serde_json::from_value::<ComputerAction>(json).unwrap(), action);
    }
}
