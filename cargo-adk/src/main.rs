//! # cargo-adk
//!
//! Scaffolding and deployment tool for ADK-Rust agent projects.
//!
//! ```bash
//! cargo install cargo-adk
//!
//! cargo adk new my-agent                    # basic Gemini agent
//! cargo adk new my-agent --template rag     # RAG agent with vector search
//! cargo adk new my-agent --template tools   # agent with custom tools
//! cargo adk new my-agent --template api     # REST-deployable agent
//! cargo adk new my-agent --template openai  # OpenAI-powered agent
//! cargo adk deploy                          # deploy to platform
//! ```

use clap::{Parser, Subcommand};
use std::fs;
use std::path::Path;

const ADK_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser)]
#[command(name = "cargo-adk", bin_name = "cargo")]
struct Cargo {
    #[command(subcommand)]
    command: CargoSubcommand,
}

#[derive(Subcommand)]
enum CargoSubcommand {
    /// ADK-Rust agent scaffolding and deployment
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

    /// Deploy the agent to the ADK platform
    Deploy {
        /// Target environment
        #[arg(long, default_value = "production")]
        environment: String,

        /// Auth token (or set ADK_DEPLOY_TOKEN env var)
        #[arg(long, env = "ADK_DEPLOY_TOKEN")]
        token: Option<String>,

        /// Server URL
        #[arg(long, default_value = "http://127.0.0.1:8090")]
        server: String,

        /// Skip the cargo build step (use existing binary)
        #[arg(long)]
        skip_build: bool,

        /// Validate everything without actually pushing (useful for CI)
        #[arg(long)]
        dry_run: bool,
    },
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
        AdkCommand::Deploy { environment, token, server, skip_build, dry_run } => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("failed to create tokio runtime");

            if let Err(e) = rt.block_on(run_deploy(environment, token, server, skip_build, dry_run))
            {
                eprintln!("Error: {e}");
                std::process::exit(1);
            }
        }
    }
}

// ── Deploy command ──────────────────────────────────────────────

