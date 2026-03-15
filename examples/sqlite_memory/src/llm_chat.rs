//! # LLM Chat with SQLite Memory
//!
//! A Gemini-powered chatbot that uses SQLite for both session persistence
//! and long-term memory via FTS5 full-text search. No external infrastructure
//! needed — everything runs locally in a single SQLite file.
//!
//! ## Prerequisites
//!
//! Set your Gemini API key:
//! ```bash
//! export GOOGLE_API_KEY="your-key-here"
//! ```
//!
//! ## Run
//!
//! ```bash
//! cargo run -p sqlite-memory-example --bin sqlite-memory-llm-chat
//! ```
//!
//! ## What This Demonstrates
//!
//! - SQLite as a lightweight session + memory backend
//! - FTS5 full-text search for memory retrieval
//! - Multi-turn conversation with memory injection
//! - Zero infrastructure — great for local dev and prototyping

use adk_agent::LlmAgentBuilder;
use adk_memory::{MemoryServiceAdapter, SqliteMemoryService};
use adk_model::gemini::GeminiModel;
use adk_runner::{Runner, RunnerConfig};
use adk_session::{CreateRequest, GetRequest, InMemorySessionService, SessionService};
use adk_tool::GoogleSearchTool;
use futures::StreamExt;
use serde_json::json;
use std::collections::HashMap;
use std::io::{self, Write};
use std::sync::Arc;

const APP_NAME: &str = "sqlite-memory-chat";
const USER_ID: &str = "demo-user";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt::init();

    println!("=== LLM Chat with SQLite Memory (FTS5) ===\n");

    // ── 1. Set up SQLite memory service ─────────────────────────────────
    let memory_service = Arc::new(SqliteMemoryService::new("sqlite:chat_memory.db").await?);
    memory_service.migrate().await?;
    println!("SQLite memory initialized (chat_memory.db)");

    // ── 2. Session service (in-memory for simplicity) ───────────────────
    let session_service = Arc::new(InMemorySessionService::new());

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
                "You are a friendly assistant with long-term memory. You can recall \
                 things the user told you in previous conversations. Use Google Search \
                 when asked about current events. Keep answers concise.",
            )
            .model(Arc::new(model))
            .tool(Arc::new(GoogleSearchTool::new()))
            .build()?,
    );

    // ── 5. Create a session ─────────────────────────────────────────────
    let session_id = "chat-session-1".to_string();

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
            initial_state.insert("app:name".to_string(), json!("SQLite Memory Chat"));
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
    println!("Memory is stored in chat_memory.db — persists across restarts!\n");

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
