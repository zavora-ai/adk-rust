//! # Telemetry SQLite Export Example
//!
//! An agentic LLM run traced end-to-end into a local SQLite file — zero
//! observability infrastructure (no OTLP collector, no backend).
//!
//! The agent answers two questions, calling a function tool for one of them.
//! Every agent-loop span (`agent.execute`, `call_llm`, `execute_tool …`) is
//! exported to `traces.db` by `adk-telemetry`'s `sqlite` feature, then read
//! back with `SqliteTraceReader` and printed as a session trace summary.
//!
//! ## Run
//!
//! ```bash
//! GOOGLE_API_KEY=... cargo run --manifest-path examples/telemetry_sqlite_export/Cargo.toml
//! ```
//!
//! Inspect the database afterwards with any SQLite client:
//!
//! ```bash
//! sqlite3 traces.db 'SELECT span_name, session_id FROM spans'
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_core::{SessionId, UserId};
use adk_model::GeminiModel;
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_telemetry::init_with_sqlite;
use adk_telemetry::sqlite::SqliteTraceReader;
use adk_tool::FunctionTool;
use futures::StreamExt;
use serde_json::json;

const APP_NAME: &str = "telemetry-sqlite-example";
const DB_PATH: &str = "traces.db";

/// Arguments for the weather tool — the schema tells the model what to pass.
#[derive(serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[allow(dead_code)]
struct WeatherArgs {
    /// The city to look up
    city: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    // One line of setup: spans land in traces.db, no collector needed.
    let exporter = init_with_sqlite(APP_NAME, DB_PATH)?;

    let api_key = std::env::var("GOOGLE_API_KEY")
        .map_err(|_| anyhow::anyhow!("GOOGLE_API_KEY is not set"))?;
    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    let weather_tool = FunctionTool::new(
        "get_weather",
        "Get the current weather for a city",
        |_ctx, args| async move {
            let city = args["city"].as_str().unwrap_or("Unknown").to_string();
            Ok(json!({ "city": city, "temp_c": 22, "condition": "sunny" }))
        },
    )
    .with_parameters_schema::<WeatherArgs>();

    let agent = Arc::new(
        LlmAgentBuilder::new("traced_assistant")
            .description("Assistant whose every step is traced to SQLite")
            .instruction("Be concise. Use the get_weather tool for weather questions.")
            .model(Arc::new(model))
            .tool(Arc::new(weather_tool))
            .build()?,
    );

    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP_NAME.into(),
            user_id: "demo-user".into(),
            session_id: Some("demo-session".into()),
            state: HashMap::new(),
        })
        .await?;

    let runner =
        Runner::builder().app_name(APP_NAME).agent(agent).session_service(sessions).build()?;

    for prompt in
        ["What's the weather in Nairobi? Use your tool.", "Reply with one fun fact about Rust."]
    {
        println!("\n>>> {prompt}");
        let mut stream = runner
            .run(
                UserId::new("demo-user")?,
                SessionId::new("demo-session")?,
                adk_core::Content::new("user").with_text(prompt),
            )
            .await?;

        while let Some(event) = stream.next().await {
            let event = event?;
            for part in &event.llm_response.content.iter().flat_map(|c| c.parts.clone()).collect::<Vec<_>>()
            {
                if let adk_core::Part::Text { text } = part
                    && !text.is_empty()
                {
                    print!("{text}");
                }
            }
        }
        println!();
    }

    // Make sure every span is committed before we read the database back.
    exporter.flush().map_err(|e| anyhow::anyhow!("flush failed: {e}"))?;

    println!("\n=== trace summary from {DB_PATH} ===");
    let reader = SqliteTraceReader::open(DB_PATH).map_err(|e| anyhow::anyhow!("{e}"))?;
    let sessions = reader.sessions().map_err(|e| anyhow::anyhow!("{e}"))?;
    anyhow::ensure!(!sessions.is_empty(), "no sessions were exported to {DB_PATH}");

    let mut saw_llm_span = false;
    let mut saw_tool_span = false;
    for summary in &sessions {
        println!("session {} — {} spans", summary.session_id, summary.span_count);
        for span in
            reader.session_trace(&summary.session_id).map_err(|e| anyhow::anyhow!("{e}"))?
        {
            saw_llm_span |= span.span_name == "call_llm";
            saw_tool_span |= span.span_name.starts_with("execute_tool");
            println!(
                "  {:<28} {:>6.1} ms  (trace {})",
                span.span_name,
                span.duration_nanos() as f64 / 1_000_000.0,
                span.trace_id
            );
        }
    }

    anyhow::ensure!(saw_llm_span, "expected at least one call_llm span in the export");
    anyhow::ensure!(saw_tool_span, "expected at least one execute_tool span in the export");
    println!("\n✅ SQLite export validated: LLM and tool spans persisted and read back.");
    Ok(())
}
