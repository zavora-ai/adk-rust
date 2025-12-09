//! Interactive Browser Agent Example
//!
//! Demonstrates a browser agent that can interact with forms, buttons,
//! and dynamic web content using Gemini.
//!
//! ## Requirements
//!
//! 1. WebDriver running: `docker run -d -p 4444:4444 selenium/standalone-chrome`
//! 2. GOOGLE_API_KEY environment variable set
//!
//! ## Running
//!
//! ```bash
//! cargo run --example browser_interactive --features browser
//! ```

use adk_agent::LlmAgentBuilder;
use adk_browser::{BrowserConfig, BrowserSession, BrowserToolset};
use adk_core::{Agent, Content, InvocationContext, Part, RunConfig, Session, State};
use adk_model::GeminiModel;
use async_trait::async_trait;
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;

struct SimpleState {
    data: std::sync::Mutex<HashMap<String, serde_json::Value>>,
}

impl SimpleState {
    fn new() -> Self {
        Self { data: std::sync::Mutex::new(HashMap::new()) }
    }
}

impl State for SimpleState {
    fn get(&self, key: &str) -> Option<serde_json::Value> {
        self.data.lock().unwrap().get(key).cloned()
    }
    fn set(&mut self, key: String, value: serde_json::Value) {
        self.data.lock().unwrap().insert(key, value);
    }
    fn all(&self) -> HashMap<String, serde_json::Value> {
        self.data.lock().unwrap().clone()
    }
}

struct SimpleSession {
    state: SimpleState,
}

impl Session for SimpleSession {
    fn id(&self) -> &str {
        "interactive-session"
    }
    fn app_name(&self) -> &str {
        "browser_interactive"
    }
    fn user_id(&self) -> &str {
        "user"
    }
    fn state(&self) -> &dyn State {
        &self.state
    }
    fn conversation_history(&self) -> Vec<Content> {
        Vec::new()
    }
}

struct AgentContext {
    agent: Arc<dyn Agent>,
    content: Content,
    config: RunConfig,
    session: SimpleSession,
}

#[async_trait]
impl adk_core::ReadonlyContext for AgentContext {
    fn invocation_id(&self) -> &str {
        "inv-1"
    }
    fn agent_name(&self) -> &str {
        self.agent.name()
    }
    fn user_id(&self) -> &str {
        "user"
    }
    fn app_name(&self) -> &str {
        "browser_interactive"
    }
    fn session_id(&self) -> &str {
        "interactive-session"
    }
    fn branch(&self) -> &str {
        ""
    }
    fn user_content(&self) -> &Content {
        &self.content
    }
}

#[async_trait]
impl adk_core::CallbackContext for AgentContext {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}

#[async_trait]
impl InvocationContext for AgentContext {
    fn agent(&self) -> Arc<dyn Agent> {
        self.agent.clone()
    }
    fn memory(&self) -> Option<Arc<dyn adk_core::Memory>> {
        None
    }
    fn session(&self) -> &dyn Session {
        &self.session
    }
    fn run_config(&self) -> &RunConfig {
        &self.config
    }
    fn end_invocation(&self) {}
    fn ended(&self) -> bool {
        false
    }
}

