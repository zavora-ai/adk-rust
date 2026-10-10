//! Browser toolset that provides all browser tools as a collection.

use crate::pool::BrowserSessionPool;
use crate::session::BrowserSession;
use crate::tools::*;
use adk_core::{ReadonlyContext, Result, Tool, Toolset};
use async_trait::async_trait;
use std::path::PathBuf;
use std::sync::Arc;

/// Internal abstraction for session acquisition.
/// Not exposed in the public API.
enum SessionResolver {
    /// Fixed session — always returns the same session.
    Fixed(Arc<BrowserSession>),
    /// Pool-backed — resolves session from pool using user_id from context.
    Pool(Arc<BrowserSessionPool>),
}

impl SessionResolver {
    async fn resolve(&self, ctx: &Arc<dyn ReadonlyContext>) -> Result<Arc<BrowserSession>> {
        match self {
            SessionResolver::Fixed(session) => Ok(session.clone()),
            SessionResolver::Pool(pool) => pool.get_or_create(ctx.user_id()).await,
        }
    }
}

/// Pre-configured tool profiles for common use cases.
///
/// Instead of using every tool (which overwhelms LLM context windows),
/// select a profile that matches your agent's task. No profile includes
/// `browser_evaluate_js` or `browser_file_upload`; enable them with
/// [`BrowserToolset::with_evaluate_js`] and [`BrowserToolset::with_file_upload`].
///
/// # Example
///
/// ```rust,ignore
/// use adk_browser::{BrowserToolset, BrowserProfile, BrowserSession, BrowserConfig};
/// use std::sync::Arc;
///
/// let browser = Arc::new(BrowserSession::new(BrowserConfig::default()));
/// let toolset = BrowserToolset::with_profile(browser, BrowserProfile::FormFilling);
/// let tools = toolset.all_tools(); // 19 tools instead of 45
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserProfile {
    /// 19 tools: navigation + interaction + extraction + wait + screenshot.
    /// Best for simple browsing tasks.
    Minimal,
    /// 19 tools: same categories as Minimal (navigation + interaction + extraction + wait + screenshot).
    /// Best for form-filling agents.
    FormFilling,
    /// 13 tools: navigation + extraction + screenshot + scroll, hover, and alert handling.
    /// Best for data extraction / scraping agents (no interaction tools).
    Scraping,
    /// 44 tools: every tool except `browser_evaluate_js` and `browser_file_upload`. Use
    /// only when the agent needs full browser control.
    Full,
}

/// A toolset that provides all browser automation tools.
///
/// Use this to add all browser tools to an agent at once, or use
/// individual tools for more control.
///
/// Four defaults limit what a model can reach through the browser:
///
/// - `browser_evaluate_js`, which runs model-written JavaScript in the page, is
///   off until [`with_evaluate_js`](Self::with_evaluate_js) or
///   [`with_js`](Self::with_js) enables it.
/// - `browser_file_upload` is off until [`with_file_upload`](Self::with_file_upload)
///   names the directories files may be uploaded from.
/// - Navigation accepts only `http` and `https` URLs until
///   [`with_allowed_schemes`](Self::with_allowed_schemes) says otherwise, so
///   `file:`, `javascript:`, `data:`, and browser-internal URLs are refused.
/// - Navigation refuses loopback, private, link-local, and cloud metadata
///   addresses until [`with_private_network_access`](Self::with_private_network_access)
///   permits them, checks the page it ends on after redirects, and refuses host
///   names that do not resolve on the agent host until
///   [`with_unresolved_hosts`](Self::with_unresolved_hosts) permits them.
pub struct BrowserToolset {
    resolver: SessionResolver,
    /// Include navigation tools (navigate, back, forward, refresh)
    include_navigation: bool,
    /// Include interaction tools (click, type, select)
    include_interaction: bool,
    /// Include extraction tools (extract text, attributes, links, page info)
    include_extraction: bool,
    /// Include wait tools
    include_wait: bool,
    /// Include screenshot tool
    include_screenshot: bool,
    /// Include the JavaScript helper tools (scroll, hover, alert handling)
    include_js: bool,
    /// Include `browser_evaluate_js`, which runs arbitrary model-written JavaScript
    include_evaluate_js: bool,
    /// Include cookie management tools
    include_cookies: bool,
    /// Include window/tab management tools
    include_windows: bool,
    /// Include frame/iframe management tools
    include_frames: bool,
    /// Include advanced action tools (drag-drop, focus, key presses, etc.)
    include_actions: bool,
    /// Directories `browser_file_upload` may read from; the tool is offered only when set
    upload_roots: Option<Vec<PathBuf>>,
    /// URL schemes the navigation and new-tab/window tools accept
    allowed_schemes: Vec<String>,
    /// Whether the navigation, history, and new-tab/window tools may open private network
    /// addresses
    allow_private_network: bool,
    /// Whether those tools may open host names that do not resolve on the agent host
    allow_unresolved_hosts: bool,
}

