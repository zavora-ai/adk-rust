//! # cargo-adk
//!
//! Scaffolding tool for ADK-Rust agent projects.
//!
//! ```bash
//! cargo install cargo-adk
//!
//! cargo adk new my-agent                    # basic Gemini agent
//! cargo adk new my-agent --template rag     # RAG agent with vector search
//! cargo adk new my-agent --template tools   # agent with custom tools
//! cargo adk new my-agent --template api     # REST-deployable agent
//! cargo adk new my-agent --template openai  # OpenAI-powered agent
//! ```

use clap::{Parser, Subcommand};
use std::fs;
use std::path::Path;

#[derive(Parser)]
#[command(name = "cargo-adk", bin_name = "cargo")]
struct Cargo {
    #[command(subcommand)]
    command: CargoSubcommand,
}

#[derive(Subcommand)]
enum CargoSubcommand {
    /// ADK-Rust agent scaffolding
    Adk(AdkCli),
}

#[derive(Parser)]
struct AdkCli {
    #[command(subcommand)]
    command: AdkCommand,
}

#[derive(Subcommand)]
enum AdkCommand {
    /// Create a new ADK agent project
    New {
        /// Project name (used for directory and crate name)
        name: String,

        /// Project template
        #[arg(short, long, default_value = "basic")]
        template: String,

        /// LLM provider to use
        #[arg(short, long, default_value = "gemini")]
        provider: String,
    },

    /// List available templates
    Templates,
}

fn main() {
    let cli = Cargo::parse();
    let CargoSubcommand::Adk(adk) = cli.command;

    match adk.command {
        AdkCommand::New { name, template, provider } => {
            if let Err(e) = create_project(&name, &template, &provider) {
                eprintln!("Error: {e}");
                std::process::exit(1);
            }
        }
        AdkCommand::Templates => {
            print_templates();
        }
    }
}

fn print_templates() {
    println!("Available templates:\n");
    println!("  basic    Basic LLM agent with interactive console (default)");
    println!("  tools    Agent with custom function tools using #[tool] macro");
    println!("  rag      RAG agent with document ingestion and vector search");
    println!("  api      REST API server with health check and A2A protocol");
    println!("  openai   OpenAI-powered agent (GPT-4o-mini)");
    println!("\nUsage: cargo adk new my-agent --template <template>");
}

fn create_project(name: &str, template: &str, provider: &str) -> Result<(), String> {
    let path = Path::new(name);
    if path.exists() {
        return Err(format!("directory '{name}' already exists"));
    }

    let (cargo_toml, main_rs, env_example) = match template {
        "basic" => generate_basic(name, provider),
        "tools" => generate_tools(name, provider),
        "rag" => generate_rag(name, provider),
        "api" => generate_api(name, provider),
        "openai" => generate_basic(name, "openai"),
        _ => return Err(format!("unknown template '{template}'. Run `cargo adk templates` to see options")),
    };

    // Create project structure
    fs::create_dir_all(path.join("src")).map_err(|e| e.to_string())?;
    fs::write(path.join("Cargo.toml"), cargo_toml).map_err(|e| e.to_string())?;
    fs::write(path.join("src/main.rs"), main_rs).map_err(|e| e.to_string())?;
    fs::write(path.join(".env.example"), env_example).map_err(|e| e.to_string())?;
    fs::write(path.join(".gitignore"), "/target\n.env\n").map_err(|e| e.to_string())?;

    println!("Created ADK agent project: {name}/");
    println!("  template: {template}");
    println!("  provider: {provider}");
    println!();
    println!("Next steps:");
    println!("  cd {name}");
    println!("  cp .env.example .env    # add your API key");
    println!("  cargo run");

    Ok(())
}

// ── Template generators ─────────────────────────────────────────

