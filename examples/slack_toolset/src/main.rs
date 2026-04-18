//! # Slack Toolset Example
//!
//! Demonstrates the native Slack toolset from ADK-Rust v0.7.0 — sending
//! messages, reading channels, adding reactions, and listing threads via
//! the Slack API, all driven by an LLM agent.
//!
//! ## What This Shows
//!
//! - Creating a `SlackToolset` with a Slack Bot Token
//! - Building an `LlmAgent` that uses Slack tools (`slack_send_message`,
//!   `slack_read_channel`, `slack_add_reaction`, `slack_list_threads`)
//! - Dry-run mode: prints what API calls would be made when no token is set
//! - Live mode: executes real Slack API calls when `SLACK_BOT_TOKEN` is set
//! - Handling Slack API errors with descriptive messages
//!
//! ## Prerequisites
//!
//! - `GOOGLE_API_KEY` environment variable set (for the Gemini LLM provider)
//! - (Optional) `SLACK_BOT_TOKEN` for live mode
//! - (Optional) `SLACK_CHANNEL` to specify a target channel (default: `#general`)
//!
//! ## Run
//!
//! ```bash
//! cargo run --manifest-path examples/slack_toolset/Cargo.toml
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_core::{Content, Part, SessionId, Toolset, UserId};
use adk_model::GeminiModel;
use adk_runner::{Runner, RunnerConfig};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_tool::slack::SlackToolset;
use futures::StreamExt;
use tracing_subscriber::EnvFilter;

const APP_NAME: &str = "slack-toolset-example";

// ---------------------------------------------------------------------------
// Helper: require an environment variable or exit with a descriptive message
// ---------------------------------------------------------------------------

fn require_env(name: &str) -> anyhow::Result<String> {
    std::env::var(name).map_err(|_| {
        anyhow::anyhow!(
            "Missing required environment variable: {name}\n\
             Set it in your .env file or export it in your shell.\n\
             See .env.example for all required variables."
        )
    })
}

// ---------------------------------------------------------------------------
// Helper: create a Runner with an in-memory session
// ---------------------------------------------------------------------------