impl BrowserToolset {
    /// Create a new toolset with every tool enabled except `browser_evaluate_js` and
    /// `browser_file_upload`.
    ///
    /// Equivalent to [`with_profile`](Self::with_profile) with [`BrowserProfile::Full`].
    pub fn new(browser: Arc<BrowserSession>) -> Self {
        Self::from_profile(SessionResolver::Fixed(browser), BrowserProfile::Full)
    }

    /// Create a pool-backed toolset with every tool enabled except `browser_evaluate_js`
    /// and `browser_file_upload`.
    ///
    /// Sessions are resolved per-user at runtime via `Toolset::tools(ctx)`.
    /// The pool calls `get_or_create(ctx.user_id())` to obtain an isolated
    /// browser session for each user.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_browser::{BrowserToolset, BrowserSessionPool, BrowserConfig};
    /// use std::sync::Arc;
    ///
    /// let pool = Arc::new(BrowserSessionPool::new(BrowserConfig::default()));
    /// let toolset = BrowserToolset::with_pool(pool);
    /// ```
    pub fn with_pool(pool: Arc<BrowserSessionPool>) -> Self {
        Self::from_profile(SessionResolver::Pool(pool), BrowserProfile::Full)
    }

    /// Create a pool-backed toolset with a pre-configured profile.
    ///
    /// Combines pool-backed session resolution with profile-based tool
    /// category selection.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_browser::{BrowserToolset, BrowserProfile, BrowserSessionPool, BrowserConfig};
    /// use std::sync::Arc;
    ///
    /// let pool = Arc::new(BrowserSessionPool::new(BrowserConfig::default()));
    /// let toolset = BrowserToolset::with_pool_and_profile(pool, BrowserProfile::Minimal);
    /// ```
    pub fn with_pool_and_profile(pool: Arc<BrowserSessionPool>, profile: BrowserProfile) -> Self {
        Self::from_profile(SessionResolver::Pool(pool), profile)
    }

    /// Create a toolset with a pre-configured profile.
    ///
    /// This is the recommended way to create a toolset for most agents.
    /// Using `BrowserProfile::Full` is equivalent to `BrowserToolset::new()`.
    pub fn with_profile(browser: Arc<BrowserSession>, profile: BrowserProfile) -> Self {
        Self::from_profile(SessionResolver::Fixed(browser), profile)
    }

    fn from_profile(resolver: SessionResolver, profile: BrowserProfile) -> Self {
        let (interaction, wait, js, extended) = match profile {
            BrowserProfile::Minimal | BrowserProfile::FormFilling => (true, true, false, false),
            BrowserProfile::Scraping => (false, false, true, false),
            BrowserProfile::Full => (true, true, true, true),
        };
        Self {
            resolver,
            include_navigation: true,
            include_interaction: interaction,
            include_extraction: true,
            include_wait: wait,
            include_screenshot: true,
            include_js: js,
            include_evaluate_js: false,
            include_cookies: extended,
            include_windows: extended,
            include_frames: extended,
            include_actions: extended,
            upload_roots: None,
            allowed_schemes: default_allowed_schemes(),
            allow_private_network: false,
            allow_unresolved_hosts: false,
        }
    }

    /// Enable or disable navigation tools.
    pub fn with_navigation(mut self, enabled: bool) -> Self {
        self.include_navigation = enabled;
        self
    }

    /// Enable or disable interaction tools.
    pub fn with_interaction(mut self, enabled: bool) -> Self {
        self.include_interaction = enabled;
        self
    }

    /// Enable or disable extraction tools.
    pub fn with_extraction(mut self, enabled: bool) -> Self {
        self.include_extraction = enabled;
        self
    }

    /// Enable or disable wait tools.
    pub fn with_wait(mut self, enabled: bool) -> Self {
        self.include_wait = enabled;
        self
    }

