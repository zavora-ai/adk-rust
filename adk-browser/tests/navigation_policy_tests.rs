//! What a model can reach through the browser toolset by default.
//!
//! `browser_navigate` only checked that the URL parsed, so `file:///etc/passwd`,
//! `javascript:` and `data:` URLs, and browser-internal pages such as `chrome://settings`
//! were all opened. `BrowserToolset::new` also included `browser_evaluate_js`, which runs
//! arbitrary model-written JavaScript in the page.
//!
//! Navigation also checked the scheme alone, so `http://169.254.169.254/` reached a cloud
//! metadata service and `http://localhost/` the host's own services, and the full profile
//! offered `browser_file_upload` for any model-chosen path.
//!
//! None of these tests needs a WebDriver server: a refused URL is rejected before the
//! session starts, and the session points at an unreachable address.

use adk_browser::{
    BrowserConfig, BrowserProfile, BrowserSession, BrowserToolset, DEFAULT_ALLOWED_SCHEMES,
    FileUploadTool, NavigateTool, NewTabTool, NewWindowTool,
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

// ── Private network addresses are refused by default ──────────────────

const PRIVATE: &[&str] = &[
    "http://127.0.0.1:8080/admin",
    "http://localhost:3000/",
    "http://LOCALHOST./",
    "http://app.localhost/",
    "http://169.254.169.254/latest/meta-data/",
    "http://metadata.google.internal/computeMetadata/v1/",
    "http://100.100.100.200/latest/meta-data/",
    "http://10.0.0.5/",
    "http://172.16.0.1/",
    "http://192.168.1.1/",
    "http://0.0.0.0:8080/",
    "http://2130706433/",
    "http://0x7f.1/",
    "http://[::1]/",
    "http://[fd00:ec2::254]/",
    "http://[fe80::1]/",
    "http://[::ffff:169.254.169.254]/",
    "http://[64:ff9b::a9fe:a9fe]/",
];

#[tokio::test]
async fn navigate_refuses_private_addresses_before_touching_the_browser() {
    let session = offline_session();
    let tool = NavigateTool::new(session.clone());

    for url in PRIVATE {
        let message = error_for(&tool, url).await;
        assert!(message.contains("private network"), "{url} was not refused: {message}");
    }
    assert!(!session.is_active().await, "a refused URL must not start the browser");
}

#[tokio::test]
async fn new_tab_and_new_window_refuse_private_addresses() {
    let session = offline_session();
    let tools: [Arc<dyn Tool>; 2] =
        [Arc::new(NewTabTool::new(session.clone())), Arc::new(NewWindowTool::new(session.clone()))];

    for tool in &tools {
        let message = error_for(tool.as_ref(), "http://169.254.169.254/").await;
        assert!(message.contains("private network"), "{} opened the metadata address", tool.name());
    }
    assert!(!session.is_active().await);
}

#[tokio::test]
async fn private_network_access_is_an_explicit_opt_in() {
    let tool = NavigateTool::new(offline_session()).with_private_network_access(true);
    let message = error_for(&tool, "http://127.0.0.1:8080/").await;
    assert!(!message.contains("private network"), "the opt-in was ignored: {message}");

    let toolset = BrowserToolset::new(offline_session()).with_private_network_access(true);
    for name in ["browser_navigate", "browser_new_tab", "browser_new_window"] {
        let tool = toolset
            .all_tools()
            .into_iter()
            .find(|tool| tool.name() == name)
            .expect("the full toolset must expose the tool");
        let message = error_for(tool.as_ref(), "http://10.0.0.5/").await;
        assert!(!message.contains("private network"), "{name} ignored the opt-in: {message}");
    }
}

#[tokio::test]
async fn public_addresses_still_reach_the_browser() {
    let tool = NavigateTool::new(offline_session());

    for url in ["http://93.184.215.14/", "https://[2606:4700::1111]/", "http://100.128.0.1/"] {
        let message = error_for(&tool, url).await;
        assert!(!message.contains("private network"), "{url} was refused: {message}");
    }
}

// ── browser_file_upload is opt-in and confined to its roots ───────────

#[test]
fn no_profile_exposes_file_upload() {
    for toolset in [
        BrowserToolset::new(offline_session()),
        BrowserToolset::new(offline_session()).with_actions(true),
        BrowserToolset::with_profile(offline_session(), BrowserProfile::Full),
        BrowserToolset::with_profile(offline_session(), BrowserProfile::Minimal),
    ] {
        assert!(!names(&toolset).contains(&"browser_file_upload".to_string()));
    }
}

#[test]
fn file_upload_is_offered_once_roots_are_configured() {
    let toolset = BrowserToolset::with_profile(offline_session(), BrowserProfile::Minimal)
        .with_file_upload(["/srv/uploads"]);

    assert!(names(&toolset).contains(&"browser_file_upload".to_string()));
}

/// Runs `tool` with a `file_path` argument and returns its error message.
async fn upload_error(tool: &dyn Tool, file_path: &std::path::Path) -> String {
    let ctx: Arc<dyn ToolContext> = Arc::new(TestCtx { content: Content::new("user") });
    tool.execute(ctx, json!({ "selector": "input[type=file]", "file_path": file_path }))
        .await
        .expect_err("the call must fail")
        .to_string()
}

#[tokio::test]
async fn upload_without_roots_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("report.pdf");
    std::fs::write(&file, b"pdf").unwrap();
    let session = offline_session();

    let message = upload_error(&FileUploadTool::new(session.clone()), &file).await;

    assert!(message.contains("no upload roots"), "{message}");
    assert!(!session.is_active().await);
}

#[tokio::test]
async fn upload_outside_the_roots_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let secret = outside.path().join("id_rsa");
    std::fs::write(&secret, b"key").unwrap();
    let session = offline_session();
    let tool = FileUploadTool::new(session.clone()).with_allowed_roots([root.path()]);

    let traversal = root.path().join("..").join(outside.path().file_name().unwrap()).join("id_rsa");
    for path in [secret.clone(), traversal, root.path().join("missing.txt"), root.path().into()] {
        let message = upload_error(&tool, &path).await;
        assert!(message.contains("not an existing file inside"), "{path:?}: {message}");
    }
    assert!(!session.is_active().await, "a refused upload must not start the browser");
}

#[cfg(unix)]
#[tokio::test]
async fn a_symlink_inside_the_root_cannot_upload_a_file_outside_it() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let secret = outside.path().join("id_rsa");
    std::fs::write(&secret, b"key").unwrap();
    std::os::unix::fs::symlink(&secret, root.path().join("innocent.txt")).unwrap();
    let tool = FileUploadTool::new(offline_session()).with_allowed_roots([root.path()]);

    let message = upload_error(&tool, &root.path().join("innocent.txt")).await;

    assert!(message.contains("not an existing file inside"), "{message}");
}

#[tokio::test]
async fn upload_inside_the_roots_reaches_the_browser() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("report.pdf");
    std::fs::write(&file, b"pdf").unwrap();
    let tool = FileUploadTool::new(offline_session()).with_allowed_roots([root.path()]);

    // The call fails only because the WebDriver is unreachable.
    let message = upload_error(&tool, &file).await;
    assert!(!message.contains("upload root"), "an allowed file was refused: {message}");
}
