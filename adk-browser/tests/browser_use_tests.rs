//! Claude's browser use toolset over WebDriver, against a recorded WebDriver server.

#[path = "support/webdriver.rs"]
mod webdriver;

use adk_browser::{BrowserConfig, BrowserSession, BrowserUseToolset, WebDriverComputer};
use adk_core::egress::ToolEgress;
use adk_core::{
    ActuationOutput, CallbackContext, ComputerAction, ComputerActuator, Content, EventActions,
    FunctionResponseData, MemoryEntry, MouseButton, ReadonlyContext, Result, ScreenPoint, Tool,
    ToolContext, ToolEffect,
};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;
use webdriver::{HOME, MORE, WebDriverFixture};

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

fn session(fixture: &WebDriverFixture) -> Arc<BrowserSession> {
    Arc::new(BrowserSession::new(
        BrowserConfig::new().webdriver_url(fixture.url()).viewport(1280, 800),
    ))
}

fn toolset(fixture: &WebDriverFixture) -> BrowserUseToolset {
    // The fixture serves recorded pages, so no host needs to resolve.
    BrowserUseToolset::new(session(fixture)).with_unresolved_hosts(true)
}

struct Members(Vec<Arc<dyn Tool>>);

impl Members {
    fn get(&self, member: &str) -> Arc<dyn Tool> {
        let name = format!("browser.{member}");
        self.0.iter().find(|tool| tool.name() == name).cloned().unwrap_or_else(|| panic!("{name}"))
    }

    async fn call(&self, member: &str, args: Value) -> Result<Value> {
        let ctx: Arc<dyn ToolContext> = Arc::new(TestCtx { content: Content::new("user") });
        self.get(member).execute(ctx, args).await
    }
}

#[test]
fn members_follow_the_toolset_defaults() {
    let fixture = WebDriverFixture::start();
    let names: Vec<String> =
        toolset(&fixture).member_tools().iter().map(|tool| tool.name().to_string()).collect();
    assert_eq!(names.len(), 27);
    assert!(!names.contains(&"browser.javascript_exec".to_string()));
    assert!(!names.contains(&"browser.file_upload".to_string()));
    assert!(!names.contains(&"browser.read_console".to_string()));

    let opted_in = toolset(&fixture).with_javascript(true).with_file_upload(["/srv/uploads"]);
    assert_eq!(opted_in.members().len(), 29);

    let members = Members(toolset(&fixture).member_tools());
    let navigate = members.get("navigate");
    assert_eq!(
        navigate.declaration()["x-adk-anthropic-toolset-member"],
        json!({ "toolset": "browser", "member": "navigate" })
    );
    assert_eq!(navigate.batch_group().unwrap().halt_text, adk_browser::BROWSER_BATCH_HALT_TEXT);
    assert_eq!(navigate.effect(), ToolEffect::NonIdempotent);
    assert_eq!(members.get("read_page").effect(), ToolEffect::ReadOnly);
    assert_eq!(
        navigate.egress(&json!({ "url": "example.com/docs" })),
        ToolEgress::for_urls(["https://example.com/docs"])
    );
    assert_eq!(members.get("screenshot").egress(&json!({})), ToolEgress::None);
    assert_eq!(members.get("left_click").egress(&json!({})), ToolEgress::Undeclared);
}

#[tokio::test]
async fn a_page_is_navigated_read_and_clicked_by_reference() {
    let fixture = WebDriverFixture::start();
    let members = Members(toolset(&fixture).member_tools());

    let navigated = members.call("navigate", json!({ "url": "example.com" })).await.unwrap();
    assert_eq!(navigated["text"], format!("Navigated to {HOME}"));
    let tabs = &navigated["browser_state"]["tabs"];
    assert_eq!(
        tabs,
        &json!([{ "tab_id": "tab-1", "title": "Example Domain", "url": HOME, "active": true }])
    );

    let tree = members.call("read_page", json!({ "filter": "interactive" })).await.unwrap();
    assert_eq!(
        tree,
        json!(
            "link \"More information...\" href=\"https://www.iana.org/help/example-domains\" [ref_2]\n\
             textbox \"Search\" [ref_3]\nbutton \"Search\" [ref_4]"
        )
    );

    let clicked = members
        .call("left_click", json!({ "target": { "type": "ref", "ref": "ref_2" } }))
        .await
        .unwrap();
    assert_eq!(clicked["text"], "Clicked element ref_2.");
    assert_eq!(clicked["browser_state"]["tabs"][0]["url"], MORE);
    assert_eq!(fixture.clicks(), vec!["ref_2".to_string()]);

    let text = members.call("get_page_text", json!({})).await.unwrap();
    assert!(text.as_str().unwrap().contains("RFC 2606"), "{text}");
}

#[tokio::test]
async fn typing_and_enter_submit_the_search_form() {
    let fixture = WebDriverFixture::start();
    let members = Members(toolset(&fixture).member_tools());
    members.call("navigate", json!({ "url": HOME })).await.unwrap();
    members
        .call("left_click", json!({ "target": { "type": "coordinate", "x": 560, "y": 420 } }))
        .await
        .unwrap();
    members.call("type", json!({ "text": "rfc 2606" })).await.unwrap();
    let pressed = members.call("key", json!({ "text": "Enter" })).await.unwrap();
    assert_eq!(pressed["browser_state"]["tabs"][0]["title"], "Search results");
    assert!(fixture.current_url().ends_with("search?q=rfc+2606"), "{}", fixture.current_url());
}

