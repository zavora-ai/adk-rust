//! # Spanner Toolset Example
//!
//! Demonstrates the native Spanner toolset from ADK-Rust v0.7.0 — listing
//! tables, inspecting table schemas, and executing SQL queries via the
//! Cloud Spanner API, all driven by an LLM agent.
//!
//! ## What This Shows
//!
//! - Creating a `SpannerToolset` with a project ID, instance ID, and database ID
//! - Building an `LlmAgent` that uses Spanner tools (`spanner_list_tables`,
//!   `spanner_get_table_schema`, `spanner_execute_sql`)
//! - Dry-run mode: prints what SQL queries and API calls would be made when no
//!   project is configured
//! - Live mode: executes real Spanner API calls when `SPANNER_PROJECT_ID` is set
//!
//! ## Prerequisites
//!
//! - `GOOGLE_API_KEY` environment variable set (for the Gemini LLM provider)
//! - (Optional) `SPANNER_PROJECT_ID`, `SPANNER_INSTANCE_ID`, `SPANNER_DATABASE_ID`
//!   for live mode
//! - (Optional) Google Cloud Application Default Credentials configured
//!
//! ## Run
//!
//! ```bash
//! cargo run --manifest-path examples/spanner_toolset/Cargo.toml
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_core::{Content, Part, SessionId, Toolset, UserId};
use adk_model::GeminiModel;
use adk_runner::{Runner, RunnerConfig};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_tool::spanner::SpannerToolset;
use futures::StreamExt;
use tracing_subscriber::EnvFilter;

const APP_NAME: &str = "spanner-toolset-example";

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
    println!("║  Spanner Toolset — ADK-Rust v0.7.0       ║");
    println!("╚══════════════════════════════════════════╝\n");

    // Check for the LLM provider key (required for both modes).
    let api_key = require_env("GOOGLE_API_KEY")?;

    // -----------------------------------------------------------------------
    // Dry-run vs Live mode
    // -----------------------------------------------------------------------
    //
    // When SPANNER_PROJECT_ID is not set, the example runs in dry-run mode:
    // it prints what the Spanner tools would do without making real API calls.
    //
    // When SPANNER_PROJECT_ID is set (along with SPANNER_INSTANCE_ID and
    // SPANNER_DATABASE_ID), the example creates a real SpannerToolset and
    // runs the agent against the live Spanner API.

    let dry_run = std::env::var("SPANNER_PROJECT_ID").is_err();

    if dry_run {
        // ---------------------------------------------------------------
        // Dry-run mode: describe the tools and what they would do
        // ---------------------------------------------------------------
        println!("⚠️  Running in dry-run mode (no SPANNER_PROJECT_ID set)");
        println!("   Set SPANNER_PROJECT_ID, SPANNER_INSTANCE_ID, and SPANNER_DATABASE_ID");
        println!("   to run against the real Spanner API.\n");

        println!("--- Available Spanner Tools ---\n");
        println!("  1. spanner_list_tables");
        println!("     Lists all tables in the Spanner database.");
        println!("     Parameters: none (uses toolset config)");
        println!("     Would call: Spanner API — INFORMATION_SCHEMA.TABLES query\n");

        println!("  2. spanner_get_table_schema");
        println!("     Retrieves the schema (columns, types) of a specific table.");
        println!("     Parameters: table_name (string)");
        println!("     Would call: Spanner API — INFORMATION_SCHEMA.COLUMNS query\n");

        println!("  3. spanner_execute_sql");
        println!("     Executes a SQL query against the Spanner database and returns results.");
        println!("     Parameters: query (string)");
        println!("     Would call: Spanner API — executeSql\n");

        println!("--- Simulated Agent Interaction ---\n");
        println!("  Agent prompt: \"List tables, pick one, inspect its schema,");
        println!("                  and run a sample query.\"\n");
        println!("  In live mode the agent would:");
        println!("    1. Call spanner_list_tables() to discover available tables");
        println!("    2. Pick a table and call spanner_get_table_schema(table_name=<table>)");
        println!("    3. Call spanner_execute_sql(query=\"SELECT * FROM <table> LIMIT 5\")");
        println!("    4. Display and summarize the query results\n");

        println!("  To run in live mode:");
        println!("    export SPANNER_PROJECT_ID=your-gcp-project-id");
        println!("    export SPANNER_INSTANCE_ID=your-instance-id");
        println!("    export SPANNER_DATABASE_ID=your-database-id");
        println!("    # Ensure Application Default Credentials are configured:");
        println!("    # gcloud auth application-default login");
        println!("    cargo run --manifest-path examples/spanner_toolset/Cargo.toml");
    } else {
        // ---------------------------------------------------------------
        // Live mode: create a real SpannerToolset and run the agent
        // ---------------------------------------------------------------
        let project_id = std::env::var("SPANNER_PROJECT_ID").unwrap();
        let instance_id = std::env::var("SPANNER_INSTANCE_ID")
            .expect("SPANNER_INSTANCE_ID required when SPANNER_PROJECT_ID is set");
        let database_id = std::env::var("SPANNER_DATABASE_ID")
            .expect("SPANNER_DATABASE_ID required when SPANNER_PROJECT_ID is set");

        println!(
            "🔑 Running in live mode with project: {project_id}, \
             instance: {instance_id}, database: {database_id}\n"
        );

        // Create the Spanner toolset with project, instance, and database IDs.
        // SpannerToolset implements the Toolset trait and provides three tools:
        //   spanner_list_tables, spanner_get_table_schema, spanner_execute_sql
        let spanner_toolset = SpannerToolset::new(&project_id, &instance_id, &database_id);

        // Create the Gemini model for the agent.
        let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash")?);

        // Build an LlmAgent with the Spanner toolset and instructions that
        // demonstrate the full discovery-to-query workflow.
        let agent = Arc::new(
            LlmAgentBuilder::new("spanner-assistant")
                .description("An assistant that explores and queries Cloud Spanner databases")
                .model(model)
                .toolset(Arc::new(spanner_toolset) as Arc<dyn Toolset>)
                .instruction(format!(
                    "You are a Cloud Spanner data assistant for project '{project_id}', \
                     instance '{instance_id}', database '{database_id}'.\n\n\
                     Your task:\n\
                     1. List all available tables using spanner_list_tables.\n\
                     2. Pick the first table and inspect its schema using \
                        spanner_get_table_schema.\n\
                     3. Write and execute a SQL query using spanner_execute_sql that selects \
                        the first 5 rows from that table.\n\
                     4. Summarize the results in a clear, readable format.\n\n\
                     Be concise and show the data clearly."
                ))
                .build()?,
        );

        // Create a runner and execute the agent.
        let runner = make_runner(agent, "spanner-session").await?;

        println!("--- Running Spanner Agent ---\n");

        let mut stream = runner
            .run(
                UserId::new("user")?,
                SessionId::new("spanner-session")?,
                Content::new("user").with_text(format!(
                    "Explore the tables in Spanner database '{database_id}' \
                     (instance '{instance_id}', project '{project_id}'): list tables, \
                     pick one, inspect its schema, run a sample query, and show me \
                     the results."
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

    println!("\n✅ Spanner Toolset example completed successfully.");
    Ok(())
}
