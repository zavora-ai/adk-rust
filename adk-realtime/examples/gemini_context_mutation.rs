//! Gemini Live — Context Mutation via Session Resumption
//!
//! Demonstrates mid-session context changes with Gemini Live, which uses
//! session resumption (reconnect with new config) instead of in-place updates.
//! The runner handles this transparently — the app code is identical to OpenAI.
//!
//! Requires: `GOOGLE_API_KEY` environment variable.
//!
//! Run: `cargo run -p adk-realtime --features gemini --example gemini_context_mutation`

use adk_realtime::config::{RealtimeConfig, SessionUpdateConfig, ToolDefinition};
use adk_realtime::events::ServerEvent;
use adk_realtime::runner::{FnToolHandler, RealtimeRunner};
use serde_json::json;
use tracing::{error, info, warn};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    dotenvy::dotenv().ok();

    let api_key = std::env::var("GOOGLE_API_KEY").expect("GOOGLE_API_KEY must be set");
    let model_id = std::env::var("GEMINI_LIVE_MODEL").unwrap_or("gemini-2.0-flash-live-001".into());

    info!("connecting to Gemini Live with model {model_id}");

    // Phase 1: Technical support agent
    let backend = adk_realtime::gemini::GeminiLiveBackend::studio(&api_key);
    let model = adk_realtime::gemini::GeminiRealtimeModel::new(backend, &model_id);

    let lookup_tool = ToolDefinition {
        name: "lookup_account".into(),
        description: Some("Look up a customer account by ID".into()),
        parameters: Some(json!({
            "type": "object",
            "properties": {
                "account_id": { "type": "string", "description": "Customer account ID" }
            },
            "required": ["account_id"]
        })),
    };

    let runner = RealtimeRunner::builder()
        .model(std::sync::Arc::new(model))
        .config(
            RealtimeConfig::default()
                .with_instruction(
                    "You are a technical support agent. Help users troubleshoot issues. Be patient and thorough.",
                )
                .with_voice("Puck"),
        )
        .tool(
            lookup_tool,
            FnToolHandler::new(|call| {
                let account_id = call.arguments["account_id"]
                    .as_str()
                    .unwrap_or("unknown")
                    .to_string();
                info!("looking up account {account_id}");
                Ok(json!({
                    "account_id": account_id,
                    "plan": "premium",
                    "status": "active",
                    "open_tickets": 2
                }))
            }),
        )
        .build()?;

    runner.connect().await?;
    info!("connected — phase 1: technical support");

    // Send initial query
    runner.send_text("Hi, I'm having trouble with my account ABC-123").await?;
    runner.create_response().await?;

    // Collect response
    let mut response_text = String::new();
    let mut event_count = 0;
    while let Some(event) = runner.next_event().await {
        event_count += 1;
        match event {
            Ok(ServerEvent::TextDelta { delta, .. }) => {
                response_text.push_str(&delta);
            }
            Ok(ServerEvent::ResponseDone { .. }) => {
                info!("phase 1 response ({event_count} events): {response_text}");
                break;
            }
            Ok(ServerEvent::Error { error, .. }) => {
                error!("error: {}", error.message);
                break;
            }
            Err(e) => {
                error!("stream error: {e}");
                break;
            }
            _ => {}
        }
        // Safety: don't loop forever
        if event_count > 200 {
            warn!("too many events, breaking");
            break;
        }
    }

    // Phase 2: Switch to billing agent
    // For Gemini, this triggers a session resumption (reconnect with new config).
    // The runner handles this transparently — same API as OpenAI.
    info!("switching to billing agent persona...");

    let billing_tools = vec![ToolDefinition {
        name: "get_invoice".into(),
        description: Some("Retrieve an invoice by number".into()),
        parameters: Some(json!({
            "type": "object",
            "properties": {
                "invoice_number": { "type": "string" }
            },
            "required": ["invoice_number"]
        })),
    }];

    let update = SessionUpdateConfig(
        RealtimeConfig::default()
            .with_instruction(
                "You are now a billing specialist. Help users with invoices, payments, and account charges. Be precise with numbers.",
            )
            .with_tools(billing_tools),
    );

    // This will trigger RequiresResumption for Gemini — the runner queues
    // a safe reconnect and executes it when the model is idle.
    match runner.update_session(update).await {
        Ok(()) => info!("context mutation accepted (may be queued for resumption)"),
        Err(e) => {
            // Gemini may not support resumption on all model versions
            warn!("context mutation failed: {e} — continuing with original config");
        }
    }

    // Send a billing query
    runner.send_text("Can you look up invoice INV-2026-0042?").await?;
    runner.create_response().await?;

    response_text.clear();
    event_count = 0;
    while let Some(event) = runner.next_event().await {
        event_count += 1;
        match event {
            Ok(ServerEvent::TextDelta { delta, .. }) => {
                response_text.push_str(&delta);
            }
            Ok(ServerEvent::ResponseDone { .. }) => {
                info!("phase 2 response ({event_count} events): {response_text}");
                break;
            }
            Ok(ServerEvent::Error { error, .. }) => {
                error!("error: {}", error.message);
                break;
            }
            Err(e) => {
                error!("stream error: {e}");
                break;
            }
            _ => {}
        }
        if event_count > 200 {
            warn!("too many events, breaking");
            break;
        }
    }

    runner.close().await?;
    info!("session closed");

    Ok(())
}
