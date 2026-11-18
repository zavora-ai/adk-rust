//! Claude's client toolsets — `computer_toolset_20260801` and `browser_toolset_20260801` —
//! and the earlier `computer_20251124` and `computer_20250124` computer use tools.
//!
//! A client toolset is one `tools` entry that declares a fixed set of member tools. Claude
//! calls a member with a `tool_use` block whose `name` is the member and whose
//! `toolset_name` is [`COMPUTER_TOOLSET`] or [`BROWSER_TOOLSET`]; each result echoes the
//! same `toolset_name`. Several member calls in one turn form a batch that runs in order
//! and stops at the first failure, answering the rest with [`COMPUTER_BATCH_HALT_TEXT`] or
//! [`BROWSER_BATCH_HALT_TEXT`].

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::types::CacheControlEphemeral;

/// The `toolset_name` of a computer use member call and of its result.
pub const COMPUTER_TOOLSET: &str = "computer";

/// The `toolset_name` of a browser use member call and of its result.
pub const BROWSER_TOOLSET: &str = "browser";

/// The exact result text for each computer use call after the first failure in a batch.
pub const COMPUTER_BATCH_HALT_TEXT: &str =
    "Not executed: an earlier computer action in this turn failed.";

/// The exact result text for each browser use call after the first failure in a batch.
pub const BROWSER_BATCH_HALT_TEXT: &str = "Not executed: an earlier action in this turn failed.";

/// The beta header the `computer_20251124` tool requires.
pub const COMPUTER_USE_20251124_BETA: &str = "computer-use-2025-11-24";

/// The beta header the `computer_20250124` tool requires.
pub const COMPUTER_USE_20250124_BETA: &str = "computer-use-2025-01-24";

/// Settings for one member of a client toolset, keyed by member name in `configs`.
///
/// A member left out of `configs`, or given an empty value, keeps its default.
///
/// # Example
///
/// ```rust
/// use adk_anthropic::ToolsetMemberConfig;
///
/// let off = ToolsetMemberConfig::enabled(false);
/// assert_eq!(serde_json::to_value(&off).unwrap(), serde_json::json!({ "enabled": false }));
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolsetMemberConfig {
    /// Whether the member is offered to Claude.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,

    /// Whether the toolset's definition is deferred for tool search. Every enabled member
    /// must carry the same value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub defer_loading: Option<bool>,
}

impl ToolsetMemberConfig {
    /// A config that turns the member on or off.
    pub fn enabled(enabled: bool) -> Self {
        Self { enabled: Some(enabled), defer_loading: None }
    }
}

