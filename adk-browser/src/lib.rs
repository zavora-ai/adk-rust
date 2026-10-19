//! # adk-browser
#![allow(clippy::result_large_err)]
//!
//! Browser automation tools for ADK agents using WebDriver (via thirtyfour).
//!
//! ## Overview
//!
//! This crate provides browser automation capabilities as ADK tools, allowing
//! LLM agents to interact with web pages. Tools are designed to work with any
//! LlmAgent and inherit all ADK benefits (callbacks, session management, etc.).
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use adk_browser::{BrowserConfig, BrowserProfile, BrowserSession, BrowserToolset};
//! use std::sync::Arc;
//!
//! # async fn example() -> adk_core::Result<()> {
//! // Create a browser session; it starts on first use
//! let config = BrowserConfig::new().headless(true).viewport(1920, 1080);
//! let browser = Arc::new(BrowserSession::new(config));
//!
//! // Create a toolset and register it with `LlmAgentBuilder::toolset`
//! let toolset = BrowserToolset::with_profile(browser.clone(), BrowserProfile::Scraping);
//! assert_eq!(toolset.all_tools().len(), 13);
//!
//! // Clean up
//! browser.stop().await?;
//! # Ok(())
//! # }
//! ```
//!
//! `BrowserToolset::new` offers 44 tools. `browser_evaluate_js` and `browser_file_upload` are
//! opt-in, navigation accepts only `http` and `https`, and private network addresses are refused.
//!
//! ## Available Tools
//!
//! ### Navigation
//! - `browser_navigate` - Navigate to a URL (`http`/`https` unless
//!   `BrowserToolset::with_allowed_schemes` says otherwise; private network
//!   addresses, including redirect targets, refused unless
//!   `BrowserToolset::with_private_network_access` permits them; host names that
//!   do not resolve on the agent host refused unless
//!   `BrowserToolset::with_unresolved_hosts` permits them)
//! - `browser_back` - Go back in history
//! - `browser_forward` - Go forward in history
//! - `browser_refresh` - Refresh the page
//!
//! The history tools check the page they land on against the same private
//! network policy.
//!
//! ### Interaction
//! - `browser_click` - Click on an element
//! - `browser_double_click` - Double-click an element
//! - `browser_type` - Type text into an input
//! - `browser_clear` - Clear an input field
//! - `browser_select` - Select from a dropdown
//!
//! ### Extraction
//! - `browser_extract_text` - Get text from elements
//! - `browser_extract_attribute` - Get attribute values
//! - `browser_extract_links` - Get all links on page
//! - `browser_page_info` - Get current URL and title
//! - `browser_page_source` - Get HTML source
//!
//! ### Screenshots
//! - `browser_screenshot` - Capture page or element screenshot
//!
//! ### Waiting
//! - `browser_wait_for_element` - Wait for element to appear
//! - `browser_wait` - Wait for a duration
//! - `browser_wait_for_page_load` - Wait for page to load
//! - `browser_wait_for_text` - Wait for text to appear
//!
//! ### JavaScript
//! - `browser_evaluate_js` - Execute JavaScript code (opt-in via
//!   `BrowserToolset::with_evaluate_js(true)`)
//! - `browser_scroll` - Scroll the page
//! - `browser_hover` - Hover over an element
//! - `browser_handle_alert` - Handle JavaScript alerts
//!
//! ### Cookies
//! - `browser_get_cookies` - Get all cookies
//! - `browser_get_cookie` - Get a specific cookie
//! - `browser_add_cookie` - Add a cookie
//! - `browser_delete_cookie` - Delete a cookie
//! - `browser_delete_all_cookies` - Delete all cookies
//!
//! ### Windows/Tabs
//! - `browser_list_windows` - List all windows/tabs
//! - `browser_new_tab` - Open a new tab
//! - `browser_new_window` - Open a new window
//! - `browser_switch_window` - Switch to a window
//! - `browser_close_window` - Close current window
//! - `browser_maximize_window` - Maximize window
//! - `browser_minimize_window` - Minimize window
//! - `browser_set_window_size` - Set window size
//!
//! ### Frames
//! - `browser_switch_to_frame` - Switch to an iframe
//! - `browser_switch_to_parent_frame` - Exit current iframe
//! - `browser_switch_to_default_content` - Exit all iframes
//!
//! ### Advanced Actions
//! - `browser_drag_and_drop` - Drag and drop elements
//! - `browser_right_click` - Right-click (context menu)
//! - `browser_focus` - Focus on an element
//! - `browser_element_state` - Check element state
//! - `browser_press_key` - Press keyboard keys
//! - `browser_file_upload` - Upload files from allowed directories (opt-in via
//!   `BrowserToolset::with_file_upload(roots)`)
//! - `browser_print_to_pdf` - Print page to PDF
//!
//! ## Requirements
//!
//! A WebDriver server (like ChromeDriver, geckodriver, or Selenium) must be
//! running and accessible. By default, tools connect to `http://localhost:4444`.
//!
//! ### Starting ChromeDriver
//!
//! ```bash
//! # Install ChromeDriver (macOS)
//! brew install chromedriver
//!
//! # Start ChromeDriver
//! chromedriver --port=4444
//! ```
//!
//! ### Using Docker
//!
//! ```bash
//! docker run -d -p 4444:4444 selenium/standalone-chrome
//! ```
//!
//! ## Architecture
//!
//! Tools are implemented using the ADK `Tool` trait, allowing them to:
//! - Work with any LLM model (Gemini, OpenAI, Anthropic)
//! - Use callbacks for monitoring and control
//! - Access session state and artifacts
//! - Compose with other tools and agents
//!
//! ```text
//! ┌─────────────────────────────────────────────────┐
//! │                   LlmAgent                       │
//! │  (with callbacks, session, artifacts, memory)   │
//! └─────────────────────────────────────────────────┘
//!                        │
//!                        ▼
//! ┌─────────────────────────────────────────────────┐
//! │               BrowserToolset                     │
//! │  NavigateTool, ClickTool, TypeTool, ...         │
//! └─────────────────────────────────────────────────┘
//!                        │
//!                        ▼
//! ┌─────────────────────────────────────────────────┐
//! │              BrowserSession                      │
//! │         (wraps thirtyfour WebDriver)            │
//! └─────────────────────────────────────────────────┘
//!                        │
//!                        ▼
//!              WebDriver Server
//!           (ChromeDriver, etc.)
//! ```