async fn run_deploy(
    environment: String,
    token: Option<String>,
    server: String,
    skip_build: bool,
    dry_run: bool,
) -> Result<(), String> {
    use adk_deploy::{
        DeployClient, DeployClientConfig, DeploymentManifest, LoginRequest, PushDeploymentRequest,
        SecretSetRequest,
    };
    use sha2::{Digest, Sha256};

    let manifest_path = Path::new("adk-deploy.toml");
    let manifest = DeploymentManifest::from_path(manifest_path)
        .map_err(|e| format!("failed to load manifest: {e}"))?;

    let binary_name = manifest.agent.binary.clone();

    println!("Deploying agent: {}", manifest.agent.name);
    println!("  version:     {}", manifest.agent.version);
    println!("  environment: {environment}");
    println!("  server:      {server}");
    println!();

    // ── Authenticate ────────────────────────────────────────────
    println!("Authenticating...");
    let mut config =
        DeployClientConfig { endpoint: server.clone(), token: token.clone(), workspace_id: None };

    // Try loading cached config for workspace_id and token fallback
    if let Ok(cached) = DeployClientConfig::load() {
        if config.token.is_none() && cached.token.is_some() && cached.endpoint == server {
            config.token = cached.token;
            println!("  Using cached credentials");
        }
        if config.workspace_id.is_none() {
            config.workspace_id = cached.workspace_id;
        }
    }

    let mut client = DeployClient::new(config.clone());

    // If we have a token, use it directly. Otherwise, login.
    if let Some(ref token_value) = config.token {
        client = client.with_token(token_value.clone());
        println!("  Using provided token");
    } else {
        // Attempt ephemeral login
        println!("  No token provided. Attempting login...");
        let email = std::env::var("ADK_DEPLOY_EMAIL").unwrap_or_else(|_| "cli@local".to_string());
        let login_response = client
            .login_ephemeral(&LoginRequest { email, workspace_name: None })
            .await
            .map_err(|e| format!("login failed: {e}. Provide --token or set ADK_DEPLOY_TOKEN"))?;
        config.workspace_id = Some(login_response.workspace_id.clone());
        println!("  Logged in to workspace: {}", login_response.workspace_id);
    }
    println!();

    // ── Build ───────────────────────────────────────────────────
    if !skip_build {
        println!("Building release binary...");
        let status = std::process::Command::new("cargo")
            .args(["build", "--release"])
            .status()
            .map_err(|e| format!("failed to run cargo build: {e}"))?;

        if !status.success() {
            return Err("cargo build --release failed".to_string());
        }
        println!("  Build complete.");
        println!();
    }

    // Locate the compiled binary
    let binary_path = Path::new("target/release").join(&binary_name);
    if !binary_path.exists() {
        return Err(format!(
            "binary not found at '{}'. Run without --skip-build or check agent.binary in manifest.",
            binary_path.display()
        ));
    }

    // ── Upload secrets from .env ────────────────────────────────
    // Convention: UPPER_SNAKE_CASE env vars map to lower-kebab-case secret keys.
    // Example: GOOGLE_API_KEY in .env → google-api-key secret on the platform.
    let declared_secrets: Vec<&str> = manifest.secrets.iter().map(|s| s.key.as_str()).collect();
    if !declared_secrets.is_empty() {
        let env_path = Path::new(".env");
        if env_path.exists() {
            println!("Uploading secrets...");
            let env_content =
                fs::read_to_string(env_path).map_err(|e| format!("failed to read .env: {e}"))?;

            let mut uploaded = 0;
            for line in env_content.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                if let Some((key, value)) = line.split_once('=') {
                    let key = key.trim();
                    let value = value.trim().trim_matches('"').trim_matches('\'');
                    let secret_key = key.to_lowercase().replace('_', "-");
                    if declared_secrets.contains(&secret_key.as_str()) {
                        if dry_run {
                            println!("  [dry-run] would upload secret ({} chars)", value.len());
                        } else {
                            client
                                .set_secret(&SecretSetRequest {
                                    environment: environment.clone(),
                                    key: secret_key.clone(),
                                    value: value.to_string(),
                                })
                                .await
                                .map_err(|e| format!("failed to set secret: {e}"))?;
                            println!("  ✓ uploaded secret");
                        }
                        uploaded += 1;
                    }
                }
            }
            if uploaded == 0 {
                println!(
                    "  No matching secrets found in .env for {} declared secret(s).",
                    declared_secrets.len()
                );
            }
            println!();
        } else {
            println!(
                "Note: manifest declares {} secret(s) but no .env file found.",
                declared_secrets.len()
            );
            println!("      Set secrets manually or create a .env file.");
            println!();
        }
    }

    // ── Create bundle ───────────────────────────────────────────
    println!("Creating deployment bundle...");
    let dist_dir = Path::new(".adk-deploy/dist");
    fs::create_dir_all(dist_dir).map_err(|e| format!("failed to create dist dir: {e}"))?;

    let bundle_filename = format!("{}-{}.tar.gz", manifest.agent.name, manifest.agent.version);
    let bundle_path = dist_dir.join(&bundle_filename);

    create_bundle(&bundle_path, manifest_path, &binary_path, &binary_name)?;

    // Compute SHA-256 checksum
    let bundle_bytes = fs::read(&bundle_path).map_err(|e| format!("failed to read bundle: {e}"))?;
    let bundle_size = bundle_bytes.len();
    let mut hasher = Sha256::new();
    hasher.update(&bundle_bytes);
    let checksum = hex::encode(hasher.finalize());

    println!("  bundle:   {}", bundle_path.display());
    println!("  size:     {:.1} MB", bundle_size as f64 / 1_048_576.0);
    println!("  checksum: {checksum}");
    println!();

    // ── Push deployment ─────────────────────────────────────────
    if dry_run {
        println!("Dry run complete. Would push:");
        println!("  bundle:       {}", bundle_path.display());
        println!("  size:         {:.1} MB", bundle_size as f64 / 1_048_576.0);
        println!("  environment:  {environment}");
        println!("  workspace_id: {:?}", config.workspace_id);
        println!("\nNo changes were made to the server.");
        return Ok(());
    }

    println!("Pushing bundle ({:.1} MB)...", bundle_size as f64 / 1_048_576.0);

    let request = PushDeploymentRequest {
        workspace_id: config.workspace_id.clone(),
        environment,
        manifest,
        bundle_path: bundle_path.to_string_lossy().to_string(),
        checksum_sha256: checksum,
        binary_path: Some(format!("bin/{binary_name}")),
    };

    let response = client
        .push_deployment(&request)
        .await
        .map_err(|e| format!("deployment push failed: {e}"))?;

    println!();
    println!("Deployment successful!");
    println!("  id:       {}", response.deployment.id);
    println!("  version:  {}", response.deployment.version);
    println!("  status:   {:?}", response.deployment.status);
    println!("  endpoint: {}", response.deployment.endpoint_url);

    Ok(())
}

