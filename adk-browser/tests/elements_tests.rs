//! `browser_elements` (spec 3.7 X16): interactive elements with rectangles in CSS pixels
//! and a selector that matches only them, so a chosen set-of-marks number becomes a
//! `browser_click` by selector with no coordinate click.
//!
//! The first test runs against the recorded WebDriver server. The second drives a real
//! browser through the WebDriver server at `ADK_WEBDRIVER_URL` (default
//! `http://localhost:9515`, chromedriver's port) on a local fixture page with two
//! identical "Save" buttons, and is skipped with a message when no server answers.

#[path = "support/webdriver.rs"]
mod webdriver;

use adk_browser::{BrowserConfig, BrowserSession, ClickTool, ElementsTool};
use adk_core::{
    CallbackContext, Content, EventActions, MemoryEntry, ReadonlyContext, Result, Tool,
    ToolContext, ToolEffect,
};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;
use webdriver::{HOME, WebDriverFixture};

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

fn ctx() -> Arc<dyn ToolContext> {
    Arc::new(TestCtx { content: Content::new("user") })
}

#[tokio::test]
async fn elements_come_back_with_css_rectangles_and_selectors() {
    let fixture = WebDriverFixture::start();
    let browser = Arc::new(BrowserSession::new(
        BrowserConfig::new().webdriver_url(fixture.url()).viewport(1280, 800),
    ));
    browser.navigate(HOME).await.unwrap();
    let tool = ElementsTool::new(browser.clone());
    assert!(tool.is_read_only());
    assert_eq!(tool.effect(), ToolEffect::ReadOnly);

    let answer = tool.execute(ctx(), json!({ "limit": 50 })).await.unwrap();
    assert_eq!(answer["devicePixelRatio"], json!(1));
    let elements = answer["elements"].as_array().unwrap();
    let names: Vec<&str> = elements.iter().map(|e| e["name"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["More information...", "Search", "Search"]);
    assert_eq!(elements[2]["rect"], json!({ "x": 680, "y": 400, "width": 160, "height": 40 }));
    let selectors: Vec<&str> = elements.iter().map(|e| e["selector"].as_str().unwrap()).collect();
    assert_eq!(selectors.len(), 3);
    assert!(selectors.iter().all(|s| s.starts_with("[data-ref=")));
    // The script carried its marker and the limit.
    let script = fixture.log().into_iter().find(|line| line.contains("adk-browser:elements"));
    assert!(script.is_some(), "the elements script reached the browser");
    browser.stop().await.ok();
}

const PAGE: &str = r#"<!doctype html>
<html><head><title>Customer form</title><style>
  body { margin: 0; font: 16px sans-serif; }
  button, input { position: absolute; box-sizing: border-box; margin: 0; }
</style></head><body>
  <button id="toolbar-save" style="left: 1310px; top: 10px; width: 90px; height: 40px">Save</button>
  <input name="customer" aria-label="Name" style="left: 200px; top: 150px; width: 450px; height: 40px">
  <button style="left: 830px; top: 590px; width: 110px; height: 40px">Cancel</button>
  <button style="left: 960px; top: 590px; width: 90px; height: 40px">Save</button>
  <button style="left: 10px; top: 900px; width: 60px; height: 30px; visibility: hidden">Hidden</button>
  <script>
    document.addEventListener('click', (e) => {
      const b = e.target.closest('button');
      if (b) document.title = 'clicked ' + (b.id || b.style.left);
    });
  </script>
</body></html>"#;

/// Serves the fixture page on a loopback port until the test ends.
fn serve_page() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
                line.clear();
            }
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{PAGE}",
                PAGE.len()
            );
            stream.write_all(response.as_bytes()).ok();
        }
    });
    format!("http://{address}/form.html")
}