macro_rules! members {
    (
        $(#[$meta:meta])*
        $name:ident {
            $($(#[$vmeta:meta])* $variant:ident => $wire:literal),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        pub enum $name {
            $($(#[$vmeta])* #[serde(rename = $wire)] $variant,)+
        }

        impl $name {
            /// Every member, in the order the documentation lists them.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            /// The member's wire name, which is the `name` of its `tool_use` blocks.
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $wire,)+
                }
            }

            /// The member called `name`, or `None` for a name this version does not define.
            pub fn from_name(name: &str) -> Option<Self> {
                match name {
                    $($wire => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

members! {
    /// A member tool of `computer_toolset_20260801`, and an action of the earlier
    /// `computer_20251124` and `computer_20250124` tools.
    ///
    /// Coordinates in every member are in the pixel space of the full screenshots the
    /// application returns.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_anthropic::ComputerMember;
    ///
    /// assert_eq!(ComputerMember::ALL.len(), 17);
    /// assert_eq!(ComputerMember::from_name("left_click"), Some(ComputerMember::LeftClick));
    /// assert!(ComputerMember::Screenshot.is_observation());
    /// ```
    ComputerMember {
        /// Capture the full display as an image.
        Screenshot => "screenshot",
        /// Capture `region` (`[x0, y0, x1, y1]`) at full resolution as an image.
        Zoom => "zoom",
        /// Left-click at `coordinate`, or at the cursor; `text` holds modifier keys.
        LeftClick => "left_click",
        /// Right-click, as `left_click`.
        RightClick => "right_click",
        /// Middle-click, as `left_click`.
        MiddleClick => "middle_click",
        /// Double-click, as `left_click`.
        DoubleClick => "double_click",
        /// Triple-click, as `left_click`.
        TripleClick => "triple_click",
        /// Press at `start_coordinate`, drag to `coordinate`, release.
        LeftClickDrag => "left_click_drag",
        /// Move the cursor to `coordinate`.
        MouseMove => "mouse_move",
        /// Press the left button at the cursor.
        LeftMouseDown => "left_mouse_down",
        /// Release the left button at the cursor.
        LeftMouseUp => "left_mouse_up",
        /// Report the cursor position as text.
        CursorPosition => "cursor_position",
        /// Scroll by `scroll_amount` notches in `scroll_direction`.
        Scroll => "scroll",
        /// Type `text` at the keyboard focus.
        Type => "type",
        /// Press the key or chord in `text`, `repeat` times.
        Key => "key",
        /// Hold the key in `text` for `duration` seconds.
        HoldKey => "hold_key",
        /// Pause for `duration` seconds.
        Wait => "wait",
    }
}

impl ComputerMember {
    /// Whether the member only reads the screen or cursor and changes nothing.
    pub fn is_observation(self) -> bool {
        matches!(self, Self::Screenshot | Self::Zoom | Self::CursorPosition)
    }

    /// Whether the member's result must carry an image.
    pub fn returns_image(self) -> bool {
        matches!(self, Self::Screenshot | Self::Zoom)
    }
}

members! {
    /// A member tool of `browser_toolset_20260801`.
    ///
    /// A `target` is either `{"type": "coordinate", "x", "y"}` in viewport pixels or
    /// `{"type": "ref", "ref"}`, a reference an earlier `read_page` or `find` returned.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_anthropic::BrowserMember;
    ///
    /// assert_eq!(BrowserMember::ALL.len(), 31);
    /// assert!(!BrowserMember::JavascriptExec.enabled_by_default());
    /// assert!(BrowserMember::NewTab.is_tab_management());
    /// ```
    BrowserMember {
        /// Load a URL, or go `"back"`, `"forward"` or `"reload"`.
        Navigate => "navigate",
        /// Capture the viewport as an image.
        Screenshot => "screenshot",
        /// Capture `region` of the viewport, upscaled, as an image.
        Zoom => "zoom",
        /// Left-click a target.
        LeftClick => "left_click",
        /// Right-click a target.
        RightClick => "right_click",
        /// Middle-click a target.
        MiddleClick => "middle_click",
        /// Double-click a target.
        DoubleClick => "double_click",
        /// Triple-click a target.
        TripleClick => "triple_click",
        /// Move the pointer over a target.
        Hover => "hover",
        /// Press at `from`, drag to `target`, release.
        LeftClickDrag => "left_click_drag",
        /// Press and hold the left button at a coordinate.
        LeftMouseDown => "left_mouse_down",
        /// Release the left button at a coordinate.
        LeftMouseUp => "left_mouse_up",
        /// Move the pointer to a coordinate.
        MouseMove => "mouse_move",
        /// Scroll at a viewport position.
        Scroll => "scroll",
        /// Scroll a referenced element into view.
        ScrollTo => "scroll_to",
        /// Type a literal string at the focus.
        Type => "type",
        /// Press a key, chord or sequence.
        Key => "key",
        /// Hold a key for `duration` seconds.
        HoldKey => "hold_key",
        /// Pause for `duration` seconds.
        Wait => "wait",
        /// Return the accessibility tree with element references.
        ReadPage => "read_page",
        /// Find elements matching a description.
        Find => "find",
        /// Return the page's visible text.
        GetPageText => "get_page_text",
        /// Set a form element's value.
        FormInput => "form_input",
        /// Set the files on a file input (disabled by default).
        FileUpload => "file_upload",
        /// Return console entries since the last read (disabled by default).
        ReadConsole => "read_console",
        /// Return network requests since the last read (disabled by default).
        ReadNetwork => "read_network",
        /// Run JavaScript in the page (disabled by default).
        JavascriptExec => "javascript_exec",
        /// Open a tab and make it active.
        NewTab => "new_tab",
        /// Report the tab inventory.
        ListTabs => "list_tabs",
        /// Make a tab active.
        SwitchTab => "switch_tab",
        /// Close a tab.
        CloseTab => "close_tab",
    }
}

impl BrowserMember {
    /// Whether the toolset offers the member when `configs` does not mention it.
    ///
    /// `javascript_exec`, `file_upload`, `read_console` and `read_network` are off by
    /// default because they widen what a manipulated page could make Claude do.
    pub fn enabled_by_default(self) -> bool {
        !matches!(
            self,
            Self::JavascriptExec | Self::FileUpload | Self::ReadConsole | Self::ReadNetwork
        )
    }

    /// Whether the member only reads the page or the tab inventory and changes nothing.
    pub fn is_observation(self) -> bool {
        matches!(
            self,
            Self::Screenshot
                | Self::Zoom
                | Self::ReadPage
                | Self::Find
                | Self::GetPageText
                | Self::ReadConsole
                | Self::ReadNetwork
                | Self::ListTabs
        )
    }

    /// Whether a successful result is exactly one `browser_state` block.
    pub fn is_tab_management(self) -> bool {
        matches!(self, Self::NewTab | Self::ListTabs | Self::SwitchTab | Self::CloseTab)
    }
}

/// The computer use toolset entry, `{"type": "computer_toolset_20260801"}`.
///
/// It takes no `name` and no display size: coordinates are in the pixel space of the
/// screenshots the application returns, which must already fit the model's image limits.
/// All 17 members, `zoom` included, are on by default. GA, no beta header.
///
/// # Example
///
/// ```rust
/// use adk_anthropic::{ComputerMember, ComputerToolset20260801, ToolUnionParam};
/// use serde_json::json;
///
/// let tool = ToolUnionParam::ComputerToolset20260801(
///     ComputerToolset20260801::new().with_member(ComputerMember::Zoom, false),
/// );
/// assert_eq!(
///     serde_json::to_value(&tool).unwrap(),
///     json!({ "type": "computer_toolset_20260801", "configs": { "zoom": { "enabled": false } } })
/// );
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ComputerToolset20260801 {
    /// Per-member settings keyed by member name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub configs: BTreeMap<String, ToolsetMemberConfig>,

    /// Prompt-cache breakpoint at the toolset definition.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControlEphemeral>,

    /// Who may call the members; only `["direct"]` is accepted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_callers: Option<Vec<String>>,
}

impl ComputerToolset20260801 {
    /// The toolset with every member at its default.
    pub fn new() -> Self {
        Self::default()
    }

    /// Turns `member` on or off.
    pub fn with_member(mut self, member: ComputerMember, enabled: bool) -> Self {
        self.configs.insert(member.as_str().to_string(), ToolsetMemberConfig::enabled(enabled));
        self
    }

    /// Whether `member` is offered to Claude under these `configs`.
    pub fn is_enabled(&self, member: ComputerMember) -> bool {
        self.configs.get(member.as_str()).and_then(|config| config.enabled).unwrap_or(true)
    }

    /// Sets a prompt-cache breakpoint at the toolset definition.
    pub fn with_cache_control(mut self, cache_control: CacheControlEphemeral) -> Self {
        self.cache_control = Some(cache_control);
        self
    }
}

/// The browser use toolset entry, `{"type": "browser_toolset_20260801"}`.
///
/// It declares 31 members, 27 of them on by default; `javascript_exec`, `file_upload`,
/// `read_console` and `read_network` are off until `configs` enables them. GA, no beta
/// header. Results may carry a [`BrowserStateBlock`](crate::BrowserStateBlock).
///
/// # Example
///
/// ```rust
/// use adk_anthropic::{BrowserMember, BrowserToolset20260801, ToolUnionParam};
/// use serde_json::json;
///
/// let tool = ToolUnionParam::BrowserToolset20260801(
///     BrowserToolset20260801::new().with_member(BrowserMember::ReadConsole, true),
/// );
/// assert_eq!(
///     serde_json::to_value(&tool).unwrap(),
///     json!({
///         "type": "browser_toolset_20260801",
///         "configs": { "read_console": { "enabled": true } }
///     })
/// );
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct BrowserToolset20260801 {
    /// Per-member settings keyed by member name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub configs: BTreeMap<String, ToolsetMemberConfig>,

    /// Prompt-cache breakpoint at the toolset definition.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControlEphemeral>,

    /// Who may call the members; only `["direct"]` is accepted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_callers: Option<Vec<String>>,
}

impl BrowserToolset20260801 {
    /// The toolset with every member at its default.
    pub fn new() -> Self {
        Self::default()
    }

    /// Turns `member` on or off.
    pub fn with_member(mut self, member: BrowserMember, enabled: bool) -> Self {
        self.configs.insert(member.as_str().to_string(), ToolsetMemberConfig::enabled(enabled));
        self
    }

    /// Whether `member` is offered to Claude under these `configs`.
    pub fn is_enabled(&self, member: BrowserMember) -> bool {
        self.configs
            .get(member.as_str())
            .and_then(|config| config.enabled)
            .unwrap_or_else(|| member.enabled_by_default())
    }

    /// Sets a prompt-cache breakpoint at the toolset definition.
    pub fn with_cache_control(mut self, cache_control: CacheControlEphemeral) -> Self {
        self.cache_control = Some(cache_control);
        self
    }
}

fn default_computer_name() -> String {
    COMPUTER_TOOLSET.to_string()
}

/// The `computer_20251124` tool (beta `computer-use-2025-11-24`).
///
/// One tool named `computer` whose calls carry the member in `input.action`. Claude Opus
/// 4.5–4.7 and Sonnet 4.6 take it; Claude Opus 5.5, Sonnet 5.5, Haiku 5.5 and Fable 5.2
/// reject it on the Claude API.
///
/// # Example
///
/// ```rust
/// use adk_anthropic::{ToolComputer20251124, ToolUnionParam};
/// use serde_json::json;
///
/// let tool = ToolUnionParam::Computer20251124(ToolComputer20251124::new(1280, 800));
/// assert_eq!(
///     serde_json::to_value(&tool).unwrap(),
///     json!({
///         "type": "computer_20251124",
///         "name": "computer",
///         "display_width_px": 1280,
///         "display_height_px": 800
///     })
/// );
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolComputer20251124 {
    /// Always `computer`.
    #[serde(default = "default_computer_name")]
    pub name: String,

    /// Width of the display in the screenshots' pixel space.
    pub display_width_px: u32,

    /// Height of the display in the screenshots' pixel space.
    pub display_height_px: u32,

    /// X11 display number, for environments with several.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_number: Option<u32>,

    /// Whether the `zoom` action is offered; off unless set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enable_zoom: Option<bool>,

    /// Prompt-cache breakpoint at the tool definition.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControlEphemeral>,
}

impl ToolComputer20251124 {
    /// The tool for a display of `width` by `height` screenshot pixels.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            name: default_computer_name(),
            display_width_px: width,
            display_height_px: height,
            display_number: None,
            enable_zoom: None,
            cache_control: None,
        }
    }

    /// Offers or withholds the `zoom` action.
    pub fn with_zoom(mut self, enabled: bool) -> Self {
        self.enable_zoom = Some(enabled);
        self
    }
}

/// The `computer_20250124` tool (beta `computer-use-2025-01-24`).
///
/// Claude Haiku 4.5, Sonnet 4.5, Sonnet 4 and Opus 4.1 take it. It has no `zoom` action.
///
/// # Example
///
/// ```rust
/// use adk_anthropic::ToolComputer20250124;
///
/// let tool = ToolComputer20250124::new(1024, 768);
/// assert_eq!(tool.name, "computer");
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolComputer20250124 {
    /// Always `computer`.
    #[serde(default = "default_computer_name")]
    pub name: String,

    /// Width of the display in the screenshots' pixel space.
    pub display_width_px: u32,

    /// Height of the display in the screenshots' pixel space.
    pub display_height_px: u32,

    /// X11 display number, for environments with several.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_number: Option<u32>,

    /// Prompt-cache breakpoint at the tool definition.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControlEphemeral>,
}

impl ToolComputer20250124 {
    /// The tool for a display of `width` by `height` screenshot pixels.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            name: default_computer_name(),
            display_width_px: width,
            display_height_px: height,
            display_number: None,
            cache_control: None,
        }
    }
}

/// The family and version a Claude model identifier names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Generation {
    /// `claude-fable-*`, `claude-mythos-*`.
    Frontier,
    Opus(u32, u32),
    Sonnet(u32, u32),
    Haiku(u32, u32),
    /// A Claude 3 identifier (`claude-3-7-sonnet-…`), with its minor version.
    Claude3 {
        minor: u32,
        sonnet: bool,
    },
}

/// Reads the family and version out of a Claude model identifier, including provider
/// forms such as `anthropic.claude-sonnet-4-6` or `claude-opus-4-5@20251101`.
fn generation(model: &str) -> Option<Generation> {
    let start = model.find("claude-")?;
    let rest = &model[start + "claude-".len()..];
    let mut parts = rest.split(['-', '@', '.', ':']);
    let first = parts.next()?;
    if first == "3" {
        let minor = parts.next().and_then(|part| part.parse::<u32>().ok()).unwrap_or(0);
        let sonnet = rest.contains("sonnet");
        return Some(Generation::Claude3 { minor, sonnet });
    }
    if matches!(first, "fable" | "mythos") {
        return Some(Generation::Frontier);
    }
    let major = parts.next()?.parse::<u32>().ok()?;
    // A dated snapshot (`-20250929`) is not a minor version.
    let minor = parts
        .next()
        .filter(|part| part.len() <= 2)
        .and_then(|part| part.parse::<u32>().ok())
        .unwrap_or(0);
    match first {
        "opus" => Some(Generation::Opus(major, minor)),
        "sonnet" => Some(Generation::Sonnet(major, minor)),
        "haiku" => Some(Generation::Haiku(major, minor)),
        _ => None,
    }
}

/// The computer use tool version a model accepts on the Claude API and Google Cloud.
///
/// Claude Opus 5.5, Sonnet 5.5, Haiku 5.5 and Fable/Mythos 5.2 accept only the toolset
/// there; Amazon Bedrock still accepts `computer_20251124` for them, so a Bedrock caller
/// that sends the earlier version keeps it.
///
/// # Example
///
/// ```rust
/// use adk_anthropic::ComputerToolVersion;
///
/// assert_eq!(
///     ComputerToolVersion::for_model("claude-sonnet-5-5"),
///     Some(ComputerToolVersion::Toolset20260801)
/// );
/// assert_eq!(
///     ComputerToolVersion::for_model("claude-opus-4-7"),
///     Some(ComputerToolVersion::Computer20251124)
/// );
/// assert_eq!(
///     ComputerToolVersion::for_model("claude-haiku-4-5-20251001"),
///     Some(ComputerToolVersion::Computer20250124)
/// );
/// assert_eq!(ComputerToolVersion::for_model("my-proxy-alias"), None);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ComputerToolVersion {
    /// `computer_toolset_20260801`, GA, no beta header.
    Toolset20260801,
    /// `computer_20251124`, beta `computer-use-2025-11-24`.
    Computer20251124,
    /// `computer_20250124`, beta `computer-use-2025-01-24`.
    Computer20250124,
}

impl ComputerToolVersion {
    /// The version `model` accepts.
    ///
    /// A Claude identifier newer than any listed here takes the toolset. Returns `None`
    /// for an identifier that names no Claude model (an alias or deployment name), and for
    /// a Claude model with no computer use version.
    pub fn for_model(model: &str) -> Option<Self> {
        match generation(model)? {
            Generation::Frontier => Some(Self::Toolset20260801),
            Generation::Opus(major, minor) => match (major, minor) {
                version if version >= (4, 8) => Some(Self::Toolset20260801),
                (4, 5..=7) => Some(Self::Computer20251124),
                (4, _) => Some(Self::Computer20250124),
                _ => None,
            },
            Generation::Sonnet(major, minor) => match (major, minor) {
                version if version >= (5, 0) => Some(Self::Toolset20260801),
                (4, 6..) => Some(Self::Computer20251124),
                (4, _) => Some(Self::Computer20250124),
                _ => None,
            },
            Generation::Haiku(major, minor) => match (major, minor) {
                version if version >= (5, 0) => Some(Self::Toolset20260801),
                (4, _) => Some(Self::Computer20250124),
                _ => None,
            },
            Generation::Claude3 { minor: 7, sonnet: true } => Some(Self::Computer20250124),
            Generation::Claude3 { .. } => None,
        }
    }

    /// The `type` of the tools entry.
    pub fn type_name(self) -> &'static str {
        match self {
            Self::Toolset20260801 => "computer_toolset_20260801",
            Self::Computer20251124 => "computer_20251124",
            Self::Computer20250124 => "computer_20250124",
        }
    }

    /// The beta header the version requires, if any.
    pub fn beta_header(self) -> Option<&'static str> {
        match self {
            Self::Toolset20260801 => None,
            Self::Computer20251124 => Some(COMPUTER_USE_20251124_BETA),
            Self::Computer20250124 => Some(COMPUTER_USE_20250124_BETA),
        }
    }

    /// Whether calls arrive as toolset member blocks rather than as one `computer` tool
    /// with the member in `input.action`.
    pub fn is_toolset(self) -> bool {
        matches!(self, Self::Toolset20260801)
    }
}

