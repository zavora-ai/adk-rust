//! The page a navigation ends on is checked against the private network policy, and host
//! names that do not resolve on the agent host are refused unless permitted.
//!
//! The URL check ran only on the URL the model asked for, so a public page that redirected
//! to `http://169.254.169.254/` was loaded and returned to the model, and a host name the
//! agent host could not resolve was let through for the browser to resolve.
//!
//! A fake WebDriver server plays the browser: it records every navigation and reports a
//! scripted redirect target as the current URL.

use adk_browser::{
    BackTool, BrowserConfig, BrowserSession, BrowserToolset, NavigateTool, NewTabTool, RefreshTool,
};
use adk_core::{
    CallbackContext, Content, EventActions, MemoryEntry, ReadonlyContext, Result, Tool, ToolContext,
};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

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

/// Browser state behind the fake WebDriver: one tab's history plus scripted redirects.
#[derive(Default)]
struct Browser {
    /// Requested URL → URL the browser ends on.
    redirects: HashMap<String, String>,
    history: Vec<String>,
    index: usize,
    /// Every URL passed to `goto`, in order.
    navigations: Vec<String>,
}

impl Browser {
    fn current(&self) -> String {
        self.history.get(self.index).cloned().unwrap_or_else(|| "about:blank".to_string())
    }

    fn goto(&mut self, url: &str) {
        self.navigations.push(url.to_string());
        let landed = self.redirects.get(url).cloned().unwrap_or_else(|| url.to_string());
        if !self.history.is_empty() {
            self.history.truncate(self.index + 1);
        }
        self.history.push(landed);
        self.index = self.history.len() - 1;
    }
}

struct WebDriverFixture {
    address: SocketAddr,
    browser: Arc<Mutex<Browser>>,
    stopped: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl WebDriverFixture {
    fn start(redirects: &[(&str, &str)]) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let browser = Arc::new(Mutex::new(Browser {
            redirects: redirects
                .iter()
                .map(|(from, to)| (from.to_string(), to.to_string()))
                .collect(),
            ..Browser::default()
        }));
        let stopped = Arc::new(AtomicBool::new(false));
        let worker = {
            let browser = browser.clone();
            let stopped = stopped.clone();
            std::thread::spawn(move || {
                for connection in listener.incoming() {
                    if stopped.load(Ordering::SeqCst) {
                        break;
                    }
                    serve(connection.unwrap(), &browser);
                }
            })
        };
        Self { address, browser, stopped, worker: Some(worker) }
    }

    fn session(&self) -> Arc<BrowserSession> {
        let config = BrowserConfig::new().webdriver_url(format!("http://{}", self.address));
        Arc::new(BrowserSession::new(config))
    }

    fn navigations(&self) -> Vec<String> {
        self.browser.lock().unwrap().navigations.clone()
    }

    fn current(&self) -> String {
        self.browser.lock().unwrap().current()
    }
}

impl Drop for WebDriverFixture {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.address);
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}

/// Answers one WebDriver request.
fn serve(mut connection: TcpStream, browser: &Mutex<Browser>) {
    connection.set_read_timeout(Some(std::time::Duration::from_secs(2))).unwrap();
    let mut reader = BufReader::new(&mut connection);
    let mut request = String::new();
    if reader.read_line(&mut request).is_err() || request.is_empty() {
        return;
    }
    let mut length = 0;
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).unwrap();
        if header == "\r\n" || header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse::<usize>().unwrap();
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let mut parts = request.split_whitespace();
    let (method, path) = (parts.next().unwrap(), parts.next().unwrap());
    let endpoint = path.strip_prefix("/session/session-1").unwrap_or(path);

    let value = {
        let mut browser = browser.lock().unwrap();
        match (method, endpoint) {
            ("POST", "/session") => {
                json!({ "sessionId": "session-1", "capabilities": { "browserName": "chrome" } })
            }
            ("POST", "/url") => {
                browser.goto(body["url"].as_str().unwrap());
                Value::Null
            }
            ("GET", "/url") => json!(browser.current()),
            ("POST", "/back") => {
                browser.index = browser.index.saturating_sub(1);
                Value::Null
            }
            ("GET", "/title") => json!("Fixture page"),
            ("POST", "/execute/sync") => json!("page text"),
            ("POST", "/window/new") => json!({ "handle": "window-2", "type": "tab" }),
            (_, "/window/rect") => json!({ "x": 0, "y": 0, "width": 1280, "height": 720 }),
            _ => Value::Null,
        }
    };
    let body = json!({ "value": value }).to_string();
    write!(
        connection,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
}

const PUBLIC: &str = "http://93.184.215.14/start";
const METADATA: &str = "http://169.254.169.254/latest/meta-data/";

async fn run(tool: &dyn Tool, args: Value) -> Result<Value> {
    let ctx: Arc<dyn ToolContext> = Arc::new(TestCtx { content: Content::new("user") });
    tool.execute(ctx, args).await
}

#[tokio::test]
async fn a_redirect_to_a_private_address_is_refused_and_the_page_cleared() {
    let fixture = WebDriverFixture::start(&[(PUBLIC, METADATA)]);
    let tool = NavigateTool::new(fixture.session());

    let error = run(&tool, json!({ "url": PUBLIC })).await.unwrap_err().to_string();

    assert!(error.contains("private network"), "{error}");
    assert!(error.contains(METADATA), "{error}");
    assert_eq!(fixture.navigations(), vec![PUBLIC, "about:blank"]);
    assert_eq!(fixture.current(), "about:blank");
}

