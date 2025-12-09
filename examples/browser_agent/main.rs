//! Browser Agent Example (Gemini)
//!
//! Demonstrates a web scraping agent that can navigate websites,
//! extract information, and answer questions about web content.
//!
//! ## Requirements
//!
//! 1. WebDriver running: `docker run -d -p 4444:4444 selenium/standalone-chrome`
//! 2. GOOGLE_API_KEY environment variable set
//!
//! ## Running
//!
//! ```bash
//! cargo run --example browser_agent --features browser
//! ```

use adk_agent::LlmAgentBuilder;
use adk_browser::{BrowserConfig, BrowserSession, BrowserToolset};
use adk_core::{Agent, Content, InvocationContext, Part, RunConfig, Session, State};
use adk_model::GeminiModel;
use async_trait::async_trait;
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;

// Simple session implementation
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
        "browser-agent-session"
    }
    fn app_name(&self) -> &str {
        "browser_agent"
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
        "browser_agent"
    }
    fn session_id(&self) -> &str {
        "browser-agent-session"
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

    while let Some(result) = stream.next().await {
        match result {
            Ok(event) => {
                if let Some(content) = &event.llm_response.content {
                    for part in &content.parts {
                        if let Part::Text { text } = part {
                            response.push_str(text);
                        }
                    }
                }
            }
            Err(e) => {
                return Err(format!("Agent error: {}", e).into());
            }
        }
    }

    Ok(response)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Browser Agent Example (Gemini) ===\n");

    // Load API key
    let _ = dotenvy::dotenv();
    let api_key = match std::env::var("GOOGLE_API_KEY") {
        Ok(key) => key,
        Err(_) => {
            println!("GOOGLE_API_KEY not set");
            println!("export GOOGLE_API_KEY=your_key");
            return Ok(());
        }
    };

    // Check WebDriver
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

    println!("WebDriver: {}", webdriver_url);

    // Setup browser
    let config =
        BrowserConfig::new().webdriver_url(&webdriver_url).headless(true).viewport(1920, 1080);

    let browser = Arc::new(BrowserSession::new(config));
    browser.start().await?;
    println!("Browser session started\n");

    // Create toolset with essential tools only (faster for demos)
    let toolset = BrowserToolset::new(browser.clone())
        .with_cookies(false)
        .with_windows(false)
        .with_frames(false)
        .with_actions(false);

    let tools = toolset.all_tools();
    println!("Browser tools loaded: {}\n", tools.len());

    // Create model
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash")?);

    // Create agent
    let mut builder = LlmAgentBuilder::new("web_researcher")
        .model(model)
        .description("A web research assistant that can browse websites and extract information")
        .instruction(r#"You are a helpful web research assistant. You can:
- Navigate to websites using browser_navigate
- Extract text content using browser_extract_text
- Get page information using browser_page_info
- Find links using browser_extract_links
- Take screenshots using browser_screenshot
- Wait for elements using browser_wait_for_element

When given a research task:
1. Navigate to the relevant website
2. Wait for the page to load if needed
3. Extract the requested information
4. Provide a clear, concise answer

Always use the tools available to you. Don't make assumptions about web content - actually browse and verify."#);

    for tool in tools {
        builder = builder.tool(tool);
    }

    let agent = Arc::new(builder.build()?);
    println!("Agent created: {}\n", agent.name());

    // =========================================================================
    // Task 1: Simple page info
    // =========================================================================
    println!("--- Task 1: Get website info ---");
    println!("Task: Navigate to example.com and tell me the page title and main heading\n");

    let response = run_agent(
        agent.clone(),
        "Navigate to https://example.com and tell me the page title and main heading text.",
    )
    .await?;

    println!("Response:\n{}\n", response);

    // =========================================================================
    // Task 2: Extract links
    // =========================================================================
    println!("--- Task 2: Extract links ---");
    println!("Task: Find all links on example.com\n");

    let response = run_agent(
        agent.clone(),
        "What links are available on example.com? Navigate there and extract all links.",
    )
    .await?;

    println!("Response:\n{}\n", response);

    // =========================================================================
    // Task 3: Research task
    // =========================================================================
    println!("--- Task 3: Research task ---");
    println!("Task: Check if httpbin.org is working\n");

    let response = run_agent(
        agent.clone(),
        "Navigate to https://yahoo.com and tell me what this website is about based on the page content."
    ).await?;

    println!("Response:\n{}\n", response);

    // Cleanup
    browser.stop().await?;
    println!("Browser session stopped");
    println!("\n=== Example Complete ===");

    Ok(())
}