fn provider_dep(provider: &str) -> (&str, &str, &str) {
    // Returns (feature_flags, model_constructor, env_var)
    match provider {
        "openai" => (
            r#"adk-rust = { version = "0.4", default-features = false, features = ["standard", "openai"] }"#,
            r#"let model = adk_rust::model::openai::OpenAIClient::new(
        adk_rust::model::openai::OpenAIConfig::new(&api_key, "gpt-4o-mini"),
    )?;"#,
            "OPENAI_API_KEY",
        ),
        "anthropic" => (
            r#"adk-rust = { version = "0.4", default-features = false, features = ["standard", "anthropic"] }"#,
            r#"let model = adk_rust::model::anthropic::AnthropicClient::new(
        adk_rust::model::anthropic::AnthropicConfig::new(&api_key, "claude-sonnet-4-5-20250929"),
    )?;"#,
            "ANTHROPIC_API_KEY",
        ),
        _ => (
            r#"adk-rust = "0.4""#,
            r#"let model = adk_rust::model::GeminiModel::new(&api_key, "gemini-2.5-flash")?;"#,
            "GOOGLE_API_KEY",
        ),
    }
}

fn generate_basic(name: &str, provider: &str) -> (String, String, String) {
    let (dep, model_code, env_var) = provider_dep(provider);
    let cargo = format!(
        r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2024"

[dependencies]
{dep}
tokio = {{ version = "1", features = ["full"] }}
dotenvy = "0.15"
anyhow = "1"
"#
    );

    let main = format!(
        r#"use adk_rust::prelude::*;
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {{
    dotenvy::dotenv().ok();
    let api_key = std::env::var("{env_var}")?;

    {model_code}

    let agent = LlmAgentBuilder::new("{name}")
        .description("A helpful AI assistant")
        .instruction("You are a friendly assistant. Be concise and helpful.")
        .model(Arc::new(model))
        .build()?;

    Launcher::new(Arc::new(agent)).run().await?;
    Ok(())
}}
"#
    );

    let env = format!("{env_var}=your-api-key-here\n");
    (cargo, main, env)
}

fn generate_tools(name: &str, provider: &str) -> (String, String, String) {
    let (dep, model_code, env_var) = provider_dep(provider);
    let cargo = format!(
        r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2024"

[dependencies]
{dep}
adk-tool = "0.4"
tokio = {{ version = "1", features = ["full"] }}
dotenvy = "0.15"
anyhow = "1"
serde = {{ version = "1", features = ["derive"] }}
serde_json = "1"
schemars = "0.8"
"#
    );

    let main = format!(
        r#"use adk_rust::prelude::*;
use adk_rust::Launcher;
use adk_tool::tool;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{{json, Value}};
use std::sync::Arc;

#[derive(Deserialize, JsonSchema)]
struct GreetArgs {{
    /// Name of the person to greet
    name: String,
    /// Greeting style: formal or casual
    style: Option<String>,
}}

/// Greet a person by name.
#[tool]
async fn greet(args: GreetArgs) -> Result<Value, AdkError> {{
    let greeting = match args.style.as_deref() {{
        Some("formal") => format!("Good day, {{}}. How may I assist you?", args.name),
        _ => format!("Hey {{}}! What's up?", args.name),
    }};
    Ok(json!({{ "greeting": greeting }}))
}}

#[tokio::main]
async fn main() -> anyhow::Result<()> {{
    dotenvy::dotenv().ok();
    let api_key = std::env::var("{env_var}")?;

    {model_code}

    let agent = LlmAgentBuilder::new("{name}")
        .description("Assistant with custom tools")
        .instruction("You are a helpful assistant. Use the greet tool when asked to greet someone.")
        .model(Arc::new(model))
        .tool(Arc::new(Greet))
        .build()?;

    Launcher::new(Arc::new(agent)).run().await?;
    Ok(())
}}
"#
    );

    let env = format!("{env_var}=your-api-key-here\n");
    (cargo, main, env)
}