async fn make_runner(
    agent: Arc<dyn adk_core::Agent>,
    session_id: &str,
) -> anyhow::Result<Runner> {
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP_NAME.into(),
            user_id: "user".into(),
            session_id: Some(session_id.into()),
            state: HashMap::new(),
        })
        .await?;
    Ok(Runner::new(RunnerConfig {
        app_name: APP_NAME.into(),
        agent,
        session_service: sessions,
        artifact_service: None,
        memory_service: None,
        plugin_manager: None,
        run_config: None,
        compaction_config: None,
        context_cache_config: None,
        cache_capable: None,
        request_context: None,
        cancellation_token: None,
        intra_compaction_config: None,
        intra_compaction_summarizer: None,
    })?)
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // --- Environment Setup ---
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    println!("╔══════════════════════════════════════════╗");
    println!("║  Slack Toolset — ADK-Rust v0.7.0         ║");
    println!("╚══════════════════════════════════════════╝\n");

    // Check for the LLM provider key (required for both modes).
    let api_key = require_env("GOOGLE_API_KEY")?;

    // Determine the target channel (defaults to #general).
    let channel = std::env::var("SLACK_CHANNEL").unwrap_or_else(|_| "#general".to_string());

    // -----------------------------------------------------------------------
    // Dry-run vs Live mode
    // -----------------------------------------------------------------------
    //
    // When SLACK_BOT_TOKEN is not set, the example runs in dry-run mode:
    // it prints what the Slack tools would do without making real API calls.
    //
    // When SLACK_BOT_TOKEN is set, the example creates a real SlackToolset
    // and runs the agent against the live Slack API.

    let dry_run = std::env::var("SLACK_BOT_TOKEN").is_err();

    if dry_run {
        // ---------------------------------------------------------------
        // Dry-run mode: describe the tools and what they would do
        // ---------------------------------------------------------------
        println!("⚠️  Running in dry-run mode (no SLACK_BOT_TOKEN set)");
        println!("   Set SLACK_BOT_TOKEN to run against the real Slack API.\n");

        println!("--- Available Slack Tools ---\n");
        println!("  1. slack_send_message");
        println!("     Posts a message to a Slack channel.");
        println!("     Parameters: channel (string), text (string), thread_ts (optional)");
        println!("     Would call: POST https://slack.com/api/chat.postMessage\n");

        println!("  2. slack_read_channel");
        println!("     Reads recent messages from a Slack channel.");
        println!("     Parameters: channel (string), limit (optional, default 10)");
        println!("     Would call: POST https://slack.com/api/conversations.history\n");

        println!("  3. slack_add_reaction");
        println!("     Adds an emoji reaction to a message.");
        println!("     Parameters: channel (string), timestamp (string), name (string)");
        println!("     Would call: POST https://slack.com/api/reactions.add\n");

        println!("  4. slack_list_threads");
        println!("     Lists threaded replies in a channel.");
        println!("     Parameters: channel (string), ts (string), limit (optional)");
        println!("     Would call: POST https://slack.com/api/conversations.replies\n");

        println!("--- Simulated Agent Interaction ---\n");
        println!("  Agent prompt: \"Read the last 5 messages from {channel},");
        println!("                  then send a summary and react with 👍.\"\n");
        println!("  In live mode the agent would:");
        println!("    1. Call slack_read_channel(channel=\"{channel}\", limit=5)");
        println!("    2. Summarize the messages using the LLM");
        println!("    3. Call slack_send_message(channel=\"{channel}\", text=<summary>)");
        println!("    4. Call slack_add_reaction(channel=\"{channel}\", timestamp=<ts>, name=\"thumbsup\")\n");

        println!("  To run in live mode:");
        println!("    export SLACK_BOT_TOKEN=xoxb-your-bot-token");
        println!("    export SLACK_CHANNEL=#your-channel");
        println!("    cargo run --manifest-path examples/slack_toolset/Cargo.toml");
    } else {
        // ---------------------------------------------------------------
        // Live mode: create a real SlackToolset and run the agent
        // ---------------------------------------------------------------
        let token = std::env::var("SLACK_BOT_TOKEN").unwrap();
        println!("🔑 Running in live mode with SLACK_BOT_TOKEN");
        println!("   Target channel: {channel}\n");

        // Create the Slack toolset with the bot token.
        // SlackToolset implements the Toolset trait and provides four tools:
        //   slack_send_message, slack_read_channel, slack_add_reaction, slack_list_threads
        let slack_toolset = SlackToolset::new(&token);

        // Create the Gemini model for the agent.
        let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash")?);

        // Build an LlmAgent with the Slack toolset and instructions that
        // demonstrate all three primary tools.
        let agent = Arc::new(
            LlmAgentBuilder::new("slack-assistant")
                .description("An assistant that interacts with Slack channels")
                .model(model)
                .toolset(Arc::new(slack_toolset) as Arc<dyn Toolset>)
                .instruction(format!(
                    "You are a Slack assistant. Use the Slack tools to interact with \
                     the channel '{channel}'.\n\n\
                     Your task:\n\
                     1. Read the last 5 messages from the channel using slack_read_channel.\n\
                     2. Write a brief one-sentence summary of what was discussed.\n\
                     3. Send that summary to the channel using slack_send_message.\n\
                     4. React to your own summary message with a thumbsup emoji \
                        using slack_add_reaction.\n\n\
                     Be concise and helpful."
                ))
                .build()?,
        );

        // Create a runner and execute the agent.
        let runner = make_runner(agent, "slack-session").await?;

        println!("--- Running Slack Agent ---\n");

        let mut stream = runner
            .run(
                UserId::new("user")?,
                SessionId::new("slack-session")?,
                Content::new("user").with_text(format!(
                    "Please read the last 5 messages from {channel}, summarize them, \
                     send the summary back to the channel, and react with 👍."
                )),
            )
            .await?;

        // Stream and print agent events.
        while let Some(event) = stream.next().await {
            let event = event?;
            if let Some(content) = &event.llm_response.content {
                for part in &content.parts {
                    match part {
                        Part::Text { text } if !text.trim().is_empty() => {
                            println!("  💬 Agent: {text}");
                        }
                        Part::FunctionCall { name, args, .. } => {
                            println!("  🔧 Tool call: {name}({args})");
                        }
                        Part::FunctionResponse { function_response, .. } => {
                            // Check for Slack API errors in the response.
                            let response = &function_response.response;
                            if let Some(ok) = response.get("ok") {
                                if ok == false {
                                    let error_code = response
                                        .get("error")
                                        .and_then(|e| e.as_str())
                                        .unwrap_or("unknown");
                                    println!(
                                        "  ❌ Slack API error: {error_code} — {}",
                                        describe_slack_error(error_code)
                                    );
                                }
                            }
                            println!(
                                "  ← Response from {}: {}",
                                function_response.name,
                                serde_json::to_string_pretty(&function_response.response)
                                    .unwrap_or_default()
                            );
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    println!("\n✅ Slack Toolset example completed successfully.");
    Ok(())
}

// ---------------------------------------------------------------------------
// Helper: describe common Slack API error codes
// ---------------------------------------------------------------------------

/// Returns a human-readable description for common Slack API error codes.
fn describe_slack_error(code: &str) -> &str {
    match code {
        "channel_not_found" => "The specified channel does not exist or the bot is not a member.",
        "not_in_channel" => "The bot is not a member of the specified channel.",
        "invalid_auth" => "The bot token is invalid or has been revoked.",
        "token_revoked" => "The bot token has been revoked.",
        "missing_scope" => "The bot token is missing a required OAuth scope.",
        "no_text" => "The message text was empty.",
        "msg_too_long" => "The message exceeds Slack's maximum length.",
        "rate_limited" => "Too many requests — wait and retry.",
        "already_reacted" => "The bot has already added this reaction.",
        "message_not_found" => "The specified message was not found.",
        "too_many_emoji" => "Too many emoji reactions on this message.",
        _ => "An unexpected Slack API error occurred.",
    }
}