async fn run_agent(
    agent: Arc<dyn Agent>,
    task: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    println!("  Executing task...");

    let content =
        Content { role: "user".to_string(), parts: vec![Part::Text { text: task.to_string() }] };

    let ctx = Arc::new(AgentContext {
        agent: agent.clone(),
        content,
        config: RunConfig::default(),
        session: SimpleSession { state: SimpleState::new() },
    });

    let mut stream = agent.run(ctx).await?;
    let mut response = String::new();
    let mut tool_calls = 0;

    while let Some(result) = stream.next().await {
        match result {
            Ok(event) => {
                // Count events (approximates tool usage)
                tool_calls += 1;

                if let Some(content) = &event.llm_response.content {
                    for part in &content.parts {
                        if let Part::Text { text } = part {
                            response.push_str(text);
                        }
                    }
                }
            }
            Err(e) => return Err(format!("Agent error: {}", e).into()),
        }
    }

    println!("  Events processed: {}", tool_calls);
    Ok(response)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Interactive Browser Agent Example ===\n");

    let _ = dotenvy::dotenv();
    let api_key = match std::env::var("GOOGLE_API_KEY") {
        Ok(key) => key,
        Err(_) => {
            println!("GOOGLE_API_KEY not set");
            return Ok(());
        }
    };

    let webdriver_url =
        std::env::var("WEBDRIVER_URL").unwrap_or_else(|_| "http://localhost:4444".to_string());

    let available = reqwest::Client::new()
        .get(format!("{}/status", webdriver_url))
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
        .is_ok();

    if !available {
        println!("WebDriver not available at {}", webdriver_url);
        println!("Start with: docker run -d -p 4444:4444 selenium/standalone-chrome");
        return Ok(());
    }

    println!("WebDriver: {}\n", webdriver_url);

    // Setup browser with full toolset
    let config =
        BrowserConfig::new().webdriver_url(&webdriver_url).headless(true).viewport(1920, 1080);

    let browser = Arc::new(BrowserSession::new(config));
    browser.start().await?;
    println!("Browser started with full toolset\n");

    // Full toolset for interactive tasks
    let toolset = BrowserToolset::new(browser.clone());
    let tools = toolset.all_tools();
    println!("All {} browser tools loaded\n", tools.len());

    // Create model
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash")?);

    // Create agent with comprehensive instructions
    let mut builder = LlmAgentBuilder::new("interactive_browser")
        .model(model)
        .description("An interactive web automation agent")
        .instruction(r#"You are an expert web automation agent. You have access to comprehensive browser tools:

NAVIGATION:
- browser_navigate: Go to a URL
- browser_back, browser_forward, browser_refresh: History navigation

INTERACTION:
- browser_click: Click on elements (use CSS selectors like '#id', '.class', 'button')
- browser_type: Type text into inputs
- browser_clear: Clear input fields
- browser_select: Select dropdown options
- browser_hover: Hover over elements

EXTRACTION:
- browser_extract_text: Get text content
- browser_extract_attribute: Get attributes like href, src
- browser_extract_links: Get all links
- browser_page_info: Get title, URL
- browser_page_source: Get HTML

WAITING:
- browser_wait_for_element: Wait for element to appear
- browser_wait_for_text: Wait for text to appear
- browser_wait_for_page_load: Wait for page load

ADVANCED:
- browser_evaluate_js: Execute JavaScript
- browser_screenshot: Capture page
- browser_element_state: Check if element is visible/enabled
- browser_press_key: Press keyboard keys (Enter, Tab, etc.)

When automating:
1. Always wait for elements before interacting
2. Use specific CSS selectors
3. Verify actions succeeded
4. Report what you observe"#);

    for tool in tools {
        builder = builder.tool(tool);
    }

    let agent = Arc::new(builder.build()?);

    // =========================================================================
    // Task 1: Navigate and inspect
    // =========================================================================
    println!("--- Task 1: Navigate and inspect elements ---");
    println!("Task: Go to httpbin.org/forms/post and describe the form fields\n");

    let response = run_agent(
        agent.clone(),
        "Navigate to https://httpbin.org/forms/post and tell me what form fields are available. Use browser_extract_text or browser_page_source to see the form structure."
    ).await?;

    println!("\nResponse:\n{}\n", response);

    // =========================================================================
    // Task 2: JavaScript execution
    // =========================================================================
    println!("--- Task 2: Execute JavaScript ---");
    println!("Task: Get document information using JavaScript\n");

    let response = run_agent(
        agent.clone(),
        "Navigate to example.com and use browser_evaluate_js to get: 1) the document title, 2) number of paragraphs, 3) the window dimensions. Return the results."
    ).await?;

    println!("\nResponse:\n{}\n", response);

    // =========================================================================
    // Task 3: Element state checking
    // =========================================================================
    println!("--- Task 3: Check element states ---");
    println!("Task: Verify element visibility and state\n");

    let response = run_agent(
        agent.clone(),
        "Go to example.com and check the state of the h1 element and the link element. Are they displayed? Are they clickable? Use browser_element_state."
    ).await?;

    println!("\nResponse:\n{}\n", response);

    // =========================================================================
    // Task 4: Take and describe screenshot
    // =========================================================================
    println!("--- Task 4: Screenshot analysis ---");
    println!("Task: Take a screenshot and describe the page layout\n");

    let response = run_agent(
        agent.clone(),
        "Navigate to example.com, take a screenshot using browser_screenshot, and describe the page layout based on the elements you can extract (heading, paragraphs, links)."
    ).await?;

    println!("\nResponse:\n{}\n", response);

    // Cleanup
    browser.stop().await?;
    println!("Browser stopped");
    println!("\n=== Example Complete ===");

    Ok(())
}
