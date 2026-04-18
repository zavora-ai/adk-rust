//! # Intra-Compaction Example
//!
//! Demonstrates ADK-Rust's intra-invocation context compaction — automatically
//! summarizing older conversation events when the estimated token count exceeds
//! a threshold, keeping the context window manageable during long conversations.
//!
//! ## What This Shows
//!
//! - Configuring `IntraCompactionConfig` with a low token threshold to trigger
//!   compaction quickly during the demo
//! - Using `estimate_tokens()` to monitor estimated token usage
//! - Wiring `IntraInvocationCompactor` into the `Runner` via
//!   `intra_compaction_config` and `intra_compaction_summarizer`
//! - Overlap preservation: recent events are kept after compaction for continuity
//! - Coherence after compaction: the agent still references earlier context
//!
//! ## Prerequisites
//!
//! - `GOOGLE_API_KEY` environment variable set (for the Gemini LLM provider)
//!
//! ## Run
//!
//! ```bash
//! cargo run --manifest-path examples/intra_compaction/Cargo.toml
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_core::intra_compaction::estimate_tokens;
use adk_core::{Content, IntraCompactionConfig, Part, SessionId, UserId};
use adk_model::GeminiModel;
use adk_runner::{Runner, RunnerConfig};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use futures::StreamExt;
use tracing_subscriber::EnvFilter;

