//! Browser Automation Basic Example
//!
//! This example demonstrates using adk-browser tools with an LLM agent
//! for web automation tasks.
//!
//! ## Requirements
//!
//! Before running this example, you need:
//! 1. A WebDriver server running (ChromeDriver, geckodriver, or Selenium)
//! 2. GOOGLE_API_KEY environment variable set
//!
//! ### Starting ChromeDriver
//!
//! ```bash
//! # Install (macOS)
//! brew install chromedriver
//!
//! # Start ChromeDriver
//! chromedriver --port=4444
//! ```
//!
//! ### Using Docker (alternative)
//!
//! ```bash
//! docker run -d -p 4444:4444 selenium/standalone-chrome
//! ```
//!
//! ## Running the Example
//!
//! ```bash
//! # Set API key
//! export GOOGLE_API_KEY=your_api_key
//!
//! # Run example
//! cargo run --example browser_basic
//! ```

use adk_agent::LlmAgentBuilder;
use adk_browser::{BrowserConfig, BrowserSession, BrowserToolset};
use adk_core::{Agent, Content, InvocationContext, Part, RunConfig, Session, State};
use adk_model::GeminiModel;
use async_trait::async_trait;
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;

// Simple mock session for demonstration
struct MockState {
    data: std::sync::Mutex<HashMap<String, serde_json::Value>>,
}

impl MockState {
    fn new() -> Self {
        Self { data: std::sync::Mutex::new(HashMap::new()) }
    }
}

impl State for MockState {
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

struct MockSession {
    state: MockState,
}

impl MockSession {
    fn new() -> Self {
        Self { state: MockState::new() }
    }
}

impl Session for MockSession {
    fn id(&self) -> &str {
        "browser-session"
    }

