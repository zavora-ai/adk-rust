//! Multi-Tenant Browser Pool Example
//!
//! Demonstrates pool-backed `BrowserToolset` with per-user session isolation
//! using the `LlmAgentBuilder::toolset()` API. This is the production path
//! for multi-tenant browser agents introduced by the browser production
//! hardening spec.
//!
//! ## Features Showcased
//!
//! - `BrowserToolset::with_pool()` — pool-backed session resolution
//! - `BrowserToolset::with_pool_and_profile()` — pool + profile filtering
//! - `LlmAgentBuilder::toolset()` — dynamic toolset registration
//! - `BrowserSession::ensure_started()` — automatic session lifecycle
//! - Navigation tool page context alignment
//!
//! ## Requirements
//!
//! 1. WebDriver: `docker run -d -p 4444:4444 selenium/standalone-chrome`
//! 2. `GOOGLE_API_KEY` environment variable
//!
//! ## Running
//!
//! ```bash
//! cargo run --example browser_pool --features browser
//! ```

use adk_agent::LlmAgentBuilder;
use adk_browser::{
    BrowserConfig, BrowserProfile, BrowserSession, BrowserSessionPool, BrowserToolset,
};
use adk_core::{
    Agent, Content, InvocationContext, Part, ReadonlyContext, RunConfig, Session, State,
};
use adk_model::GeminiModel;
use async_trait::async_trait;
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Minimal session / context boilerplate
// ---------------------------------------------------------------------------

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
    id: String,
    user_id: String,
    state: SimpleState,
}
impl Session for SimpleSession {
    fn id(&self) -> &str {
        &self.id
    }
    fn app_name(&self) -> &str {
        "browser_pool"
    }
    fn user_id(&self) -> &str {
        &self.user_id
    }
    fn state(&self) -> &dyn State {
        &self.state
    }
    fn conversation_history(&self) -> Vec<Content> {
        Vec::new()
    }
}

struct AgentCtx {
    agent: Arc<dyn Agent>,
    content: Content,
    config: RunConfig,
    session: SimpleSession,
}

#[async_trait]
impl ReadonlyContext for AgentCtx {
    fn invocation_id(&self) -> &str {
        "inv-1"
    }
    fn agent_name(&self) -> &str {
        self.agent.name()
    }
    fn user_id(&self) -> &str {
        self.session.user_id()
    }
    fn app_name(&self) -> &str {
        "browser_pool"
    }
    fn session_id(&self) -> &str {
        self.session.id()
    }
    fn branch(&self) -> &str {
        ""
    }
    fn user_content(&self) -> &Content {
        &self.content
    }
}

#[async_trait]
impl adk_core::CallbackContext for AgentCtx {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}