mod config;
mod escape;
mod pool;
mod session;
pub mod tools;
mod toolset;

// Re-export main types
pub use config::{BrowserConfig, BrowserType};
pub use escape::escape_js_string;
pub use pool::BrowserSessionPool;
pub use session::{BrowserSession, ElementState, shared_session};
pub use toolset::{BrowserProfile, BrowserToolset, minimal_browser_tools, readonly_browser_tools};

// Re-export individual tools for selective use
pub use tools::{
    // Cookies
    AddCookieTool,
    // JavaScript
    AlertTool,
    // Navigation
    BackTool,
    // Interaction
    ClearTool,
    ClickTool,
    // Windows/Tabs
    CloseWindowTool,
    // Navigation scheme allowlist
    DEFAULT_ALLOWED_SCHEMES,
    DeleteAllCookiesTool,
    DeleteCookieTool,
    DoubleClickTool,
    // Advanced Actions
    DragAndDropTool,
    ElementStateTool,
    EvaluateJsTool,
    // Extraction
    ExtractAttributeTool,
    ExtractLinksTool,
    ExtractTextTool,
    FileUploadTool,
    FocusTool,
    ForwardTool,
    GetCookieTool,
    GetCookiesTool,
    HoverTool,
    ListWindowsTool,
    MaximizeWindowTool,
    MinimizeWindowTool,
    NavigateTool,
    NewTabTool,
    NewWindowTool,
    PageInfoTool,
    PageSourceTool,
    PressKeyTool,
    PrintToPdfTool,
    RefreshTool,
    RightClickTool,
    // Screenshots
    ScreenshotTool,
    ScrollTool,
    SelectTool,
    SetWindowSizeTool,
    // Frames
    SwitchToDefaultContentTool,
    SwitchToFrameTool,
    SwitchToParentFrameTool,
    SwitchWindowTool,
    TypeTool,
    // Waiting
    WaitForElementTool,
    WaitForPageLoadTool,
    WaitForTextTool,
    WaitTool,
};

/// Prelude module for convenient imports.
pub mod prelude {
    pub use crate::config::{BrowserConfig, BrowserType};
    pub use crate::pool::BrowserSessionPool;
    pub use crate::session::{BrowserSession, shared_session};
    pub use crate::toolset::{
        BrowserProfile, BrowserToolset, minimal_browser_tools, readonly_browser_tools,
    };
}
