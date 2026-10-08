//! Browser automation tools for ADK agents.
//!
//! This module provides a collection of tools for browser automation:
//!
//! - Navigation: `NavigateTool`, `BackTool`, `ForwardTool`, `RefreshTool`
//! - Interaction: `ClickTool`, `DoubleClickTool`, `TypeTool`, `ClearTool`, `SelectTool`
//! - Extraction: `ExtractTextTool`, `ExtractAttributeTool`, `ExtractLinksTool`, `PageInfoTool`, `PageSourceTool`
//! - Screenshots: `ScreenshotTool`
//! - Waiting: `WaitForElementTool`, `WaitTool`, `WaitForPageLoadTool`, `WaitForTextTool`
//! - JavaScript: `EvaluateJsTool`, `ScrollTool`, `HoverTool`, `AlertTool`
//! - Cookies: `GetCookiesTool`, `GetCookieTool`, `AddCookieTool`, `DeleteCookieTool`, `DeleteAllCookiesTool`
//! - Windows/Tabs: `ListWindowsTool`, `NewTabTool`, `NewWindowTool`, `SwitchWindowTool`, `CloseWindowTool`, etc.
//! - Frames: `SwitchToFrameTool`, `SwitchToParentFrameTool`, `SwitchToDefaultContentTool`
//! - Advanced: `DragAndDropTool`, `RightClickTool`, `FocusTool`, `ElementStateTool`, `PressKeyTool`, etc.

mod actions;
mod click;
mod cookies;
mod evaluate;
mod extract;
mod frames;
mod navigate;
mod screenshot;
mod type_text;
mod wait;
mod windows;

// Navigation tools
pub(crate) use navigate::default_allowed_schemes;
pub use navigate::{BackTool, DEFAULT_ALLOWED_SCHEMES, ForwardTool, NavigateTool, RefreshTool};

// Click/interaction tools
pub use click::{ClickTool, DoubleClickTool};

// Type/form tools
pub use type_text::{ClearTool, SelectTool, TypeTool};

// Screenshot tools
pub use screenshot::ScreenshotTool;

// Extraction tools
pub use extract::{
    ExtractAttributeTool, ExtractLinksTool, ExtractTextTool, PageInfoTool, PageSourceTool,
};

// Wait tools
pub use wait::{WaitForElementTool, WaitForPageLoadTool, WaitForTextTool, WaitTool};

// JavaScript/advanced tools
pub use evaluate::{AlertTool, EvaluateJsTool, HoverTool, ScrollTool};

// Cookie management tools
pub use cookies::{
    AddCookieTool, DeleteAllCookiesTool, DeleteCookieTool, GetCookieTool, GetCookiesTool,
};

// Window/tab management tools
pub use windows::{
    CloseWindowTool, ListWindowsTool, MaximizeWindowTool, MinimizeWindowTool, NewTabTool,
    NewWindowTool, SetWindowSizeTool, SwitchWindowTool,
};

// Frame/iframe management tools
pub use frames::{SwitchToDefaultContentTool, SwitchToFrameTool, SwitchToParentFrameTool};

// Advanced action tools
pub use actions::{
    DragAndDropTool, ElementStateTool, FileUploadTool, FocusTool, PressKeyTool, PrintToPdfTool,
    RightClickTool,
};
