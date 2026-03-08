//! # LLM Chat with Redis Session + Redis Memory
//!
//! A Gemini-powered chatbot that uses Redis for both session persistence
//! and long-term memory. Redis gives you fast in-memory storage with
//! keyword-based memory search across conversations.
//!
//! ## Prerequisites
//!
//! 1. Start a Redis container:
//!    ```bash
//!    docker run -d --name adk-redis-test -p 6399:6379 redis:7-alpine
//!    ```
//!
//! 2. Set your Gemini API key:
//!    ```bash
//!    export GOOGLE_API_KEY="your-key-here"
//!    ```
//!
//! 3. Run:
//!    ```bash
//!    cargo run -p redis-session-example --bin redis-memory-llm-chat
//!    ```
//!
//! ## What This Demonstrates
//!
//! - Redis as a unified session + memory backend
//! - Keyword-based memory search across conversations
//! - Multi-turn conversation with memory injection
//! - Low-latency persistence for chat applications

use adk_agent::LlmAgentBuilder;
use adk_memory::{MemoryServiceAdapter, RedisMemoryConfig, RedisMemoryService};
use adk_model::gemini::GeminiModel;
use adk_runner::{Runner, RunnerConfig};
use adk_session::{
    CreateRequest, GetRequest, RedisSessionConfig, RedisSessionService, SessionService,
};
use adk_tool::GoogleSearchTool;
use futures::StreamExt;
use serde_json::json;
use std::collections::HashMap;
use std::io::{self, Write};
use std::sync::Arc;

const REDIS_URL: &str = "redis://localhost:6399";
const APP_NAME: &str = "redis-memory-chat";
const USER_ID: &str = "demo-user";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt::init();

    println!("=== LLM Chat with Redis Session + Memory ===\n");

    // ── 1. Connect to Redis for sessions ────────────────────────────────
    println!("Connecting to Redis...");
    let session_config =
        RedisSessionConfig { url: REDIS_URL.to_string(), ttl: None, cluster_nodes: None };
    let session_service = Arc::new(RedisSessionService::new(session_config).await?);

    // ── 2. Connect to Redis for memory ──────────────────────────────────
    let memory_config = RedisMemoryConfig { url: REDIS_URL.to_string(), ttl: None };
    let memory_service = Arc::new(RedisMemoryService::new(memory_config).await?);
    println!("Connected (sessions + memory on same Redis).\n");

    // ── 3. Set up the Gemini model ──────────────────────────────────────
    let api_key = std::env::var("GOOGLE_API_KEY")
        .or_else(|_| std::env::var("GEMINI_API_KEY"))
        .expect("Set GOOGLE_API_KEY or GEMINI_API_KEY");

    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    // ── 4. Build an agent with memory enabled ───────────────────────────
    let agent = Arc::new(
        LlmAgentBuilder::new("assistant")
            .description("A helpful assistant with long-term memory")
            .instruction(
                "You are a friendly assistant with long-term memory backed by Redis. \
                 You can recall things the user told you in previous conversations. \
                 Use Google Search when asked about current events. Keep answers concise.",
            )
            .model(Arc::new(model))
            .tool(Arc::new(GoogleSearchTool::new()))
            .build()?,
    );

    // ── 5. Create (or resume) a session ─────────────────────────────────
    let session_id = "memory-chat-1".to_string();

    let session = match session_service
        .get(GetRequest {
            app_name: APP_NAME.to_string(),
            user_id: USER_ID.to_string(),
            session_id: session_id.clone(),
            num_recent_events: None,
            after: None,
        })
        .await
    {
        Ok(existing) => {
            println!("Resumed session with {} events.", existing.events().len());
            existing
        }
        Err(_) => {
            let mut initial_state = HashMap::new();
            initial_state.insert("app:name".to_string(), json!("Redis Memory Chat"));
            initial_state.insert("user:name".to_string(), json!("Demo User"));
            let new_session = session_service
                .create(CreateRequest {
                    app_name: APP_NAME.to_string(),
                    user_id: USER_ID.to_string(),
                    session_id: Some(session_id.clone()),
                    state: initial_state,
                })
                .await?;
            println!("Created new session '{}'.", new_session.id());
            new_session
        }
    };

    // ── 6. Wire the runner with memory ──────────────────────────────────
    let runner = Runner::new(RunnerConfig {
        app_name: APP_NAME.to_string(),
        agent: agent.clone(),
        session_service: session_service.clone(),
        artifact_service: None,
        memory_service: Some(Arc::new(MemoryServiceAdapter::new(
            memory_service,
            APP_NAME,
            USER_ID,
        ))),
        plugin_manager: None,
        run_config: None,
        compaction_config: None,
        context_cache_config: None,
        cache_capable: None,
        request_context: None,
        cancellation_token: None,
    })?;

    // ── 7. Interactive chat loop ────────────────────────────────────────
    println!("\nType a message and press Enter. Ctrl+C to exit.");
    println!("Sessions + memory stored in Redis — persists across restarts!\n");

    let stdin = io::stdin();
    loop {
        print!("You > ");
        io::stdout().flush()?;

        let mut input = String::new();
        if stdin.read_line(&mut input)? == 0 {
            break;
        }
        let input = input.trim();
        if input.is_empty() {
            continue;
        }

        let user_content = adk_core::Content::new("user").with_text(input);
        let mut events =
            runner.run(USER_ID.to_string(), session.id().to_string(), user_content).await?;

        print!("\nAssistant > ");
        io::stdout().flush()?;

        while let Some(event) = events.next().await {
            match event {
                Ok(evt) => {
                    if let Some(content) = &evt.llm_response.content {
                        for part in &content.parts {
                            if let adk_core::Part::Text { text } = part {
                                print!("{text}");
                                io::stdout().flush()?;
                            }
                        }
                    }
                }
                Err(e) => eprintln!("\nError: {e}"),
            }
        }
        println!("\n");
    }

    Ok(())
}