const APP_NAME: &str = "intra-compaction-example";

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
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // --- Environment Setup ---
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    println!("╔══════════════════════════════════════════╗");
    println!("║  Intra-Compaction — ADK-Rust v0.7.0      ║");
    println!("╚══════════════════════════════════════════╝\n");

    let api_key = require_env("GOOGLE_API_KEY")?;

    // -----------------------------------------------------------------------
    // Step 1: Configure intra-compaction with a low threshold
    // -----------------------------------------------------------------------
    //
    // In production you would use a higher threshold (e.g., 50_000–100_000).
    // We use a low value here so compaction triggers quickly during the demo.

    let compaction_config = IntraCompactionConfig {
        token_threshold: 2000,
        overlap_event_count: 3,
        chars_per_token: 4,
    };

    println!("--- Step 1: Compaction Configuration ---\n");
    println!("  Token threshold:     {} tokens", compaction_config.token_threshold);
    println!("  Overlap event count: {} events preserved after compaction", compaction_config.overlap_event_count);
    println!("  Chars-per-token:     {} (heuristic ratio for estimation)", compaction_config.chars_per_token);

    // -----------------------------------------------------------------------
    // Step 2: Create the LLM model, agent, and summarizer
    // -----------------------------------------------------------------------
    //
    // The summarizer uses the same LLM to generate summaries of older events.
    // In production you might use a cheaper/faster model for summarization.

    println!("\n--- Step 2: Create Agent and Summarizer ---\n");

    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash")?);
    println!("  ✓ Created Gemini model (gemini-2.0-flash)");

    // The LlmEventSummarizer reuses the BaseEventsSummarizer trait from adk-core.
    // It formats conversation events into a prompt and asks the LLM to summarize.
    let summarizer: Arc<dyn adk_core::BaseEventsSummarizer> =
        Arc::new(adk_agent::LlmEventSummarizer::new(model.clone()));
    println!("  ✓ Created LlmEventSummarizer (uses same model for summaries)");

    let agent = Arc::new(
        LlmAgentBuilder::new("compaction-demo")
            .description("A helpful assistant for demonstrating intra-compaction")
            .instruction(
                "You are a knowledgeable assistant. Answer questions thoroughly with \
                 detailed explanations. When asked about previous topics in the conversation, \
                 reference what was discussed earlier to show continuity.",
            )
            .model(model.clone())
            .build()?,
    );
    println!("  ✓ Created LlmAgent 'compaction-demo'");

    // -----------------------------------------------------------------------
    // Step 3: Set up the Runner with compaction enabled
    // -----------------------------------------------------------------------

    println!("\n--- Step 3: Set Up Runner with Compaction ---\n");

    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let session_id = "compaction-session";
    sessions
        .create(CreateRequest {
            app_name: APP_NAME.into(),
            user_id: "user".into(),
            session_id: Some(session_id.into()),
            state: HashMap::new(),
        })
        .await?;

    let runner = Runner::new(RunnerConfig {
        app_name: APP_NAME.into(),
        agent: agent.clone(),
        session_service: sessions.clone(),
        artifact_service: None,
        memory_service: None,
        plugin_manager: None,
        run_config: None,
        compaction_config: None,
        context_cache_config: None,
        cache_capable: None,
        request_context: None,
        cancellation_token: None,
        intra_compaction_config: Some(compaction_config.clone()),
        intra_compaction_summarizer: Some(summarizer),
    })?;

    println!("  ✓ Runner created with intra-compaction enabled");

    // -----------------------------------------------------------------------
    // Step 4: Multi-turn conversation to trigger compaction
    // -----------------------------------------------------------------------
    //
    // We send several long prompts to build up the context. The runner
    // estimates token count before each agent call and triggers compaction
    // when the threshold is exceeded.

    println!("\n--- Step 4: Multi-Turn Conversation ---\n");

    let prompts = [
        "Explain the key differences between TCP and UDP protocols in computer networking. \
         Cover reliability, ordering, connection setup, and typical use cases for each. \
         Include details about the three-way handshake, flow control, and congestion control \
         mechanisms in TCP, and why UDP is preferred for real-time applications like video \
         streaming and online gaming.",

        "Now explain how HTTP/2 improves upon HTTP/1.1. Discuss multiplexing, header \
         compression with HPACK, server push, stream prioritization, and the binary framing \
         layer. Also explain why HTTP/2 still uses TCP and what problems that causes, \
         particularly head-of-line blocking at the transport layer.",

        "Building on what we discussed about TCP limitations, explain how QUIC protocol \
         and HTTP/3 address these issues. Cover the use of UDP as the transport layer, \
         built-in TLS 1.3, connection migration, independent stream multiplexing without \
         head-of-line blocking, and 0-RTT connection establishment. Compare the latency \
         characteristics with HTTP/2 over TCP.",

        "Explain the concept of WebSockets and how they differ from regular HTTP requests. \
         Cover the upgrade handshake, full-duplex communication, frame types (text, binary, \
         ping/pong, close), and common use cases like chat applications, live dashboards, \
         and collaborative editing. Also discuss the relationship between WebSockets and \
         the protocols we discussed earlier (HTTP/1.1, HTTP/2, QUIC).",
    ];

    let chars_per_token = compaction_config.chars_per_token;

    for (i, prompt) in prompts.iter().enumerate() {
        let turn = i + 1;
        println!("  ┌─ Turn {turn} ─────────────────────────────────────────");
        println!("  │ User: {}...", &prompt[..60]);

        // Estimate tokens before this turn by checking the session events
        let session = sessions
            .get(adk_session::GetRequest {
                app_name: APP_NAME.into(),
                user_id: "user".into(),
                session_id: session_id.into(),
                num_recent_events: None,
                after: None,
            })
            .await?;
        let events_before = session.events().all();
        let tokens_before = estimate_tokens(&events_before, chars_per_token);
        println!("  │ Estimated tokens before turn: {tokens_before}");
        println!("  │ Events in session: {}", events_before.len());

        // Send the prompt through the runner
        let user_content = Content {
            role: "user".to_string(),
            parts: vec![Part::Text { text: prompt.to_string() }],
        };

        let mut stream = runner
            .run(UserId::new("user")?, SessionId::new(session_id)?, user_content)
            .await?;

        let mut response_text = String::new();
        while let Some(result) = stream.next().await {
            match result {
                Ok(event) => {
                    if let Some(content) = &event.llm_response.content {
                        for part in &content.parts {
                            if let Part::Text { text } = part {
                                response_text.push_str(text);
                            }
                        }
                    }
                }
                Err(e) => {
                    println!("  │ ⚠ Error: {e}");
                    break;
                }
            }
        }

        // Show a truncated response
        let preview = if response_text.len() > 120 {
            format!("{}...", &response_text[..120])
        } else {
            response_text.clone()
        };
        println!("  │ Agent: {preview}");

        // Check tokens after this turn
        let session_after = sessions
            .get(adk_session::GetRequest {
                app_name: APP_NAME.into(),
                user_id: "user".into(),
                session_id: session_id.into(),
                num_recent_events: None,
                after: None,
            })
            .await?;
        let events_after = session_after.events().all();
        let tokens_after = estimate_tokens(&events_after, chars_per_token);
        println!("  │ Estimated tokens after turn:  {tokens_after}");
        println!("  │ Events in session after turn: {}", events_after.len());

        // Check if compaction likely occurred (fewer events than expected)
        // Each turn adds 2 events (user + agent), so without compaction
        // we'd expect 2 * turn events.
        let expected_without_compaction = 2 * turn;
        if events_after.len() < expected_without_compaction {
            println!("  │ 🗜️  Compaction detected! Events reduced from expected {expected_without_compaction} to {}", events_after.len());
            println!("  │    Preserved recent events (overlap): {}", compaction_config.overlap_event_count.min(events_after.len()));
        }

        println!("  └──────────────────────────────────────────────\n");
    }

    // -----------------------------------------------------------------------
    // Step 5: Coherence check — reference earlier context after compaction
    // -----------------------------------------------------------------------
    //
    // After compaction, the agent should still have relevant context from the
    // summary. We ask a follow-up that references the first topic (TCP/UDP).

    println!("--- Step 5: Coherence Check After Compaction ---\n");

    let followup = "Based on everything we discussed about networking protocols — \
         from TCP/UDP through HTTP/2, QUIC, and WebSockets — which protocol stack \
         would you recommend for building a real-time collaborative document editor, \
         and why? Reference the specific trade-offs we covered earlier.";

    println!("  User: {followup}\n");

    let followup_content = Content {
        role: "user".to_string(),
        parts: vec![Part::Text { text: followup.to_string() }],
    };

    let mut stream = runner
        .run(UserId::new("user")?, SessionId::new(session_id)?, followup_content)
        .await?;

    let mut followup_response = String::new();
    while let Some(result) = stream.next().await {
        match result {
            Ok(event) => {
                if let Some(content) = &event.llm_response.content {
                    for part in &content.parts {
                        if let Part::Text { text } = part {
                            followup_response.push_str(text);
                        }
                    }
                }
            }
            Err(e) => {
                println!("  ⚠ Error: {e}");
                break;
            }
        }
    }

    // Print the full follow-up response to show coherence
    let preview = if followup_response.len() > 500 {
        format!("{}...", &followup_response[..500])
    } else {
        followup_response.clone()
    };
    println!("  Agent: {preview}");

    // Final session stats
    let final_session = sessions
        .get(adk_session::GetRequest {
            app_name: APP_NAME.into(),
            user_id: "user".into(),
            session_id: session_id.into(),
            num_recent_events: None,
            after: None,
        })
        .await?;
    let final_events = final_session.events().all();
    let final_tokens = estimate_tokens(&final_events, chars_per_token);

    println!("\n--- Final Session Stats ---\n");
    println!("  Total events in session: {}", final_events.len());
    println!("  Estimated token count:   {final_tokens}");
    println!("  Total turns completed:   5 (4 conversation + 1 follow-up)");

    println!("\n✅ Intra-Compaction example completed successfully.");
    Ok(())
}
