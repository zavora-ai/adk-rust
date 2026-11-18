//! Live: Claude drives a browser through the governed tool path.
//!
//! The browser is the recorded WebDriver server in `adk-browser/tests/support/webdriver.rs`,
//! or a real one when `ADK_WEBDRIVER_URL` names it (for example ChromeDriver on
//! `http://localhost:9515`). Requires `ANTHROPIC_API_KEY`.

#[path = "../../adk-browser/tests/support/webdriver.rs"]
mod webdriver;

use adk_agent::LlmAgentBuilder;
use adk_browser::{BrowserConfig, BrowserSession, BrowserUseToolset, WebDriverComputer};
use adk_computer_use::ComputerUseToolset;
use adk_core::{
    Content, DeclarativePolicy, Event, Part, Result, RunBudget, RunConfig, SessionId,
    ToolConfirmationDecision, ToolConfirmationHandler, ToolConfirmationRequest, UserId,
};
use adk_model::anthropic::{AnthropicClient, AnthropicConfig, Effort};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use webdriver::{HOME, MORE, WebDriverFixture};

/// Approves navigation to the example domains only, and records every request it decides.
#[derive(Debug, Default)]
struct ExampleDomainsOnly {
    decided: Mutex<Vec<String>>,
}

#[async_trait]
impl ToolConfirmationHandler for ExampleDomainsOnly {
    async fn decide(&self, request: &ToolConfirmationRequest) -> Result<ToolConfirmationDecision> {
        let url = request.args["url"].as_str().unwrap_or_default().to_string();
        self.decided.lock().unwrap().push(format!("{} {url}", request.tool_name));
        let allowed = ["example.com", "www.iana.org"].iter().any(|host| url.contains(host));
        Ok(if allowed { ToolConfirmationDecision::Approve } else { ToolConfirmationDecision::Deny })
    }
}

fn browser(fixture: &WebDriverFixture) -> Arc<BrowserSession> {
    let url = std::env::var("ADK_WEBDRIVER_URL").unwrap_or_else(|_| fixture.url());
    Arc::new(BrowserSession::new(
        BrowserConfig::new().webdriver_url(url).headless(true).viewport(1280, 800),
    ))
}

