//! # YAML Agent Definition Example
//!
//! Demonstrates the YAML agent definition loading feature from ADK-Rust v0.7.0.
//!
//! ## What This Shows
//! - Loading a single agent from a YAML file with `AgentConfigLoader::load_file()`
//! - Loading a directory of agents with cross-references via `AgentConfigLoader::load_directory()`
//! - Validation error handling for malformed YAML definitions
//! - Creating a custom `ModelFactory` and `ToolRegistry` for the loader
//!
//! ## Prerequisites
//! - `GOOGLE_API_KEY` environment variable set (for Gemini model creation)
//!
//! ## Run
//! ```bash
//! cargo run --manifest-path examples/yaml_agent/Cargo.toml
//! ```

use std::path::Path;
use std::sync::Arc;

use adk_core::{Llm, Tool, ToolRegistry};
use adk_model::GeminiModel;
use adk_server::yaml_agent::{AgentConfigLoader, ModelFactory};
use async_trait::async_trait;
use tracing_subscriber::EnvFilter;

// ---------------------------------------------------------------------------
// Helper: require an environment variable with a descriptive error
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
// ModelFactory: creates LLM instances from provider + model_id
// ---------------------------------------------------------------------------

/// A simple model factory that creates Gemini models using the API key
/// from the environment. In a production application you would support
/// multiple providers here.
struct EnvModelFactory {
    api_key: String,
}

#[async_trait]
impl ModelFactory for EnvModelFactory {
    async fn create_model(
        &self,
        provider: &str,
        model_id: &str,
    ) -> adk_core::Result<Arc<dyn Llm>> {
        match provider {
            "gemini" => {
                let model = GeminiModel::new(&self.api_key, model_id).map_err(|e| {
                    adk_core::AdkError::config(format!(
                        "failed to create Gemini model '{model_id}': {e}"
                    ))
                })?;
                Ok(Arc::new(model))
            }
            other => Err(adk_core::AdkError::config(format!(
                "unsupported model provider '{other}'. This example only supports 'gemini'."
            ))),
        }
    }
}

// ---------------------------------------------------------------------------
// ToolRegistry: empty registry for this example (no custom tools)
// ---------------------------------------------------------------------------

/// An empty tool registry — this example does not register any custom tools.
/// The YAML agent definitions in `agents/` don't reference tools, so an
/// empty registry is sufficient.
struct EmptyToolRegistry;

impl ToolRegistry for EmptyToolRegistry {
    fn resolve(&self, _tool_name: &str) -> Option<Arc<dyn Tool>> {
        None
    }

    fn available_tools(&self) -> Vec<String> {
        vec![]
    }
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
    println!("║  YAML Agent Definition — ADK-Rust v0.7.0 ║");
    println!("╚══════════════════════════════════════════╝\n");

    // Read the API key from the environment. The ModelFactory uses it
    // to create Gemini model instances when loading YAML definitions.
    let api_key = require_env("GOOGLE_API_KEY")?;

    // Create the model factory and tool registry that the loader needs.
    let model_factory: Arc<dyn ModelFactory> = Arc::new(EnvModelFactory { api_key });
    let tool_registry: Arc<dyn ToolRegistry> = Arc::new(EmptyToolRegistry);

    // Construct the loader — it caches agents by name so that sub-agent
    // cross-references can be resolved across files.
    let loader = AgentConfigLoader::new(tool_registry, model_factory);

    // ------------------------------------------------------------------
    // Section 1: Load a single YAML file
    // ------------------------------------------------------------------
    println!("── Section 1: Load a single YAML file ─────────────────────\n");

    let researcher_path = Path::new("examples/yaml_agent/agents/researcher.yaml");
    let researcher = loader.load_file(researcher_path).await?;

    println!("  Loaded agent: {}", researcher.name());
    println!("  Description:  {}", researcher.description());
    println!("  Sub-agents:   {}", researcher.sub_agents().len());
    println!();

    // ------------------------------------------------------------------
    // Section 2: Load a directory with parent/sub-agent cross-references
    // ------------------------------------------------------------------
    println!("── Section 2: Load directory with cross-references ────────\n");

    // load_directory() reads all .yaml/.yml files, resolves sub-agent
    // references (assistant.yaml references researcher), and returns
    // all agents in dependency order.
    let agents_dir = Path::new("examples/yaml_agent/agents");
    let agents = loader.load_directory(agents_dir).await?;

    println!("  Loaded {} agents from directory:\n", agents.len());
    for agent in &agents {
        let sub_names: Vec<&str> = agent.sub_agents().iter().map(|a| a.name()).collect();
        println!("    • {} — {}", agent.name(), agent.description());
        if !sub_names.is_empty() {
            println!("      sub-agents: {}", sub_names.join(", "));
        }
    }
    println!();

    // ------------------------------------------------------------------
    // Section 3: Validation error handling
    // ------------------------------------------------------------------
    println!("── Section 3: Validation error handling ───────────────────\n");

    // Demonstrate what happens when a malformed YAML string is loaded.
    // We write a temporary file with an invalid definition (missing
    // required 'model' field) and attempt to load it.
    let temp_dir = tempfile::tempdir()?;
    let bad_yaml = "name: broken_agent\ndescription: missing model field\n";
    let bad_path = temp_dir.path().join("bad_agent.yaml");
    tokio::fs::write(&bad_path, bad_yaml).await?;

    match loader.load_file(&bad_path).await {
        Ok(_) => println!("  (unexpectedly succeeded)"),
        Err(e) => {
            println!("  Expected validation error:");
            println!("  {e}\n");
        }
    }

    // Also demonstrate an invalid temperature value
    let bad_temp_yaml = r#"
name: bad_temp_agent
model:
  provider: gemini
  model_id: gemini-2.0-flash
  temperature: 5.0
"#;
    let bad_temp_path = temp_dir.path().join("bad_temp.yaml");
    tokio::fs::write(&bad_temp_path, bad_temp_yaml).await?;

    match loader.load_file(&bad_temp_path).await {
        Ok(_) => println!("  (unexpectedly succeeded)"),
        Err(e) => {
            println!("  Expected temperature validation error:");
            println!("  {e}\n");
        }
    }

    println!("✅ YAML Agent Definition example completed successfully.");
    Ok(())
}
