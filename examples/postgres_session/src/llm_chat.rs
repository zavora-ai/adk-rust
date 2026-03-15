//! # LLM Chat with PostgreSQL Session Persistence
//!
//! This example shows how to build a Gemini-powered chatbot that stores
//! its conversation history and state in PostgreSQL. Every message you
//! send and every response the agent produces is persisted — so you can
//! stop the program, restart it, and pick up right where you left off.
//!
//! ## Prerequisites
//!
//! 1. Start a PostgreSQL container:
//!    ```bash
//!    docker run -d --name adk-postgres-test \
//!      -e POSTGRES_USER=adk -e POSTGRES_PASSWORD=adk_test \
//!      -e POSTGRES_DB=adk_sessions -p 5499:5432 postgres:17-alpine
//!    ```
//!
//! 2. Set your Gemini API key:
//!    ```bash
//!    export GOOGLE_API_KEY="your-key-here"
//!    ```
//!
//! 3. Run:
//!    ```bash
//!    cargo run -p postgres-session-example --bin postgres-llm-chat
//!    ```
//!
//! ## What This Demonstrates
//!
//! - Wiring a real database session backend into the ADK runner
//! - Multi-turn conversation with full history persistence
//! - Session state (app-level, user-level, session-level) stored in PostgreSQL
//! - Resuming a previous conversation by session ID

use adk_agent::LlmAgentBuilder;
use adk_model::gemini::GeminiModel;
use adk_runner::{Runner, RunnerConfig};
use adk_session::{CreateRequest, GetRequest, PostgresSessionService, SessionService};
use adk_tool::GoogleSearchTool;
use futures::StreamExt;
use serde_json::json;
use std::collections::HashMap;
use std::io::{self, Write};
use std::sync::Arc;

const DATABASE_URL: &str = "postgres://adk:adk_test@localhost:5499/adk_sessions";
const APP_NAME: &str = "postgres-chat";
const USER_ID: &str = "demo-user";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt::init();

    // ── 1. Connect to PostgreSQL ────────────────────────────────────────
    println!("=== LLM Chat with PostgreSQL Sessions ===\n");
    println!("Connecting to PostgreSQL...");
    let session_service = Arc::new(PostgresSessionService::new(DATABASE_URL).await?);
    session_service.migrate().await?;
    println!("Connected and migrated.\n");

    // ── 2. Set up the Gemini model ──────────────────────────────────────
    let api_key = std::env::var("GOOGLE_API_KEY")
        .or_else(|_| std::env::var("GEMINI_API_KEY"))
        .expect("Set GOOGLE_API_KEY or GEMINI_API_KEY");

    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    // ── 3. Build an agent with a search tool ────────────────────────────
    let agent = Arc::new(
        LlmAgentBuilder::new("assistant")
            .description("A helpful assistant with web search")
            .instruction(
                "You are a friendly assistant. Use Google Search when the user asks \
                 about current events, weather, or facts you're unsure about. \
                 Keep answers concise.",
            )
            .model(Arc::new(model))
            .tool(Arc::new(GoogleSearchTool::new()))
            .build()?,
    );

    // ── 4. Create (or resume) a session ─────────────────────────────────
    //
    // Try to resume an existing session. If it doesn't exist, create one.
    // This shows how PostgreSQL persistence lets you pick up conversations.
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
            let event_count = existing.events().len();
            println!("Resumed session '{session_id}' with {event_count} previous events.");
            existing
        }
        Err(_) => {
            // First run — create a new session with some initial state
            let mut initial_state = HashMap::new();
            initial_state.insert("app:name".to_string(), json!("PostgreSQL Chat Demo"));
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

    // ── 5. Wire the runner ──────────────────────────────────────────────
    //
    // The Runner connects the agent to the session service. Every message
    // and response is automatically persisted to PostgreSQL.
    let runner = Runner::new(RunnerConfig {
        app_name: APP_NAME.to_string(),
        agent: agent.clone(),
        session_service: session_service.clone(),
        artifact_service: None,
        memory_service: None,
        plugin_manager: None,
        run_config: None,
        compaction_config: None,
        context_cache_config: None,
        cache_capable: None,
        request_context: None,
        cancellation_token: None,
    })?;

    // ── 6. Interactive chat loop ────────────────────────────────────────
    println!("\nType a message and press Enter. Ctrl+C to exit.");
    println!("Your conversation is saved to PostgreSQL — restart to resume!\n");

    let stdin = io::stdin();
    loop {
        print!("You > ");
        io::stdout().flush()?;

        let mut input = String::new();
        if stdin.read_line(&mut input)? == 0 {
            break; // EOF
        }
        let input = input.trim();
        if input.is_empty() {
            continue;
        }

        // Send the user message to the agent
        let user_content = adk_core::Content::new("user").with_text(input);
        let mut events =
            runner.run(USER_ID.to_string(), session.id().to_string(), user_content).await?;

        // Stream the response
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