/// Create a .tar.gz bundle with paths that have NO `./` prefix.
fn create_bundle(
    bundle_path: &Path,
    manifest_path: &Path,
    binary_path: &Path,
    binary_name: &str,
) -> Result<(), String> {
    use flate2::Compression;
    use flate2::write::GzEncoder;

    let file =
        fs::File::create(bundle_path).map_err(|e| format!("failed to create bundle file: {e}"))?;
    let encoder = GzEncoder::new(file, Compression::default());
    let mut archive = tar::Builder::new(encoder);

    // Add adk-deploy.toml at the root (bare path, no ./ prefix)
    let manifest_bytes =
        fs::read(manifest_path).map_err(|e| format!("failed to read manifest: {e}"))?;
    let mut header = tar::Header::new_gnu();
    header.set_size(manifest_bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    archive
        .append_data(&mut header, "adk-deploy.toml", manifest_bytes.as_slice())
        .map_err(|e| format!("failed to add manifest to bundle: {e}"))?;

    // Add binary at bin/{binary_name} (bare path, no ./ prefix)
    let binary_bytes = fs::read(binary_path).map_err(|e| format!("failed to read binary: {e}"))?;
    let mut header = tar::Header::new_gnu();
    header.set_size(binary_bytes.len() as u64);
    header.set_mode(0o755);
    header.set_cksum();
    let bin_path = format!("bin/{binary_name}");
    archive
        .append_data(&mut header, &bin_path, binary_bytes.as_slice())
        .map_err(|e| format!("failed to add binary to bundle: {e}"))?;

    archive.finish().map_err(|e| format!("failed to finalize bundle: {e}"))?;

    Ok(())
}

// ── Scaffolding commands ────────────────────────────────────────

fn print_templates() {
    println!("Available templates:\n");
    println!("  basic    Basic LLM agent with interactive console (default)");
    println!("  tools    Agent with custom function tools using #[tool] macro");
    println!("  rag      RAG agent with document ingestion and vector search");
    println!("  api      REST API server with health check and A2A protocol");
    println!("  openai   OpenAI-powered agent (gpt-5-mini)");
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
        _ => {
            return Err(format!(
                "unknown template '{template}'. Run `cargo adk templates` to see options"
            ));
        }
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

fn provider_features(provider: &str) -> Vec<&'static str> {
    match provider {
        "openai" => vec!["agents", "models", "openai", "runner", "sessions"],
        "anthropic" => vec!["agents", "models", "anthropic", "runner", "sessions"],
        _ => vec!["minimal"],
    }
}

fn adk_rust_dep(features: &[&str]) -> String {
    format!(
        r#"adk-rust = {{ version = "{ADK_VERSION}", default-features = false, features = [{}] }}"#,
        features.iter().map(|feature| format!(r#""{feature}""#)).collect::<Vec<_>>().join(", ")
    )
}

fn provider_dep(provider: &str) -> (String, &str, &str) {
    // Returns (feature_flags, model_constructor, env_var)
    match provider {
        "openai" => (
            adk_rust_dep(&provider_features(provider)),
            r#"let model = adk_rust::model::openai::OpenAIClient::new(
        adk_rust::model::openai::OpenAIConfig::new(&api_key, "gpt-5-mini"),
    )?;"#,
            "OPENAI_API_KEY",
        ),
        "anthropic" => (
            adk_rust_dep(&provider_features(provider)),
            r#"let model = adk_rust::model::anthropic::AnthropicClient::new(
        adk_rust::model::anthropic::AnthropicConfig::new(&api_key, "claude-sonnet-4-5-20250929"),
    )?;"#,
            "ANTHROPIC_API_KEY",
        ),
        _ => (
            adk_rust_dep(&provider_features("gemini")),
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
adk-tool = "{ADK_VERSION}"
tokio = {{ version = "1", features = ["full"] }}
dotenvy = "0.15"
anyhow = "1"
serde = {{ version = "1", features = ["derive"] }}
serde_json = "1"
schemars = "1"
"#
    );

    let main = format!(
        r#"use adk_rust::prelude::*;
use adk_rust::Launcher;
use adk_tool::{{tool, AdkError}};
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
async fn greet(args: GreetArgs) -> std::result::Result<Value, AdkError> {{
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
        adk_rust_dep(&["agents", "models", "gemini", "runner", "sessions", "rag"])
    } else {
        adk_rust_dep(&["agents", "models", provider, "runner", "sessions", "rag"])
    };

    let cargo = format!(
        r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2024"

[dependencies]
{dep}
adk-rag = {{ version = "{ADK_VERSION}", features = ["gemini"] }}
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

    let env =
        format!("{env_var}=your-api-key-here\nGOOGLE_API_KEY=your-gemini-key-for-embeddings\n");
    (cargo, main, env)
}

fn generate_api(name: &str, provider: &str) -> (String, String, String) {
    let (_, model_code, env_var) = provider_dep(provider);
    let dep = if provider == "gemini" {
        adk_rust_dep(&["agents", "models", "gemini", "runner", "sessions", "server"])
    } else {
        adk_rust_dep(&["agents", "models", provider, "runner", "sessions", "server"])
    };

    let cargo = format!(
        r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2024"

[dependencies]
{dep}
axum = "0.8"
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

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_current_template(cargo_toml: &str) {
        assert!(
            cargo_toml.contains(&format!(r#"version = "{ADK_VERSION}""#)),
            "template must use the cargo-adk package version"
        );
        assert!(
            !cargo_toml.contains("0.4") && !cargo_toml.contains("standard"),
            "template should not use stale versions or the heavy standard preset"
        );
    }

    #[test]
    fn basic_templates_use_current_lean_dependencies() {
        for provider in ["gemini", "openai", "anthropic"] {
            let (cargo_toml, _, _) = generate_basic("assistant", provider);
            assert_current_template(&cargo_toml);
            assert!(cargo_toml.contains("default-features = false"));
        }
    }

    #[test]
    fn tool_template_uses_schemars_one_and_current_adk_tool() {
        let (cargo_toml, _, _) = generate_tools("toolbox", "gemini");
        assert_current_template(&cargo_toml);
        assert!(cargo_toml.contains(&format!(r#"adk-tool = "{ADK_VERSION}""#)));
        assert!(cargo_toml.contains(r#"schemars = "1""#));
    }

    #[test]
    fn rag_and_api_templates_use_current_versions() {
        for generator in [generate_rag, generate_api] {
            let (cargo_toml, _, _) = generator("assistant", "gemini");
            assert_current_template(&cargo_toml);
        }
    }

    #[test]
    fn bundle_has_no_dot_slash_prefix() {
        // Create a temp directory with a fake manifest and binary
        let tmp = std::env::temp_dir().join("cargo-adk-test-bundle");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let manifest_path = tmp.join("adk-deploy.toml");
        fs::write(&manifest_path, b"[agent]\nname = \"test\"\nbinary = \"test\"\n").unwrap();

        let binary_path = tmp.join("test-binary");
        fs::write(&binary_path, b"fake-binary-content").unwrap();

        let bundle_path = tmp.join("test-bundle.tar.gz");
        create_bundle(&bundle_path, &manifest_path, &binary_path, "test-binary").unwrap();

        // Read back and verify paths
        let file = fs::File::open(&bundle_path).unwrap();
        let decoder = flate2::read::GzDecoder::new(file);
        let mut archive = tar::Archive::new(decoder);

        let mut paths: Vec<String> = Vec::new();
        for entry in archive.entries().unwrap() {
            let entry = entry.unwrap();
            paths.push(entry.path().unwrap().to_string_lossy().to_string());
        }

        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0], "adk-deploy.toml");
        assert_eq!(paths[1], "bin/test-binary");

        // Verify no ./ prefix
        for path in &paths {
            assert!(!path.starts_with("./"), "path should not start with ./: {path}");
        }

        // Cleanup
        let _ = fs::remove_dir_all(&tmp);
    }
}