    /// Enable or disable screenshot tool.
    pub fn with_screenshot(mut self, enabled: bool) -> Self {
        self.include_screenshot = enabled;
        self
    }

    /// Enable or disable every JavaScript tool: scroll, hover, alert handling, and
    /// `browser_evaluate_js`.
    ///
    /// `with_js(true)` is an explicit opt-in to `browser_evaluate_js`, which runs
    /// model-written JavaScript in the page. To keep the helpers without it, follow
    /// with `.with_evaluate_js(false)`.
    pub fn with_js(mut self, enabled: bool) -> Self {
        self.include_js = enabled;
        self.include_evaluate_js = enabled;
        self
    }

    /// Enable or disable `browser_evaluate_js`.
    ///
    /// Off by default. The tool runs arbitrary model-written JavaScript in the
    /// current page, with the page's cookies and session, so a prompt-injected page
    /// can steer it. Enable it only for agents that need it.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_browser::{BrowserConfig, BrowserSession, BrowserToolset};
    /// use std::sync::Arc;
    ///
    /// let browser = Arc::new(BrowserSession::new(BrowserConfig::default()));
    /// let toolset = BrowserToolset::new(browser).with_evaluate_js(true);
    /// ```
    pub fn with_evaluate_js(mut self, enabled: bool) -> Self {
        self.include_evaluate_js = enabled;
        self
    }

    /// Replace the URL schemes `browser_navigate`, `browser_new_tab`, and
    /// `browser_new_window` accept.
    ///
    /// Defaults to [`DEFAULT_ALLOWED_SCHEMES`](crate::DEFAULT_ALLOWED_SCHEMES)
    /// (`http`, `https`). Schemes are compared case-insensitively. Allowing `file`
    /// exposes the host filesystem to the model, and allowing `javascript` or `data`
    /// lets it run script without `browser_evaluate_js`.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_browser::{BrowserConfig, BrowserSession, BrowserToolset};
    /// use std::sync::Arc;
    ///
    /// let browser = Arc::new(BrowserSession::new(BrowserConfig::default()));
    /// // HTTPS only.
    /// let toolset = BrowserToolset::new(browser).with_allowed_schemes(["https"]);
    /// ```
    #[must_use]
    pub fn with_allowed_schemes<I, S>(mut self, schemes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.allowed_schemes = schemes.into_iter().map(Into::into).collect();
        self
    }

    /// Permit or refuse navigation to loopback, private, link-local, and cloud
    /// metadata addresses.
    ///
    /// Refused by default, in `browser_navigate`, `browser_new_tab`, and
    /// `browser_new_window`, and for the page those tools and `browser_back`,
    /// `browser_forward`, and `browser_refresh` end on after redirects. Enable this
    /// only for an agent meant to browse an internal network; see
    /// [`NavigateTool::with_private_network_access`](crate::tools::NavigateTool::with_private_network_access).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_browser::{BrowserConfig, BrowserSession, BrowserToolset};
    /// use std::sync::Arc;
    ///
    /// let browser = Arc::new(BrowserSession::new(BrowserConfig::default()));
    /// let intranet = BrowserToolset::new(browser).with_private_network_access(true);
    /// ```
    #[must_use]
    pub fn with_private_network_access(mut self, enabled: bool) -> Self {
        self.allow_private_network = enabled;
        self
    }

    /// Permit or refuse navigation to host names that do not resolve on the agent host.
    ///
    /// Refused by default, since the agent cannot check where such a name leads. Enable
    /// this when the browser resolves names the agent host cannot, such as behind a proxy
    /// or on a remote WebDriver host; see
    /// [`NavigateTool::with_unresolved_hosts`](crate::tools::NavigateTool::with_unresolved_hosts).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_browser::{BrowserConfig, BrowserSession, BrowserToolset};
    /// use std::sync::Arc;
    ///
    /// let browser = Arc::new(BrowserSession::new(BrowserConfig::default()));
    /// let proxied = BrowserToolset::new(browser).with_unresolved_hosts(true);
    /// ```
    #[must_use]
    pub fn with_unresolved_hosts(mut self, enabled: bool) -> Self {
        self.allow_unresolved_hosts = enabled;
        self
    }