    fn app_name(&self) -> &str {
        "browser_example"
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

// Mock invocation context
struct MockContext {
    agent: Arc<dyn Agent>,
    content: Content,
    config: RunConfig,
    session: MockSession,
}

#[async_trait]
impl adk_core::ReadonlyContext for MockContext {
    fn invocation_id(&self) -> &str {
        "inv-browser-1"
    }

    fn agent_name(&self) -> &str {
        self.agent.name()
    }

    fn user_id(&self) -> &str {
        "user"
    }

    fn app_name(&self) -> &str {
        "browser_example"
    }

    fn session_id(&self) -> &str {
        "browser-session"
    }

    fn branch(&self) -> &str {
        ""
    }

    fn user_content(&self) -> &Content {
        &self.content
    }
}

#[async_trait]
impl adk_core::CallbackContext for MockContext {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}

#[async_trait]
impl InvocationContext for MockContext {
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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== ADK Browser Automation Example ===\n");

    // Load API key
    let _ = dotenvy::dotenv();
    let api_key = match std::env::var("GOOGLE_API_KEY") {
        Ok(key) => key,
        Err(_) => {
            println!("GOOGLE_API_KEY not set");
            println!("\nTo run this example:");
            println!("  export GOOGLE_API_KEY=your_api_key");
            println!("  cargo run --example browser_basic");
            return Ok(());
        }
    };

    // -------------------------------------------------------------------------
    // 1. Check WebDriver availability
    // -------------------------------------------------------------------------
    println!("1. Checking WebDriver availability...\n");

    let webdriver_url =
        std::env::var("WEBDRIVER_URL").unwrap_or_else(|_| "http://localhost:4444".to_string());

    // Quick check if WebDriver is running
    let webdriver_available = reqwest::Client::new()
        .get(format!("{}/status", webdriver_url))
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
        .is_ok();

    if !webdriver_available {
        println!("   WebDriver not available at {}", webdriver_url);
        println!("\n   To start WebDriver:");
        println!("   Option 1: chromedriver --port=4444");
        println!("   Option 2: docker run -d -p 4444:4444 selenium/standalone-chrome");
        println!("\n   Continuing with demonstration of tool setup...\n");
    } else {
        println!("   WebDriver available at {}", webdriver_url);
    }

    // -------------------------------------------------------------------------
    // 2. Configure browser session
    // -------------------------------------------------------------------------
    println!("2. Configuring browser session...\n");

    let config = BrowserConfig::new()
        .webdriver_url(&webdriver_url)
        .headless(true)
        .viewport(1920, 1080)
        .page_load_timeout(30);

    println!("   Browser: Chrome (headless)");
    println!("   Viewport: 1920x1080");
    println!("   WebDriver: {}", webdriver_url);

    let browser = Arc::new(BrowserSession::new(config));

    // -------------------------------------------------------------------------
    // 3. Create browser toolset
    // -------------------------------------------------------------------------
    println!("\n3. Creating browser toolset...\n");

    let toolset = BrowserToolset::new(browser.clone());
    let tools = toolset.all_tools();

    println!("   Available tools ({}):", tools.len());
    for tool in &tools {
        println!("   - {} : {}", tool.name(), tool.description());
    }

    // -------------------------------------------------------------------------
    // 4. Create LLM agent with browser tools
    // -------------------------------------------------------------------------
    println!("\n4. Creating browser automation agent...\n");

    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);

    let mut builder = LlmAgentBuilder::new("browser_agent")
        .model(model)
        .description("A web browser automation assistant")
        .instruction(
            r#"You are a helpful web browser automation assistant. You can:
- Navigate to websites
- Click on elements
- Type text into forms
- Extract information from pages
- Take screenshots

When given a task, break it down into steps and use the appropriate browser tools.
Always wait for elements before interacting with them.
If an action fails, explain what went wrong."#,
        );

    // Add all browser tools
    for tool in tools {
        builder = builder.tool(tool);
    }

    let agent = builder.build()?;

    println!("   Agent: {}", agent.name());
    println!("   Description: {}", agent.description());

    // -------------------------------------------------------------------------
    // 5. Demonstrate tool schema (works without WebDriver)
    // -------------------------------------------------------------------------
    println!("\n5. Tool parameter schemas:\n");

    // Show a few tool schemas
    let demo_tools =
        vec!["browser_navigate", "browser_click", "browser_type", "browser_extract_text"];

    let all_tools = BrowserToolset::new(browser.clone()).all_tools();
    for name in demo_tools {
        if let Some(tool) = all_tools.iter().find(|t| t.name() == name)
            && let Some(schema) = tool.parameters_schema()
        {
            println!("   {}:", name);
            let props = schema.get("properties").and_then(|p| p.as_object());
            if let Some(props) = props {
                for (prop_name, prop_value) in props {
                    let desc = prop_value.get("description").and_then(|d| d.as_str()).unwrap_or("");
                    println!("     - {}: {}", prop_name, desc);
                }
            }
            println!();
        }
    }

    // -------------------------------------------------------------------------
    // 6. Run agent if WebDriver is available
    // -------------------------------------------------------------------------
    if webdriver_available {
        println!("6. Running browser automation task...\n");

        // Start browser session
        browser.start().await?;
        println!("   Browser session started");

        // Create a simple task
        let task_content = Content {
            role: "user".to_string(),
            parts: vec![Part::Text {
                text: "Navigate to https://example.com and tell me what the page title is."
                    .to_string(),
            }],
        };

        let ctx = Arc::new(MockContext {
            agent: Arc::new(agent),
            content: task_content,
            config: RunConfig::default(),
            session: MockSession::new(),
        });

        // Run the agent
        let mut stream = ctx.agent().run(ctx.clone()).await?;

        println!("   Agent response:\n");
        while let Some(result) = stream.next().await {
            match result {
                Ok(event) => {
                    if let Some(content) = &event.llm_response.content {
                        for part in &content.parts {
                            if let Part::Text { text } = part {
                                print!("{}", text);
                            }
                        }
                    }
                }
                Err(e) => {
                    println!("   Error: {}", e);
                }
            }
        }
        println!("\n");

        // Clean up
        browser.stop().await?;
        println!("   Browser session stopped");
    } else {
        println!("6. Skipped live browser automation (WebDriver not available)");
    }

    // -------------------------------------------------------------------------
    // Summary
    // -------------------------------------------------------------------------
    println!("\n=== Example Complete ===\n");
    println!("Key features demonstrated:");
    println!("  - BrowserConfig for session configuration");
    println!("  - BrowserSession for WebDriver management");
    println!("  - BrowserToolset for all browser tools");
    println!("  - Integration with LlmAgent");
    println!("  - Full callback support via ADK architecture");
    println!("\nUsage patterns:");
    println!("  - Use minimal_browser_tools() for basic automation");
    println!("  - Use readonly_browser_tools() for scraping only");
    println!("  - Use BrowserToolset::new().with_*() for custom selection");

    Ok(())
}