async fn webdriver_url() -> Option<String> {
    let url = std::env::var("ADK_WEBDRIVER_URL").unwrap_or_else(|_| "http://localhost:9515".into());
    let authority = url.strip_prefix("http://")?.trim_end_matches('/').to_string();
    let probe = authority.clone();
    let ready = tokio::task::spawn_blocking(move || {
        use std::io::Read;
        use std::net::ToSocketAddrs;
        let address = probe.to_socket_addrs().ok()?.next()?;
        let mut stream =
            std::net::TcpStream::connect_timeout(&address, Duration::from_secs(2)).ok()?;
        stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
        let request = format!("GET /status HTTP/1.1\r\nHost: {probe}\r\nConnection: close\r\n\r\n");
        stream.write_all(request.as_bytes()).ok()?;
        let mut response = String::new();
        stream.read_to_string(&mut response).ok()?;
        let body: Value = serde_json::from_str(response.split("\r\n\r\n").nth(1)?).ok()?;
        Some(body["value"]["ready"].as_bool().unwrap_or(true))
    })
    .await
    .ok()??;
    ready.then_some(url)
}

/// A real browser: the fixture page at device pixel ratio 2, two identical "Save" buttons.
/// Each element's rectangle is its CSS layout, each selector matches only it, and the
/// dialog's Save — the chosen mark — is clicked by its selector.
#[tokio::test]
async fn a_real_browser_returns_layout_rectangles_and_unique_selectors() {
    let Some(webdriver) = webdriver_url().await else {
        println!(
            "skipped: no WebDriver server answers at ADK_WEBDRIVER_URL or http://localhost:9515; \
             start chromedriver (Chrome) to run this test against a real browser"
        );
        return;
    };
    let page = serve_page();
    let browser = Arc::new(BrowserSession::new(
        BrowserConfig::new()
            .webdriver_url(webdriver)
            .viewport(1512, 982)
            .add_arg("--force-device-scale-factor=2"),
    ));
    browser.navigate(&page).await.unwrap();
    let answer = ElementsTool::new(browser.clone()).execute(ctx(), json!({})).await.unwrap();
    println!("browser_elements: {answer:#}");

    assert_eq!(answer["devicePixelRatio"].as_f64(), Some(2.0));
    let elements = answer["elements"].as_array().unwrap();
    let by_name = |name: &str| -> Vec<&Value> {
        elements.iter().filter(|e| e["name"].as_str() == Some(name)).collect()
    };
    assert!(by_name("Hidden").is_empty(), "hidden elements are not candidates");
    let saves = by_name("Save");
    assert_eq!(saves.len(), 2, "both Save buttons are candidates");
    assert_eq!(saves[0]["selector"], json!("#toolbar-save"));
    assert_eq!(saves[0]["rect"], json!({ "x": 1310.0, "y": 10.0, "width": 90.0, "height": 40.0 }));
    let dialog = saves[1];
    assert_eq!(dialog["rect"], json!({ "x": 960.0, "y": 590.0, "width": 90.0, "height": 40.0 }));
    assert_eq!(by_name("Name")[0]["selector"], json!("input[name=\"customer\"]"));
    assert_eq!(by_name("Name")[0]["role"], json!("textbox"));

    // Every selector matches exactly one element on the page.
    for element in elements {
        let selector = element["selector"].as_str().unwrap();
        let script = format!(
            "return document.querySelectorAll('{}').length;",
            adk_browser::escape_js_string(selector)
        );
        assert_eq!(browser.execute_script(&script).await.unwrap(), json!(1), "{selector}");
    }

    // The chosen mark becomes a browser_click by selector.
    let selector = dialog["selector"].as_str().unwrap();
    ClickTool::new(browser.clone())
        .execute(ctx(), json!({ "selector": selector, "wait_timeout": 5 }))
        .await
        .unwrap();
    let title = browser.execute_script("return document.title;").await.unwrap();
    assert_eq!(title, json!("clicked 960px"), "the dialog's Save was clicked, not the toolbar's");
    browser.stop().await.ok();
}