    /// Offer `browser_file_upload`, limited to files inside `roots`.
    ///
    /// Off by default in every profile. The tool resolves each model-chosen path,
    /// symlinks included, and refuses anything that is not an existing file under a
    /// resolved root, so point `roots` at directories that hold only what the agent
    /// may hand to a web page.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_browser::{BrowserConfig, BrowserSession, BrowserToolset};
    /// use std::sync::Arc;
    ///
    /// let browser = Arc::new(BrowserSession::new(BrowserConfig::default()));
    /// let toolset = BrowserToolset::new(browser).with_file_upload(["/srv/agent/uploads"]);
    /// ```
    #[must_use]
    pub fn with_file_upload<I, P>(mut self, roots: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        self.upload_roots = Some(roots.into_iter().map(Into::into).collect());
        self
    }

    /// Enable or disable cookie management tools.
    pub fn with_cookies(mut self, enabled: bool) -> Self {
        self.include_cookies = enabled;
        self
    }

    /// Enable or disable window/tab management tools.
    pub fn with_windows(mut self, enabled: bool) -> Self {
        self.include_windows = enabled;
        self
    }

    /// Enable or disable frame/iframe management tools.
    pub fn with_frames(mut self, enabled: bool) -> Self {
        self.include_frames = enabled;
        self
    }

    /// Enable or disable advanced action tools.
    pub fn with_actions(mut self, enabled: bool) -> Self {
        self.include_actions = enabled;
        self
    }

    /// Get all tools as a vector (synchronous version).
    ///
    /// Works for fixed-session toolsets. For pool-backed toolsets,
    /// returns an empty vec with a warning — use `Toolset::tools(ctx)` or
    /// `try_all_tools()` instead.
    pub fn all_tools(&self) -> Vec<Arc<dyn Tool>> {
        match &self.resolver {
            SessionResolver::Fixed(session) => self.build_tools(session.clone()),
            SessionResolver::Pool(_) => {
                tracing::warn!(
                    "BrowserToolset::all_tools() called on a pool-backed toolset. \
                     Returns empty vec. Use Toolset::tools(ctx) instead."
                );
                Vec::new()
            }
        }
    }

    /// Try to get all tools synchronously. Returns an error for pool-backed toolsets.
    ///
    /// Prefer `Toolset::tools(ctx)` for pool-backed toolsets.
    pub fn try_all_tools(&self) -> Result<Vec<Arc<dyn Tool>>> {
        match &self.resolver {
            SessionResolver::Fixed(session) => Ok(self.build_tools(session.clone())),
            SessionResolver::Pool(_) => Err(adk_core::AdkError::tool(
                "Cannot resolve tools synchronously for a pool-backed BrowserToolset. \
                 Use Toolset::tools(ctx) instead.",
            )),
        }
    }