#[tokio::test]
async fn a_redirect_to_a_public_address_is_allowed() {
    let fixture = WebDriverFixture::start(&[(PUBLIC, "http://93.184.215.15/landing")]);
    let tool = NavigateTool::new(fixture.session());

    let result = run(&tool, json!({ "url": PUBLIC })).await.unwrap();

    assert_eq!(result["url"], json!("http://93.184.215.15/landing"));
    assert_eq!(fixture.navigations(), vec![PUBLIC]);
}

#[tokio::test]
async fn private_network_access_also_permits_the_redirect() {
    let fixture = WebDriverFixture::start(&[(PUBLIC, METADATA)]);
    let tool = NavigateTool::new(fixture.session()).with_private_network_access(true);

    let result = run(&tool, json!({ "url": PUBLIC })).await.unwrap();

    assert_eq!(result["url"], json!(METADATA));
}

#[tokio::test]
async fn going_back_to_a_refused_page_is_refused_again() {
    let fixture = WebDriverFixture::start(&[(PUBLIC, METADATA)]);
    let session = fixture.session();
    run(&NavigateTool::new(session.clone()), json!({ "url": PUBLIC })).await.unwrap_err();

    // History now holds the refused page behind `about:blank`.
    let error = run(&BackTool::new(session.clone()), json!({})).await.unwrap_err().to_string();

    assert!(error.contains("private network"), "{error}");
    assert_eq!(fixture.current(), "about:blank");
}

#[tokio::test]
async fn refresh_checks_the_page_it_reloads() {
    let fixture = WebDriverFixture::start(&[]);
    let session = fixture.session();
    // An earlier tool opened the page with private network access.
    run(
        &NavigateTool::new(session.clone()).with_private_network_access(true),
        json!({ "url": METADATA }),
    )
    .await
    .unwrap();

    let error = run(&RefreshTool::new(session), json!({})).await.unwrap_err().to_string();

    assert!(error.contains("private network"), "{error}");
    assert_eq!(fixture.current(), "about:blank");
}

#[tokio::test]
async fn a_new_tab_that_redirects_to_a_private_address_is_refused() {
    let fixture = WebDriverFixture::start(&[(PUBLIC, METADATA)]);
    let tool = NewTabTool::new(fixture.session());

    let error = run(&tool, json!({ "url": PUBLIC })).await.unwrap_err().to_string();

    assert!(error.contains("private network"), "{error}");
    assert_eq!(fixture.current(), "about:blank");
}

#[tokio::test]
async fn the_toolset_checks_landing_pages_in_every_navigating_tool() {
    let fixture = WebDriverFixture::start(&[(PUBLIC, METADATA)]);
    let toolset = BrowserToolset::new(fixture.session());
    let tool = |name: &str| {
        toolset.all_tools().into_iter().find(|tool| tool.name() == name).expect("tool is exposed")
    };

    for name in ["browser_navigate", "browser_new_tab", "browser_new_window"] {
        let error = run(tool(name).as_ref(), json!({ "url": PUBLIC })).await.unwrap_err();
        assert!(error.to_string().contains("private network"), "{name}: {error}");
    }
    for name in ["browser_back", "browser_forward", "browser_refresh"] {
        // Put the refused page back as the current one, as a history step would.
        {
            let mut browser = fixture.browser.lock().unwrap();
            browser.history = vec![METADATA.to_string()];
            browser.index = 0;
        }
        let error = run(tool(name).as_ref(), json!({})).await.unwrap_err();
        assert!(error.to_string().contains("private network"), "{name}: {error}");
    }
}

// ── Host names that do not resolve on the agent host ──────────────────

/// `.invalid` is reserved and never resolves (RFC 6761).
const UNRESOLVABLE: &str = "http://agent-host-cannot-resolve.invalid/";

/// A session whose WebDriver is unreachable, so it can never start.
fn offline_session() -> Arc<BrowserSession> {
    Arc::new(BrowserSession::new(BrowserConfig::new().webdriver_url("http://127.0.0.1:1")))
}

#[tokio::test]
async fn an_unresolvable_host_is_refused_before_touching_the_browser() {
    let session = offline_session();

    for tool in [
        Arc::new(NavigateTool::new(session.clone())) as Arc<dyn Tool>,
        Arc::new(NewTabTool::new(session.clone())),
    ] {
        let error = run(tool.as_ref(), json!({ "url": UNRESOLVABLE })).await.unwrap_err();
        assert!(error.to_string().contains("does not resolve"), "{}: {error}", tool.name());
    }
    assert!(!session.is_active().await, "a refused URL must not start the browser");
}

#[tokio::test]
async fn unresolved_hosts_are_an_explicit_opt_in() {
    let tool = NavigateTool::new(offline_session()).with_unresolved_hosts(true);
    let error = run(&tool, json!({ "url": UNRESOLVABLE })).await.unwrap_err().to_string();
    assert!(!error.contains("does not resolve"), "the opt-in was ignored: {error}");

    let toolset = BrowserToolset::new(offline_session()).with_unresolved_hosts(true);
    for name in ["browser_navigate", "browser_new_tab", "browser_new_window"] {
        let tool = toolset
            .all_tools()
            .into_iter()
            .find(|tool| tool.name() == name)
            .expect("the full toolset must expose the tool");
        let error = run(tool.as_ref(), json!({ "url": UNRESOLVABLE })).await.unwrap_err();
        assert!(!error.to_string().contains("does not resolve"), "{name} ignored the opt-in");
    }
}