#[tokio::test]
async fn a_stale_reference_is_an_error_that_asks_for_a_fresh_read() {
    let fixture = WebDriverFixture::start();
    let members = Members(toolset(&fixture).member_tools());
    members.call("navigate", json!({ "url": HOME })).await.unwrap();
    let error = members
        .call("left_click", json!({ "target": { "type": "ref", "ref": "ref_99" } }))
        .await
        .unwrap_err();
    assert!(error.message.contains("ref_99 is stale"), "{error}");
    assert!(fixture.clicks().is_empty());
}

#[tokio::test]
async fn navigation_keeps_the_url_policy() {
    let fixture = WebDriverFixture::start();
    let members = Members(toolset(&fixture).member_tools());
    for url in [
        "file:///etc/passwd",
        "javascript:alert(1)",
        "http://169.254.169.254/latest",
        "http://localhost:8080/",
    ] {
        let error = members.call("navigate", json!({ "url": url })).await.unwrap_err();
        assert!(error.message.starts_with("Error: navigation refused."), "{url}: {error}");
    }
    assert!(!fixture.log().iter().any(|line| line.starts_with("POST /url")), "{:?}", fixture.log());
}

#[tokio::test]
async fn tab_management_reports_browser_state() {
    let fixture = WebDriverFixture::start();
    let members = Members(toolset(&fixture).member_tools());
    members.call("navigate", json!({ "url": HOME })).await.unwrap();
    let opened = members.call("new_tab", json!({})).await.unwrap();
    assert_eq!(
        opened,
        json!({ "browser_state": {
            "tabs": [
                { "tab_id": "tab-1", "title": "Example Domain", "url": HOME },
                { "tab_id": "tab-2", "title": "", "url": "about:blank", "active": true }
            ],
            "state_changes": [{ "type": "tab_opened", "tab_id": "tab-2" }]
        } })
    );
    let switched = members.call("switch_tab", json!({ "tab_id": "tab-1" })).await.unwrap();
    assert_eq!(switched["browser_state"]["tabs"][0]["active"], true);
    let closed = members.call("close_tab", json!({ "tab_id": "tab-2" })).await.unwrap();
    assert_eq!(closed["browser_state"]["tabs"].as_array().unwrap().len(), 1);
    let unknown = members.call("switch_tab", json!({ "tab_id": "tab-9" })).await.unwrap_err();
    assert!(unknown.message.contains("tab-9"), "{unknown}");
}

#[tokio::test]
async fn screenshots_come_back_as_inline_images() {
    let fixture = WebDriverFixture::start();
    let members = Members(toolset(&fixture).member_tools());
    members.call("navigate", json!({ "url": HOME })).await.unwrap();
    let shot = members.call("screenshot", json!({})).await.unwrap();
    let response = FunctionResponseData::from_tool_result("browser.screenshot", shot);
    assert_eq!(response.response, json!({ "text": "Screenshot captured." }));
    assert_eq!(response.inline_data[0].mime_type, "image/png");
    let zoomed = members.call("zoom", json!({ "region": [500, 280, 780, 320] })).await.unwrap();
    let zoomed = FunctionResponseData::from_tool_result("browser.zoom", zoomed);
    let png = &zoomed.inline_data[0].data;
    let width = u32::from_be_bytes([png[16], png[17], png[18], png[19]]);
    assert_eq!(width, 1280, "the zoomed region is scaled up to the screenshot width");
}

#[tokio::test]
async fn the_webdriver_computer_acts_in_screenshot_pixels() {
    let fixture = WebDriverFixture::start();
    let computer = WebDriverComputer::new(session(&fixture));
    assert_eq!(computer.display_size(), Some((1280, 800)));
    computer.session().navigate(HOME).await.unwrap();
    let ActuationOutput::Image(shot) = computer.perform(ComputerAction::Screenshot).await.unwrap()
    else {
        panic!("a screenshot");
    };
    assert_eq!((shot.width, shot.height), (1280, 800));
    computer
        .perform(ComputerAction::Click {
            button: MouseButton::Left,
            count: 1,
            at: Some(ScreenPoint::new(640, 300)),
            modifiers: Vec::new(),
        })
        .await
        .unwrap();
    assert_eq!(fixture.clicks(), vec!["ref_2".to_string()]);
    assert_eq!(fixture.current_url(), MORE);
    let outside = computer
        .perform(ComputerAction::MoveTo { at: ScreenPoint::new(5000, 10) })
        .await
        .unwrap_err();
    assert!(outside.message.contains("outside the 1280x800 viewport"), "{outside}");
    assert_eq!(
        computer.perform(ComputerAction::CursorPosition).await.unwrap(),
        ActuationOutput::Cursor(ScreenPoint::new(640, 300))
    );
}