    /// Internal: build tool instances from a resolved session.
    fn build_tools(&self, browser: Arc<BrowserSession>) -> Vec<Arc<dyn Tool>> {
        let mut tools: Vec<Arc<dyn Tool>> = Vec::new();

        if self.include_navigation {
            tools.push(Arc::new(
                NavigateTool::new(browser.clone())
                    .with_allowed_schemes(self.allowed_schemes.clone())
                    .with_private_network_access(self.allow_private_network)
                    .with_unresolved_hosts(self.allow_unresolved_hosts),
            ));
            tools.push(Arc::new(
                BackTool::new(browser.clone())
                    .with_private_network_access(self.allow_private_network)
                    .with_unresolved_hosts(self.allow_unresolved_hosts),
            ));
            tools.push(Arc::new(
                ForwardTool::new(browser.clone())
                    .with_private_network_access(self.allow_private_network)
                    .with_unresolved_hosts(self.allow_unresolved_hosts),
            ));
            tools.push(Arc::new(
                RefreshTool::new(browser.clone())
                    .with_private_network_access(self.allow_private_network)
                    .with_unresolved_hosts(self.allow_unresolved_hosts),
            ));
        }

        if self.include_interaction {
            tools.push(Arc::new(ClickTool::new(browser.clone())));
            tools.push(Arc::new(DoubleClickTool::new(browser.clone())));
            tools.push(Arc::new(TypeTool::new(browser.clone())));
            tools.push(Arc::new(ClearTool::new(browser.clone())));
            tools.push(Arc::new(SelectTool::new(browser.clone())));
        }

        if self.include_extraction {
            tools.push(Arc::new(ExtractTextTool::new(browser.clone())));
            tools.push(Arc::new(ExtractAttributeTool::new(browser.clone())));
            tools.push(Arc::new(ExtractLinksTool::new(browser.clone())));
            tools.push(Arc::new(PageInfoTool::new(browser.clone())));
            tools.push(Arc::new(PageSourceTool::new(browser.clone())));
        }

        if self.include_wait {
            tools.push(Arc::new(WaitForElementTool::new(browser.clone())));
            tools.push(Arc::new(WaitTool::new()));
            tools.push(Arc::new(WaitForPageLoadTool::new(browser.clone())));
            tools.push(Arc::new(WaitForTextTool::new(browser.clone())));
        }

        if self.include_screenshot {
            tools.push(Arc::new(ScreenshotTool::new(browser.clone())));
        }

        if self.include_evaluate_js {
            tools.push(Arc::new(EvaluateJsTool::new(browser.clone())));
        }

        if self.include_js {
            tools.push(Arc::new(ScrollTool::new(browser.clone())));
            tools.push(Arc::new(HoverTool::new(browser.clone())));
            tools.push(Arc::new(AlertTool::new(browser.clone())));
        }

        if self.include_cookies {
            tools.push(Arc::new(GetCookiesTool::new(browser.clone())));
            tools.push(Arc::new(GetCookieTool::new(browser.clone())));
            tools.push(Arc::new(AddCookieTool::new(browser.clone())));
            tools.push(Arc::new(DeleteCookieTool::new(browser.clone())));
            tools.push(Arc::new(DeleteAllCookiesTool::new(browser.clone())));
        }

        if self.include_windows {
            tools.push(Arc::new(ListWindowsTool::new(browser.clone())));
            tools.push(Arc::new(
                NewTabTool::new(browser.clone())
                    .with_allowed_schemes(self.allowed_schemes.clone())
                    .with_private_network_access(self.allow_private_network)
                    .with_unresolved_hosts(self.allow_unresolved_hosts),
            ));
            tools.push(Arc::new(
                NewWindowTool::new(browser.clone())
                    .with_allowed_schemes(self.allowed_schemes.clone())
                    .with_private_network_access(self.allow_private_network)
                    .with_unresolved_hosts(self.allow_unresolved_hosts),
            ));
            tools.push(Arc::new(SwitchWindowTool::new(browser.clone())));
            tools.push(Arc::new(CloseWindowTool::new(browser.clone())));
            tools.push(Arc::new(MaximizeWindowTool::new(browser.clone())));
            tools.push(Arc::new(MinimizeWindowTool::new(browser.clone())));
            tools.push(Arc::new(SetWindowSizeTool::new(browser.clone())));
        }

        if self.include_frames {
            tools.push(Arc::new(SwitchToFrameTool::new(browser.clone())));
            tools.push(Arc::new(SwitchToParentFrameTool::new(browser.clone())));
            tools.push(Arc::new(SwitchToDefaultContentTool::new(browser.clone())));
        }

        if self.include_actions {
            tools.push(Arc::new(DragAndDropTool::new(browser.clone())));
            tools.push(Arc::new(RightClickTool::new(browser.clone())));
            tools.push(Arc::new(FocusTool::new(browser.clone())));
            tools.push(Arc::new(ElementStateTool::new(browser.clone())));
            tools.push(Arc::new(PressKeyTool::new(browser.clone())));
            tools.push(Arc::new(PrintToPdfTool::new(browser.clone())));
        }

        if let Some(roots) = &self.upload_roots {
            tools.push(Arc::new(FileUploadTool::new(browser).with_allowed_roots(roots.clone())));
        }

        tools
    }
}

#[async_trait]
impl Toolset for BrowserToolset {
    fn name(&self) -> &str {
        "browser"
    }

    async fn tools(&self, ctx: Arc<dyn ReadonlyContext>) -> Result<Vec<Arc<dyn Tool>>> {
        let session = self.resolver.resolve(&ctx).await?;
        Ok(self.build_tools(session))
    }
}

/// Helper function to create a minimal browser toolset with only essential tools.
pub fn minimal_browser_tools(browser: Arc<BrowserSession>) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(NavigateTool::new(browser.clone())),
        Arc::new(ClickTool::new(browser.clone())),
        Arc::new(TypeTool::new(browser.clone())),
        Arc::new(ExtractTextTool::new(browser.clone())),
        Arc::new(WaitForElementTool::new(browser.clone())),
        Arc::new(ScreenshotTool::new(browser)),
    ]
}

