//! What a model can reach through the browser toolset by default.
//!
//! `browser_navigate` only checked that the URL parsed, so `file:///etc/passwd`,
//! `javascript:` and `data:` URLs, and browser-internal pages such as `chrome://settings`
//! were all opened. `BrowserToolset::new` also included `browser_evaluate_js`, which runs
//! arbitrary model-written JavaScript in the page.
//!
//! None of these tests needs a WebDriver server: a refused URL is rejected before the
//! session starts, and the session points at an unreachable address.

use adk_browser::{
    BrowserConfig, BrowserProfile, BrowserSession, BrowserToolset, DEFAULT_ALLOWED_SCHEMES,
    NavigateTool, NewTabTool, NewWindowTool,
};
use adk_core::{
    CallbackContext, Content, EventActions, MemoryEntry, ReadonlyContext, Result, Tool, ToolContext,
};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

struct TestCtx {
    content: Content,
}

#[async_trait]
impl ReadonlyContext for TestCtx {
    fn invocation_id(&self) -> &str {
        "inv-1"
    }
    fn agent_name(&self) -> &str {
        "test-agent"
    }
    fn user_id(&self) -> &str {
        "user-1"
    }
    fn app_name(&self) -> &str {
        "test-app"
    }
    fn session_id(&self) -> &str {
        "session-1"
    }
    fn branch(&self) -> &str {
        ""
    }
    fn user_content(&self) -> &Content {
        &self.content
    }
}

#[async_trait]
impl CallbackContext for TestCtx {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}

#[async_trait]
impl ToolContext for TestCtx {
    fn function_call_id(&self) -> &str {
        "call-1"
    }
    fn actions(&self) -> EventActions {
        EventActions::default()
    }
    fn set_actions(&self, _actions: EventActions) {}
    async fn search_memory(&self, _query: &str) -> Result<Vec<MemoryEntry>> {
        Ok(vec![])
    }
}

/// A session whose WebDriver is unreachable, so it can never start.
fn offline_session() -> Arc<BrowserSession> {
    Arc::new(BrowserSession::new(BrowserConfig::new().webdriver_url("http://127.0.0.1:1")))
}

/// Runs `tool` with a `url` argument and returns its error message.
async fn error_for(tool: &dyn Tool, url: &str) -> String {
    let ctx: Arc<dyn ToolContext> = Arc::new(TestCtx { content: Content::new("user") });
    tool.execute(ctx, json!({ "url": url })).await.expect_err("the call must fail").to_string()
}

fn names(toolset: &BrowserToolset) -> Vec<String> {
    toolset.all_tools().iter().map(|tool| tool.name().to_string()).collect()
}

const REFUSED: &[&str] = &[
    "file:///etc/passwd",
    "FILE:///etc/passwd",
    "javascript:alert(document.cookie)",
    "data:text/html,<script>alert(1)</script>",
    "chrome://settings",
    "about:blank",
];

// ── Navigation accepts only http and https by default ─────────────────

#[test]
fn the_default_allowlist_is_http_and_https() {
    assert_eq!(DEFAULT_ALLOWED_SCHEMES, &["http", "https"]);
}

#[tokio::test]
async fn navigate_refuses_non_web_schemes_before_touching_the_browser() {
    let session = offline_session();
    let tool = NavigateTool::new(session.clone());

    for url in REFUSED {
        let message = error_for(&tool, url).await;
        assert!(message.contains("is not allowed"), "{url} was not refused by scheme: {message}");
    }
    assert!(!session.is_active().await, "a refused URL must not start the browser");
}

#[tokio::test]
async fn navigate_lets_web_urls_through_to_the_browser() {
    let tool = NavigateTool::new(offline_session());

    for url in ["https://example.com", "http://example.com/path?q=1", "HTTPS://EXAMPLE.COM"] {
        // The call fails only because the WebDriver is unreachable.
        let message = error_for(&tool, url).await;
        assert!(!message.contains("is not allowed"), "{url} was refused by scheme: {message}");
    }
}

#[tokio::test]
async fn new_tab_and_new_window_refuse_non_web_schemes() {
    let session = offline_session();
    let tools: [Arc<dyn Tool>; 2] =
        [Arc::new(NewTabTool::new(session.clone())), Arc::new(NewWindowTool::new(session.clone()))];

    for tool in &tools {
        let message = error_for(tool.as_ref(), "file:///etc/passwd").await;
        assert!(message.contains("is not allowed"), "{} accepted a file URL", tool.name());
    }
    assert!(!session.is_active().await, "a refused URL must not open a tab or window");
}

#[tokio::test]
async fn the_toolset_allowlist_reaches_every_url_taking_tool() {
    let toolset = BrowserToolset::new(offline_session()).with_allowed_schemes(["https"]);

    for name in ["browser_navigate", "browser_new_tab", "browser_new_window"] {
        let tool = toolset
            .all_tools()
            .into_iter()
            .find(|tool| tool.name() == name)
            .expect("the full toolset must expose the tool");
        let message = error_for(tool.as_ref(), "http://example.com").await;
        assert!(message.contains("is not allowed"), "{name} accepted http: {message}");
    }
}

#[tokio::test]
async fn a_custom_allowlist_can_admit_another_scheme() {
    let tool = NavigateTool::new(offline_session()).with_allowed_schemes(["https", "file"]);

    let message = error_for(&tool, "file:///tmp/report.html").await;
    assert!(!message.contains("is not allowed"), "an allowed scheme was refused: {message}");
}

// ── browser_evaluate_js is opt-in ──────────────────────────────────────

#[test]
fn the_default_toolset_does_not_expose_evaluate_js() {
    let listed = names(&BrowserToolset::new(offline_session()));

    assert!(!listed.contains(&"browser_evaluate_js".to_string()));
    // The fixed-script JavaScript helpers stay available.
    for helper in ["browser_scroll", "browser_hover", "browser_handle_alert"] {
        assert!(listed.contains(&helper.to_string()), "{helper} is missing");
    }
}

#[test]
fn no_profile_exposes_evaluate_js() {
    for profile in [
        BrowserProfile::Minimal,
        BrowserProfile::FormFilling,
        BrowserProfile::Scraping,
        BrowserProfile::Full,
    ] {
        let listed = names(&BrowserToolset::with_profile(offline_session(), profile));
        assert!(!listed.contains(&"browser_evaluate_js".to_string()), "{profile:?} exposes it");
    }
}

#[test]
fn evaluate_js_can_be_enabled_explicitly() {
    for toolset in [
        BrowserToolset::new(offline_session()).with_evaluate_js(true),
        BrowserToolset::new(offline_session()).with_js(true),
        BrowserToolset::with_profile(offline_session(), BrowserProfile::Scraping)
            .with_evaluate_js(true),
    ] {
        assert!(names(&toolset).contains(&"browser_evaluate_js".to_string()));
    }
}

#[test]
fn with_js_false_removes_every_javascript_tool() {
    let listed =
        names(&BrowserToolset::new(offline_session()).with_evaluate_js(true).with_js(false));

    for tool in ["browser_evaluate_js", "browser_scroll", "browser_hover", "browser_handle_alert"] {
        assert!(!listed.contains(&tool.to_string()), "{tool} is still exposed");
    }
}