fn generate_rag(name: &str, provider: &str) -> (String, String, String) {
    let (_, model_code, env_var) = provider_dep(provider);
    // RAG always needs gemini for embeddings
    let dep = if provider == "gemini" {
        r#"adk-rust = { version = "0.4", features = ["rag"] }"#
    } else {
        &format!(r#"adk-rust = {{ version = "0.4", features = ["rag", "{provider}"] }}"#)
    };

    let cargo = format!(
        r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2024"

[dependencies]
{dep}
adk-rag = {{ version = "0.4", features = ["gemini"] }}
tokio = {{ version = "1", features = ["full"] }}
dotenvy = "0.15"
anyhow = "1"
serde_json = "1"
"#
    );

    let main = format!(
        r#"use adk_rust::prelude::*;
use adk_rust::Launcher;
use adk_rag::{{
    Document, FixedSizeChunker, GeminiEmbeddingProvider, InMemoryVectorStore,
    RagConfig, RagPipeline, RagTool,
}};
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {{
    dotenvy::dotenv().ok();
    let api_key = std::env::var("{env_var}")?;
    let gemini_key = std::env::var("GOOGLE_API_KEY").unwrap_or_else(|_| api_key.clone());

    // Build RAG pipeline
    let pipeline = Arc::new(
        RagPipeline::builder()
            .config(RagConfig::default())
            .embedding_provider(Arc::new(GeminiEmbeddingProvider::new(&gemini_key)?))
            .vector_store(Arc::new(InMemoryVectorStore::new()))
            .chunker(Arc::new(FixedSizeChunker::new(256, 50)))
            .build()?,
    );

    // Ingest sample documents
    pipeline.create_collection("docs").await?;
    pipeline.ingest("docs", &Document {{
        id: "example".into(),
        text: "ADK-Rust is a framework for building AI agents in Rust. \
               It supports multiple LLM providers, tool calling, RAG, and more.".into(),
        metadata: Default::default(),
        source_uri: None,
    }}).await?;

    println!("Ingested documents. Ask questions about your knowledge base.\\n");

    {model_code}

    let agent = LlmAgentBuilder::new("{name}")
        .description("RAG-powered knowledge assistant")
        .instruction("Use the rag_search tool to find relevant documents before answering.")
        .model(Arc::new(model))
        .tool(Arc::new(RagTool::new(pipeline, "docs")))
        .build()?;

    Launcher::new(Arc::new(agent)).run().await?;
    Ok(())
}}
"#
    );

    let env = format!("{env_var}=your-api-key-here\nGOOGLE_API_KEY=your-gemini-key-for-embeddings\n");
    (cargo, main, env)
}

fn generate_api(name: &str, provider: &str) -> (String, String, String) {
    let (_, model_code, env_var) = provider_dep(provider);
    let dep = if provider == "gemini" {
        r#"adk-rust = { version = "0.4", features = ["full"] }"#
    } else {
        &format!(r#"adk-rust = {{ version = "0.4", features = ["full", "{provider}"] }}"#)
    };

    let cargo = format!(
        r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2024"

[dependencies]
{dep}
tokio = {{ version = "1", features = ["full"] }}
dotenvy = "0.15"
anyhow = "1"
"#
    );

    let main = format!(
        r#"use adk_rust::prelude::*;
use adk_rust::server::{{ServerConfig, create_app}};
use adk_rust::session::InMemorySessionService;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {{
    dotenvy::dotenv().ok();
    let api_key = std::env::var("{env_var}")?;

    {model_code}

    let agent: Arc<dyn Agent> = Arc::new(
        LlmAgentBuilder::new("{name}")
            .description("REST API agent")
            .instruction("You are a helpful assistant accessible via REST API.")
            .model(Arc::new(model))
            .build()?,
    );

    let session_service = Arc::new(InMemorySessionService::new());

    let config = ServerConfig::new(
        Arc::new(adk_rust::SingleAgentLoader::new(agent)),
        session_service,
    );
    let app = create_app(config);

    let port = std::env::var("PORT").unwrap_or_else(|_| "8080".to_string());
    let addr = format!("0.0.0.0:{{}}", port);
    println!("ADK agent server running on http://{{addr}}");
    println!("  POST /chat          — send messages");
    println!("  GET  /health        — health check");

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}}
"#
    );

    let env = format!("{env_var}=your-api-key-here\nPORT=8080\n");
    (cargo, main, env)
}