/// Helper function to create a read-only browser toolset (no interaction).
pub fn readonly_browser_tools(browser: Arc<BrowserSession>) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(NavigateTool::new(browser.clone())),
        Arc::new(ExtractTextTool::new(browser.clone())),
        Arc::new(ExtractAttributeTool::new(browser.clone())),
        Arc::new(ExtractLinksTool::new(browser.clone())),
        Arc::new(PageInfoTool::new(browser.clone())),
        Arc::new(ScreenshotTool::new(browser.clone())),
        Arc::new(ScrollTool::new(browser)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::BrowserConfig;

    #[test]
    fn test_toolset_all_tools() {
        let browser = Arc::new(BrowserSession::new(BrowserConfig::default()));
        let toolset = BrowserToolset::new(browser);
        let tools = toolset.all_tools();

        // Every tool except browser_evaluate_js and browser_file_upload (44)
        assert_eq!(tools.len(), 44);

        // Check some tool names exist
        let tool_names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert!(tool_names.contains(&"browser_navigate"));
        assert!(tool_names.contains(&"browser_click"));
        assert!(tool_names.contains(&"browser_type"));
        assert!(tool_names.contains(&"browser_screenshot"));
        // New tools
        assert!(tool_names.contains(&"browser_get_cookies"));
        assert!(tool_names.contains(&"browser_new_tab"));
        assert!(tool_names.contains(&"browser_switch_to_frame"));
        assert!(tool_names.contains(&"browser_drag_and_drop"));
    }

    #[test]
    fn test_toolset_selective() {
        let browser = Arc::new(BrowserSession::new(BrowserConfig::default()));
        let toolset = BrowserToolset::new(browser)
            .with_navigation(true)
            .with_interaction(false)
            .with_extraction(false)
            .with_wait(false)
            .with_screenshot(false)
            .with_js(false)
            .with_cookies(false)
            .with_windows(false)
            .with_frames(false)
            .with_actions(false);

        let tools = toolset.all_tools();

        // Should only have navigation tools
        assert_eq!(tools.len(), 4);
    }

    #[test]
    fn test_minimal_tools() {
        let browser = Arc::new(BrowserSession::new(BrowserConfig::default()));
        let tools = minimal_browser_tools(browser);

        assert_eq!(tools.len(), 6);
    }

    #[test]
    fn test_profile_form_filling() {
        let browser = Arc::new(BrowserSession::new(BrowserConfig::default()));
        let toolset = BrowserToolset::with_profile(browser, BrowserProfile::FormFilling);
        let tools = toolset.all_tools();

        // Navigation (4) + Interaction (5) + Extraction (5) + Wait (4) + Screenshot (1) = 19
        let tool_names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert!(tool_names.contains(&"browser_navigate"));
        assert!(tool_names.contains(&"browser_click"));
        assert!(tool_names.contains(&"browser_type"));
        assert!(tool_names.contains(&"browser_select"));
        assert!(tool_names.contains(&"browser_clear"));
        assert!(tool_names.contains(&"browser_screenshot"));
        // Should NOT include cookies, windows, frames, actions
        assert!(!tool_names.contains(&"browser_get_cookies"));
        assert!(!tool_names.contains(&"browser_new_tab"));
        assert!(!tool_names.contains(&"browser_switch_to_frame"));
        assert!(!tool_names.contains(&"browser_drag_and_drop"));
    }

    #[test]
    fn test_profile_scraping() {
        let browser = Arc::new(BrowserSession::new(BrowserConfig::default()));
        let toolset = BrowserToolset::with_profile(browser, BrowserProfile::Scraping);
        let tools = toolset.all_tools();

        let tool_names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert!(tool_names.contains(&"browser_navigate"));
        assert!(tool_names.contains(&"browser_extract_text"));
        assert!(tool_names.contains(&"browser_extract_links"));
        assert!(tool_names.contains(&"browser_screenshot"));
        assert!(tool_names.contains(&"browser_scroll"));
        // Should NOT include interaction tools
        assert!(!tool_names.contains(&"browser_click"));
        assert!(!tool_names.contains(&"browser_type"));
    }

    #[test]
    fn test_profile_full_matches_new() {
        let browser1 = Arc::new(BrowserSession::new(BrowserConfig::default()));
        let browser2 = Arc::new(BrowserSession::new(BrowserConfig::default()));
        let full = BrowserToolset::with_profile(browser1, BrowserProfile::Full);
        let default = BrowserToolset::new(browser2);
        assert_eq!(full.all_tools().len(), default.all_tools().len());
    }
}