/// Whether `model` accepts `browser_toolset_20260801` on the Claude API.
///
/// Claude Fable and Mythos, Opus 4.8 and later, Sonnet 5 and later and Haiku 5.5 take it.
/// An identifier that names no Claude model returns `true`, leaving the decision to the
/// API.
///
/// # Example
///
/// ```rust
/// use adk_anthropic::supports_browser_toolset;
///
/// assert!(supports_browser_toolset("claude-haiku-5-5"));
/// assert!(!supports_browser_toolset("claude-sonnet-4-6"));
/// ```
pub fn supports_browser_toolset(model: &str) -> bool {
    match generation(model) {
        None => true,
        Some(Generation::Frontier) => true,
        Some(Generation::Opus(major, minor)) => (major, minor) >= (4, 8),
        Some(Generation::Sonnet(major, _)) | Some(Generation::Haiku(major, _)) => major >= 5,
        Some(Generation::Claude3 { .. }) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, to_value};

    #[test]
    fn member_counts_match_the_documented_toolsets() {
        assert_eq!(ComputerMember::ALL.len(), 17);
        assert_eq!(BrowserMember::ALL.len(), 31);
        let default_browser =
            BrowserMember::ALL.iter().filter(|member| member.enabled_by_default()).count();
        assert_eq!(default_browser, 27);
    }

    #[test]
    fn member_names_round_trip() {
        for member in ComputerMember::ALL {
            assert_eq!(ComputerMember::from_name(member.as_str()), Some(*member));
            assert_eq!(to_value(member).unwrap(), json!(member.as_str()));
        }
        for member in BrowserMember::ALL {
            assert_eq!(BrowserMember::from_name(member.as_str()), Some(*member));
        }
        assert_eq!(ComputerMember::from_name("navigate"), None);
    }

    #[test]
    fn toolset_entries_serialize_to_the_documented_shape() {
        let computer = ComputerToolset20260801::new()
            .with_member(ComputerMember::Zoom, false)
            .with_cache_control(CacheControlEphemeral::new());
        assert_eq!(
            to_value(&computer).unwrap(),
            json!({ "configs": { "zoom": { "enabled": false } }, "cache_control": { "type": "ephemeral" } })
        );
        assert!(!computer.is_enabled(ComputerMember::Zoom));
        assert!(computer.is_enabled(ComputerMember::Screenshot));

        let browser = BrowserToolset20260801::new()
            .with_member(BrowserMember::ReadConsole, true)
            .with_member(BrowserMember::HoldKey, false);
        assert!(browser.is_enabled(BrowserMember::ReadConsole));
        assert!(!browser.is_enabled(BrowserMember::HoldKey));
        assert!(!browser.is_enabled(BrowserMember::JavascriptExec));
        assert_eq!(to_value(BrowserToolset20260801::new()).unwrap(), json!({}));
    }

    #[test]
    fn version_follows_the_model() {
        let cases = [
            ("claude-sonnet-5-5", Some(ComputerToolVersion::Toolset20260801)),
            ("claude-haiku-5-5", Some(ComputerToolVersion::Toolset20260801)),
            ("claude-opus-5-5", Some(ComputerToolVersion::Toolset20260801)),
            ("claude-opus-5", Some(ComputerToolVersion::Toolset20260801)),
            ("claude-fable-5-2", Some(ComputerToolVersion::Toolset20260801)),
            ("claude-mythos-5-2", Some(ComputerToolVersion::Toolset20260801)),
            ("claude-mythos-preview", Some(ComputerToolVersion::Toolset20260801)),
            ("claude-opus-4-8", Some(ComputerToolVersion::Toolset20260801)),
            ("claude-sonnet-5", Some(ComputerToolVersion::Toolset20260801)),
            ("claude-opus-4-7", Some(ComputerToolVersion::Computer20251124)),
            ("claude-opus-4-6", Some(ComputerToolVersion::Computer20251124)),
            ("claude-sonnet-4-6", Some(ComputerToolVersion::Computer20251124)),
            ("claude-opus-4-5-20251101", Some(ComputerToolVersion::Computer20251124)),
            ("claude-opus-4-5@20251101", Some(ComputerToolVersion::Computer20251124)),
            ("claude-sonnet-4-5-20250929", Some(ComputerToolVersion::Computer20250124)),
            ("claude-haiku-4-5-20251001", Some(ComputerToolVersion::Computer20250124)),
            ("claude-sonnet-4-20250514", Some(ComputerToolVersion::Computer20250124)),
            ("claude-opus-4-1-20250805", Some(ComputerToolVersion::Computer20250124)),
            ("claude-3-7-sonnet-20250219", Some(ComputerToolVersion::Computer20250124)),
            ("claude-3-5-haiku-20241022", None),
            ("us.anthropic.claude-sonnet-4-6", Some(ComputerToolVersion::Computer20251124)),
            ("claude-opus-6", Some(ComputerToolVersion::Toolset20260801)),
            ("gpt-5", None),
        ];
        for (model, expected) in cases {
            assert_eq!(ComputerToolVersion::for_model(model), expected, "{model}");
        }
    }

    #[test]
    fn browser_toolset_support_follows_the_model() {
        for model in [
            "claude-sonnet-5-5",
            "claude-haiku-5-5",
            "claude-opus-5-5",
            "claude-opus-4-8",
            "claude-fable-5-2",
            "claude-sonnet-5",
            "custom-deployment",
        ] {
            assert!(supports_browser_toolset(model), "{model}");
        }
        for model in ["claude-opus-4-7", "claude-sonnet-4-6", "claude-haiku-4-5-20251001"] {
            assert!(!supports_browser_toolset(model), "{model}");
        }
    }

    #[test]
    fn earlier_versions_carry_their_beta_headers() {
        assert_eq!(ComputerToolVersion::Toolset20260801.beta_header(), None);
        assert_eq!(
            ComputerToolVersion::Computer20251124.beta_header(),
            Some("computer-use-2025-11-24")
        );
        assert_eq!(
            ComputerToolVersion::Computer20250124.beta_header(),
            Some("computer-use-2025-01-24")
        );
    }
}