async fn run(agent: adk_agent::LlmAgent, config: RunConfig, prompt: &str) -> Vec<Event> {
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "live".into(),
            user_id: "user".into(),
            session_id: Some("session".into()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let runner = Runner::builder()
        .app_name("live")
        .agent(Arc::new(agent))
        .session_service(sessions as Arc<dyn SessionService>)
        .run_config(config)
        .build()
        .unwrap();
    let stream = runner
        .run(
            UserId::new("user").unwrap(),
            SessionId::new("session").unwrap(),
            Content::new("user").with_text(prompt),
        )
        .await
        .unwrap();
    let events: Vec<Result<Event>> =
        tokio::time::timeout(Duration::from_secs(240), stream.collect())
            .await
            .expect("the run finishes");
    events.into_iter().map(|event| event.expect("no run error")).collect()
}

/// Every tool call and answer in `events`, and the final text.
fn transcript(events: &[Event]) -> (Vec<String>, String) {
    let mut steps = Vec::new();
    let mut text = String::new();
    for event in events.iter().filter(|event| !event.llm_response.partial) {
        for part in event.llm_response.content.iter().flat_map(|content| &content.parts) {
            match part {
                Part::FunctionCall { name, args, .. } => steps.push(format!("call {name} {args}")),
                Part::FunctionResponse { function_response, .. } => {
                    let answer = function_response.response.to_string();
                    steps.push(format!(
                        "answer {} {:?} {}",
                        function_response.name,
                        part.tool_result_status(),
                        answer.chars().take(160).collect::<String>()
                    ));
                }
                Part::Text { text: chunk } => text.push_str(chunk),
                _ => {}
            }
        }
    }
    (steps, text)
}

fn model(model: &str) -> Arc<AnthropicClient> {
    let key = std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY");
    Arc::new(
        AnthropicClient::new(AnthropicConfig::new(key, model).with_effort(Effort::Low)).unwrap(),
    )
}

#[tokio::test]
#[ignore = "calls the Anthropic API; requires ANTHROPIC_API_KEY"]
async fn live_browser_use_through_the_governed_path() {
    if std::env::var("ANTHROPIC_API_KEY").is_err() {
        return;
    }
    let fixture = WebDriverFixture::start();
    let toolset = BrowserUseToolset::new(browser(&fixture)).with_unresolved_hosts(true);
    let agent = LlmAgentBuilder::new("researcher")
        .instruction("Use the browser to answer. Read the page before you click, and answer in one sentence.")
        .model(model("claude-haiku-5-5"))
        .toolset(Arc::new(toolset))
        .build()
        .unwrap();
    // Navigation needs a decision; reading and clicking are allowed; typing is denied.
    let approvals = Arc::new(ExampleDomainsOnly::default());
    let policy = DeclarativePolicy::builder()
        .require_approval("browser.navigate", "opening a site needs a decision")
        .deny("browser.type", "this agent only reads")
        .allow("browser.*")
        .build();
    let config = RunConfig::builder()
        .tool_policy(Arc::new(policy))
        .tool_confirmation_handler(approvals.clone())
        .budget(RunBudget::new().max_tool_calls(20))
        .build();
    let events = run(
        agent,
        config,
        "Open example.com, follow its 'More information' link, and tell me which RFC numbers \
         that page mentions.",
    )
    .await;
    let (steps, answer) = transcript(&events);
    println!("approvals: {:?}", approvals.decided.lock().unwrap());
    for step in &steps {
        println!("{step}");
    }
    println!("browser: {:?}", fixture.log());
    println!("answer: {answer}");
    assert!(
        approvals
            .decided
            .lock()
            .unwrap()
            .iter()
            .any(|decision| decision.starts_with("browser.navigate"))
    );
    assert!(steps.iter().any(|step| step.starts_with("call browser.")), "{steps:?}");
    if std::env::var("ADK_WEBDRIVER_URL").is_err() {
        assert_eq!(fixture.current_url(), MORE, "the link was followed");
    }
    assert!(answer.contains("2606"), "{answer}");
}

#[tokio::test]
#[ignore = "calls the Anthropic API; requires ANTHROPIC_API_KEY"]
async fn live_computer_use_on_a_browser_viewport() {
    if std::env::var("ANTHROPIC_API_KEY").is_err() {
        return;
    }
    let fixture = WebDriverFixture::start();
    let session = browser(&fixture);
    session.navigate(HOME).await.unwrap();
    let toolset = ComputerUseToolset::new(Arc::new(WebDriverComputer::new(session)));
    let agent = LlmAgentBuilder::new("operator")
        .instruction("You operate a computer through screenshots and clicks. Be brief.")
        .model(model("claude-sonnet-5-5"))
        .toolset(Arc::new(toolset))
        .build()
        .unwrap();
    // Looking is free; every action that changes the screen needs a decision.
    let approvals = Arc::new(ApproveAll::default());
    let policy = DeclarativePolicy::builder()
        .allow("computer.screenshot")
        .allow("computer.zoom")
        .allow("computer.cursor_position")
        .require_approval("computer.*", "acting on the screen needs a decision")
        .build();
    let config = RunConfig::builder()
        .tool_policy(Arc::new(policy))
        .tool_confirmation_handler(approvals.clone())
        .budget(RunBudget::new().max_tool_calls(12))
        .build();
    let events = run(
        agent,
        config,
        "Take a screenshot. Then click the middle of the blue bar, take another screenshot, and \
         tell me in one sentence what changed.",
    )
    .await;
    let (steps, answer) = transcript(&events);
    println!("approvals: {:?}", approvals.decided.lock().unwrap());
    for step in &steps {
        println!("{step}");
    }
    println!("browser clicks: {:?}", fixture.clicks());
    println!("answer: {answer}");
    assert!(steps.iter().any(|step| step.starts_with("call computer.screenshot")), "{steps:?}");
    assert!(approvals.decided.lock().unwrap().iter().any(|decision| decision.contains("click")));
    if std::env::var("ADK_WEBDRIVER_URL").is_err() {
        assert_eq!(fixture.clicks(), vec!["ref_2".to_string()], "the blue link was clicked");
        assert_eq!(fixture.current_url(), MORE);
    }
}

#[derive(Debug, Default)]
struct ApproveAll {
    decided: Mutex<Vec<String>>,
}

#[async_trait]
impl ToolConfirmationHandler for ApproveAll {
    async fn decide(&self, request: &ToolConfirmationRequest) -> Result<ToolConfirmationDecision> {
        self.decided.lock().unwrap().push(format!("{} {}", request.tool_name, request.args));
        Ok(ToolConfirmationDecision::Approve)
    }
}
