//! # BigQuery Toolset Example
//!
//! Demonstrates the native BigQuery toolset from ADK-Rust v0.7.0 — listing
//! datasets, inspecting table schemas, and executing SQL queries via the
//! BigQuery API, all driven by an LLM agent.
//!
//! ## What This Shows
//!
//! - Creating a `BigQueryToolset` with a Google Cloud project ID
//! - Building an `LlmAgent` that uses BigQuery tools (`bigquery_list_datasets`,
//!   `bigquery_list_tables`, `bigquery_get_table_schema`, `bigquery_execute_sql`)
//! - Dry-run mode: prints what SQL queries and API calls would be made when no
//!   project is configured
//! - Live mode: executes real BigQuery API calls when `GOOGLE_CLOUD_PROJECT` is set
//!
//! ## Prerequisites
//!
//! - `GOOGLE_API_KEY` environment variable set (for the Gemini LLM provider)
//! - (Optional) `GOOGLE_CLOUD_PROJECT` for live mode
//! - (Optional) Google Cloud Application Default Credentials configured
//!
//! ## Run
//!
//! ```bash
//! cargo run --manifest-path examples/bigquery_toolset/Cargo.toml
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_core::{Content, Part, SessionId, Toolset, UserId};
use adk_model::GeminiModel;
use adk_runner::{Runner, RunnerConfig};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_tool::bigquery::BigQueryToolset;
use futures::StreamExt;
use tracing_subscriber::EnvFilter;

const APP_NAME: &str = "bigquery-toolset-example";

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
    println!("║  BigQuery Toolset — ADK-Rust v0.7.0      ║");
    println!("╚══════════════════════════════════════════╝\n");

    // Check for the LLM provider key (required for both modes).
    let api_key = require_env("GOOGLE_API_KEY")?;

    // -----------------------------------------------------------------------
    // Dry-run vs Live mode
    // -----------------------------------------------------------------------
    //
    // When GOOGLE_CLOUD_PROJECT is not set, the example runs in dry-run mode:
    // it prints what the BigQuery tools would do without making real API calls.
    //
    // When GOOGLE_CLOUD_PROJECT is set, the example creates a real
    // BigQueryToolset and runs the agent against the live BigQuery API.

    let dry_run = std::env::var("GOOGLE_CLOUD_PROJECT").is_err();

    if dry_run {
        // ---------------------------------------------------------------
        // Dry-run mode: describe the tools and what they would do
        // ---------------------------------------------------------------
        println!("⚠️  Running in dry-run mode (no GOOGLE_CLOUD_PROJECT set)");
        println!("   Set GOOGLE_CLOUD_PROJECT to run against the real BigQuery API.\n");

        println!("--- Available BigQuery Tools ---\n");
        println!("  1. bigquery_list_datasets");
        println!("     Lists all datasets in the Google Cloud project.");
        println!("     Parameters: project_id (optional, from toolset config)");
        println!("     Would call: BigQuery API datasets.list\n");

        println!("  2. bigquery_list_tables");
        println!("     Lists all tables in a specific dataset.");
        println!("     Parameters: dataset_id (string), project_id (optional)");
        println!("     Would call: BigQuery API tables.list\n");

        println!("  3. bigquery_get_table_schema");
        println!("     Retrieves the schema (columns, types) of a specific table.");
        println!("     Parameters: dataset_id (string), table_id (string), project_id (optional)");
        println!("     Would call: BigQuery API tables.get\n");

        println!("  4. bigquery_execute_sql");
        println!("     Executes a SQL query against BigQuery and returns results.");
        println!("     Parameters: query (string), project_id (optional)");
        println!("     Would call: BigQuery API jobs.query\n");

        println!("--- Simulated Agent Interaction ---\n");
        println!("  Agent prompt: \"Discover datasets, pick a table, inspect its schema,");
        println!("                  and run a sample query.\"\n");
        println!("  In live mode the agent would:");
        println!("    1. Call bigquery_list_datasets() to discover available datasets");
        println!("    2. Call bigquery_list_tables(dataset_id=<dataset>) to find tables");
        println!("    3. Call bigquery_get_table_schema(dataset_id=<dataset>, table_id=<table>)");
        println!("    4. Call bigquery_execute_sql(query=\"SELECT * FROM `dataset.table` LIMIT 5\")");
        println!("    5. Display and summarize the query results\n");

        println!("  To run in live mode:");
        println!("    export GOOGLE_CLOUD_PROJECT=your-gcp-project-id");
        println!("    # Ensure Application Default Credentials are configured:");
        println!("    # gcloud auth application-default login");
        println!("    cargo run --manifest-path examples/bigquery_toolset/Cargo.toml");
    } else {
        // ---------------------------------------------------------------
        // Live mode: create a real BigQueryToolset and run the agent
        // ---------------------------------------------------------------
        let project_id = std::env::var("GOOGLE_CLOUD_PROJECT").unwrap();
        println!("🔑 Running in live mode with project: {project_id}\n");

        // Create the BigQuery toolset with the project ID.
        // BigQueryToolset implements the Toolset trait and provides four tools:
        //   bigquery_list_datasets, bigquery_list_tables,
        //   bigquery_get_table_schema, bigquery_execute_sql
        let bq_toolset = BigQueryToolset::with_project(&project_id);

        // Create the Gemini model for the agent.
        let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash")?);

        // Build an LlmAgent with the BigQuery toolset and instructions that
        // demonstrate the full discovery-to-query workflow.
        let agent = Arc::new(
            LlmAgentBuilder::new("bigquery-assistant")
                .description("An assistant that explores and queries BigQuery datasets")
                .model(model)
                .toolset(Arc::new(bq_toolset) as Arc<dyn Toolset>)
                .instruction(format!(
                    "You are a BigQuery data assistant for project '{project_id}'.\n\n\
                     Your task:\n\
                     1. List all available datasets using bigquery_list_datasets.\n\
                     2. Pick the first dataset and list its tables using bigquery_list_tables.\n\
                     3. Pick the first table and inspect its schema using bigquery_get_table_schema.\n\
                     4. Write and execute a SQL query using bigquery_execute_sql that selects \
                        the first 5 rows from that table.\n\
                     5. Summarize the results in a clear, readable format.\n\n\
                     Be concise and show the data clearly."
                ))
                .build()?,
        );

        // Create a runner and execute the agent.
        let runner = make_runner(agent, "bigquery-session").await?;

        println!("--- Running BigQuery Agent ---\n");

        let mut stream = runner
            .run(
                UserId::new("user")?,
                SessionId::new("bigquery-session")?,
                Content::new("user").with_text(format!(
                    "Explore the datasets in project '{project_id}': list datasets, \
                     pick a table, inspect its schema, run a sample query, and show me \
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

    println!("\n✅ BigQuery Toolset example completed successfully.");
    Ok(())
}