#[async_trait]
impl InvocationContext for AgentCtx {
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

// ---------------------------------------------------------------------------
// Helper: run agent for a specific user
// ---------------------------------------------------------------------------

async fn run_for_user(
    agent: Arc<dyn Agent>,
    user_id: &str,
    task: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let content =
        Content { role: "user".to_string(), parts: vec![Part::Text { text: task.to_string() }] };
    let ctx = Arc::new(AgentCtx {
        agent: agent.clone(),
        content,
        config: RunConfig::default(),
        session: SimpleSession {
            id: format!("session-{user_id}"),
            user_id: user_id.to_string(),
            state: SimpleState::new(),
        },
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
            Err(e) => return Err(format!("agent error: {e}").into()),
        }
    }
    Ok(response)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    let _ = dotenvy::dotenv();

    println!("=== Multi-Tenant Browser Pool Example ===\n");

    // --- Pre-flight checks ---------------------------------------------------
    let api_key = match std::env::var("GOOGLE_API_KEY") {
        Ok(k) => k,
        Err(_) => {
            println!("GOOGLE_API_KEY not set. export GOOGLE_API_KEY=your_key");
            return Ok(());
        }
    };
    let webdriver_url =
        std::env::var("WEBDRIVER_URL").unwrap_or_else(|_| "http://localhost:4444".to_string());
    let ok = reqwest::Client::new()
        .get(format!("{webdriver_url}/status"))
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
        .is_ok();
    if !ok {
        println!("WebDriver not available at {webdriver_url}");
        println!("Start: docker run -d -p 4444:4444 selenium/standalone-chrome");
        return Ok(());
    }

    // --- Create a shared browser session pool --------------------------------
    let config =
        BrowserConfig::new().webdriver_url(&webdriver_url).headless(true).viewport(1280, 720);
    let pool = Arc::new(BrowserSessionPool::new(config, 10));
    println!("Browser pool created (lazy — sessions start on first use)\n");

    // =========================================================================
    // Example 1: Pool-backed toolset with full profile
    // =========================================================================
    println!("--- Example 1: Pool-backed BrowserToolset (full profile) ---");
    let _toolset_full = BrowserToolset::with_pool(pool.clone());
    println!("  Created BrowserToolset::with_pool() — all tool categories enabled");

    // =========================================================================
    // Example 2: Pool-backed toolset with Scraping profile
    // =========================================================================
    println!("--- Example 2: Pool-backed BrowserToolset (Scraping profile) ---");
    let toolset_scraping =
        BrowserToolset::with_pool_and_profile(pool.clone(), BrowserProfile::Scraping);
    println!("  Created with_pool_and_profile(Scraping) — navigation + extraction only\n");

    // =========================================================================
    // Example 3: Build an agent using .toolset() instead of .tool()
    // =========================================================================
    println!("--- Example 3: Agent with dynamic toolset resolution ---");
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);

    let agent: Arc<dyn Agent> = Arc::new(
        LlmAgentBuilder::new("browser_agent")
            .model(model)
            .description("Multi-tenant browser agent with pool-backed sessions")
            .instruction(
                "You are a web research assistant. Use browser tools to navigate and extract info.",
            )
            // Register the toolset — tools are resolved per-invocation using ctx.user_id()
            .toolset(Arc::new(toolset_scraping))
            .build()?,
    );
    println!("  Agent built with .toolset() — tools resolve at runtime per user\n");

    // =========================================================================
    // Example 4: Run for two different users (sequential — Selenium standalone
    // supports only one concurrent session, so we clean up between users)
    // =========================================================================
    println!("--- Example 4: Per-user session isolation ---");

    println!("  Running for user 'alice'...");
    let r1 = run_for_user(
        agent.clone(),
        "alice",
        "Navigate to https://example.com and tell me the page title.",
    )
    .await?;
    println!("  Alice's response: {}\n", r1.chars().take(200).collect::<String>());

    // Clean up Alice's session so Selenium can serve Bob
    // (Selenium standalone only supports 1 concurrent session)
    println!("  Releasing Alice's session before Bob's turn...");
    pool.release("alice").await?;

    println!("  Running for user 'bob'...");
    let r2 = run_for_user(
        agent.clone(),
        "bob",
        "Navigate to https://example.com and extract the main heading text.",
    )
    .await?;
    println!("  Bob's response: {}\n", r2.chars().take(200).collect::<String>());

    println!("  Active pool sessions: {}", pool.active_count().await);
    println!("  Active users: {:?}", pool.active_users().await);

    // =========================================================================
    // Example 5: Fixed-session toolset still works (backward compat)
    // =========================================================================
    println!("\n--- Example 5: Fixed-session backward compatibility ---");
    let fixed_session = Arc::new(BrowserSession::new(
        BrowserConfig::new().webdriver_url(&webdriver_url).headless(true),
    ));
    let _fixed_toolset = BrowserToolset::new(fixed_session.clone());
    println!("  BrowserToolset::new() still works unchanged for single-session use");

    // Cleanup
    pool.cleanup_all().await;
    println!("\nPool cleaned up. Example complete.");
    Ok(())
}
