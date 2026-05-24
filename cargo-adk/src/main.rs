//! # cargo-adk
//!
//! Scaffolding, validation, and deployment tool for ADK-Rust agent projects.
//!
//! ```bash
//! cargo install cargo-adk
//!
//! cargo adk new my-agent                    # basic Gemini agent
//! cargo adk new my-agent --template rag     # RAG agent with vector search
//! cargo adk new my-agent --template tools   # agent with custom tools
//! cargo adk new my-agent --template api     # REST-deployable agent
//! cargo adk new my-agent --template openai  # OpenAI-powered agent
//! cargo adk new my-agent --with-yaml        # also generate YAML agent definition
//! cargo adk new my-agent --output-dir /tmp  # create at specific path
//! cargo adk new my-agent --json-output      # structured JSON output
//! cargo adk templates --json                # list templates as JSON
//! cargo adk validate --yaml agent.yaml      # validate agent definition
//! cargo adk deploy                          # deploy to platform
//! cargo adk deploy --stream-output          # deploy with JSON event streaming
//! ```

use clap::{Parser, Subcommand};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

use cargo_adk::codegen::generate_project;
use cargo_adk::composition::{DryRunFile, DryRunOutput, resolve_composition};
use cargo_adk::registry::TemplateRegistry;

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

        /// Model ID to use (overrides provider default)
        #[arg(short, long)]
        model: Option<String>,

        /// Output directory (project created at <output-dir>/<name>/)
        #[arg(long)]
        output_dir: Option<PathBuf>,

        /// Never prompt for input; use defaults or fail with error
        #[arg(long)]
        non_interactive: bool,

        /// Emit structured JSON to stdout instead of human-readable text
        #[arg(long)]
        json_output: bool,

        /// Also generate a YAML agent definition alongside Rust source
        #[arg(long)]
        with_yaml: bool,

        /// Capability addons to include (repeatable)
        #[arg(long, action = clap::ArgAction::Append)]
        addon: Vec<String>,

        /// Preview what would be generated without writing files
        #[arg(long)]
        dry_run: bool,
    },

    /// List available templates
    Templates {
        /// Output as JSON (name, description, provider, features)
        #[arg(long)]
        json: bool,

        /// Custom template directory to include
        #[arg(long)]
        template_dir: Option<PathBuf>,
    },

    /// List available capability addons
    Addons {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Build the agent project (cargo build --release)
    Build {
        /// Path to Cargo.toml (defaults to current directory)
        #[arg(long)]
        manifest_path: Option<PathBuf>,

        /// Build in debug mode instead of release
        #[arg(long)]
        debug: bool,
    },

    /// Validate an agent definition without building or deploying
    Validate {
        /// Path to a YAML agent definition file
        #[arg(long)]
        yaml: Option<PathBuf>,

        /// Path to a Rust source file to syntax-check
        #[arg(long)]
        rust: Option<PathBuf>,
    },

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

        /// Scope the deployment to a specific workspace (multi-tenancy)
        #[arg(long)]
        workspace_id: Option<String>,

        /// Link the deployment to an existing agent record in the platform
        #[arg(long)]
        agent_id: Option<String>,

        /// Emit build/deploy progress as newline-delimited JSON events
        #[arg(long)]
        stream_output: bool,
    },
}

// ── JSON output types ───────────────────────────────────────────

#[derive(Serialize)]
struct NewProjectOutput {
    project_dir: String,
    template: String,
    provider: String,
    files_created: Vec<String>,
}

#[derive(Serialize)]
struct TemplateInfo {
    name: &'static str,
    description: &'static str,
    default_provider: &'static str,
    features: Vec<&'static str>,
}

#[derive(Serialize)]
struct ValidateOutput {
    valid: bool,
    warnings: Vec<String>,
    errors: Vec<String>,
}

#[derive(Serialize)]
struct DeployEvent {
    event: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    timestamp: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    percent: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    environment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    deployment_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<String>,
}

impl DeployEvent {
    fn new(event: &str) -> Self {
        Self {
            event: event.to_string(),
            timestamp: Some(chrono::Utc::now().to_rfc3339()),
            message: None,
            percent: None,
            duration_ms: None,
            environment: None,
            deployment_id: None,
            status: None,
        }
    }

    fn with_message(mut self, msg: &str) -> Self {
        self.message = Some(msg.to_string());
        self
    }

    fn emit(&self) {
        if let Ok(json) = serde_json::to_string(self) {
            println!("{json}");
        }
    }
}

// ── Main ────────────────────────────────────────────────────────

fn main() {
    let cli = Cargo::parse();
    let CargoSubcommand::Adk(adk) = cli.command;

    match adk.command {
        AdkCommand::New {
            name,
            template,
            provider,
            model,
            output_dir,
            non_interactive: _,
            json_output,
            with_yaml,
            addon,
            dry_run,
        } => {
            if let Err(e) = create_project(
                &name,
                &template,
                &provider,
                model.as_deref(),
                output_dir.as_deref(),
                json_output,
                with_yaml,
                &addon,
                dry_run,
            ) {
                if json_output {
                    let err = serde_json::json!({"error": e});
                    eprintln!("{err}");
                } else {
                    eprintln!("Error: {e}");
                }
                std::process::exit(1);
            }
        }
        AdkCommand::Templates { json, template_dir } => {
            if json {
                print_templates_json(template_dir.as_deref());
            } else {
                print_templates(template_dir.as_deref());
            }
        }
        AdkCommand::Addons { json } => {
            print_addons(json);
        }
        AdkCommand::Build { manifest_path, debug } => {
            if let Err(e) = handle_build(manifest_path, debug) {
                eprintln!("Error: {e}");
                std::process::exit(1);
            }
        }
        AdkCommand::Validate { yaml, rust } => {
            if let Err(e) = run_validate(yaml.as_deref(), rust.as_deref()) {
                eprintln!("Error: {e}");
                std::process::exit(1);
            }
        }
        AdkCommand::Deploy {
            environment,
            token,
            server,
            skip_build,
            dry_run,
            workspace_id,
            agent_id,
            stream_output,
        } => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("failed to create tokio runtime");

            if let Err(e) = rt.block_on(run_deploy(
                environment,
                token,
                server,
                skip_build,
                dry_run,
                workspace_id,
                agent_id,
                stream_output,
            )) {
                if stream_output {
                    DeployEvent::new("error").with_message(&e).emit();
                } else {
                    eprintln!("Error: {e}");
                }
                std::process::exit(1);
            }
        }
    }
}

// ── Build command ────────────────────────────────────────────────

fn handle_build(manifest_path: Option<PathBuf>, debug: bool) -> Result<(), String> {
    let mut cmd = std::process::Command::new("cargo");
    cmd.arg("build");

    if !debug {
        cmd.arg("--release");
    }

    if let Some(ref path) = manifest_path {
        cmd.arg("--manifest-path").arg(path);
    }

    let status = cmd.status().map_err(|e| format!("failed to run cargo build: {e}"))?;

    if status.success() {
        // Determine the target directory for reporting
        let profile_dir = if debug { "debug" } else { "release" };
        let target_dir = if let Some(ref path) = manifest_path {
            // If manifest_path is specified, target dir is relative to its parent
            let parent = Path::new(path).parent().unwrap_or(Path::new("."));
            parent.join("target").join(profile_dir)
        } else {
            PathBuf::from("target").join(profile_dir)
        };

        println!("✅ Build successful");
        println!("   profile: {profile_dir}");
        println!("   target:  {}", target_dir.display());

        // Try to find and report binary sizes
        if target_dir.exists() {
            if let Ok(entries) = fs::read_dir(&target_dir) {
                let mut found_binary = false;
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_file() {
                        // On Unix, check if executable; on all platforms, skip common non-binary extensions
                        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                        if ext == "d"
                            || ext == "rlib"
                            || ext == "rmeta"
                            || ext == "so"
                            || ext == "dylib"
                        {
                            continue;
                        }
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::PermissionsExt;
                            if let Ok(meta) = path.metadata() {
                                let mode = meta.permissions().mode();
                                if mode & 0o111 != 0 && ext.is_empty() {
                                    let size = meta.len();
                                    println!(
                                        "   binary:  {} ({:.1} MB)",
                                        path.display(),
                                        size as f64 / 1_048_576.0
                                    );
                                    found_binary = true;
                                }
                            }
                        }
                        #[cfg(not(unix))]
                        {
                            if ext == "exe" {
                                if let Ok(meta) = path.metadata() {
                                    let size = meta.len();
                                    println!(
                                        "   binary:  {} ({:.1} MB)",
                                        path.display(),
                                        size as f64 / 1_048_576.0
                                    );
                                    found_binary = true;
                                }
                            }
                        }
                    }
                }
                if !found_binary {
                    println!("   (no binaries found in {})", target_dir.display());
                }
            }
        }

        Ok(())
    } else {
        std::process::exit(status.code().unwrap_or(1));
    }
}

// ── Addons command ──────────────────────────────────────────────

#[derive(Serialize)]
struct AddonInfo {
    name: &'static str,
    description: &'static str,
    priority: u8,
    features: Vec<&'static str>,
}

fn get_builtin_addons() -> Vec<AddonInfo> {
    vec![
        AddonInfo {
            name: "telemetry",
            description: "OpenTelemetry tracing integration with console exporter",
            priority: 10,
            features: vec!["telemetry"],
        },
        AddonInfo {
            name: "auth",
            description: "Authentication middleware with API key and JWT support",
            priority: 20,
            features: vec!["auth"],
        },
        AddonInfo {
            name: "sessions",
            description: "Session management with configurable backend",
            priority: 30,
            features: vec!["sessions"],
        },
        AddonInfo {
            name: "memory",
            description: "Semantic memory integration with in-memory backend",
            priority: 40,
            features: vec!["memory"],
        },
        AddonInfo {
            name: "mcp",
            description: "MCP tool integration with example server connection",
            priority: 50,
            features: vec!["tools", "mcp"],
        },
        AddonInfo {
            name: "guardrails",
            description: "Input and output guardrail hooks with validation logic",
            priority: 60,
            features: vec!["guardrail"],
        },
        AddonInfo {
            name: "eval",
            description: "Evaluation harness with example test cases",
            priority: 70,
            features: vec!["eval"],
        },
        AddonInfo {
            name: "browser",
            description: "Browser automation tool integration",
            priority: 80,
            features: vec!["browser"],
        },
        AddonInfo {
            name: "server",
            description: "Axum HTTP server with health check and agent endpoints",
            priority: 90,
            features: vec!["server"],
        },
    ]
}

fn print_addons(json: bool) {
    let addons = get_builtin_addons();
    if json {
        println!("{}", serde_json::to_string_pretty(&addons).unwrap_or_default());
    } else {
        println!("Available capability addons:\n");
        for addon in &addons {
            println!("  {:<12} {}", addon.name, addon.description);
        }
        println!(
            "\nUsage: cargo adk new my-agent --template llm --addon <addon> [--addon <addon> ...]"
        );
    }
}

// ── Validate command ────────────────────────────────────────────

fn run_validate(yaml: Option<&Path>, rust: Option<&Path>) -> Result<(), String> {
    if yaml.is_none() && rust.is_none() {
        return Err("provide at least one of --yaml or --rust to validate".to_string());
    }

    let mut warnings = Vec::new();
    let mut errors = Vec::new();

    if let Some(yaml_path) = yaml {
        validate_yaml(yaml_path, &mut warnings, &mut errors)?;
    }

    if let Some(rust_path) = rust {
        validate_rust(rust_path, &mut warnings, &mut errors)?;
    }

    let valid = errors.is_empty();
    let output = ValidateOutput { valid, warnings: warnings.clone(), errors: errors.clone() };
    println!("{}", serde_json::to_string_pretty(&output).unwrap_or_default());

    if valid { Ok(()) } else { Err("validation failed".to_string()) }
}

fn validate_yaml(
    path: &Path,
    warnings: &mut Vec<String>,
    errors: &mut Vec<String>,
) -> Result<(), String> {
    if !path.exists() {
        errors.push(format!("file not found: {}", path.display()));
        return Ok(());
    }

    let content =
        fs::read_to_string(path).map_err(|e| format!("failed to read {}: {e}", path.display()))?;

    // Parse as YAML and validate structure
    let value: Result<serde_json::Value, _> = serde_yaml_ng::from_str(&content);
    match value {
        Err(e) => {
            errors.push(format!("YAML parse error: {e}"));
            return Ok(());
        }
        Ok(doc) => {
            // Check required fields
            if doc.get("name").and_then(|v| v.as_str()).is_none_or(|s| s.is_empty()) {
                errors.push("missing required field: name".to_string());
            }
            if doc.get("model").is_none() {
                errors.push("missing required field: model".to_string());
            } else {
                let model = &doc["model"];
                if model.get("provider").and_then(|v| v.as_str()).is_none_or(|s| s.is_empty()) {
                    errors.push("missing required field: model.provider".to_string());
                }
                if model.get("model_id").and_then(|v| v.as_str()).is_none_or(|s| s.is_empty()) {
                    errors.push("missing required field: model.model_id".to_string());
                }
                // Validate provider is known
                if let Some(provider) = model.get("provider").and_then(|v| v.as_str()) {
                    let known = [
                        "gemini",
                        "openai",
                        "anthropic",
                        "deepseek",
                        "groq",
                        "ollama",
                        "bedrock",
                        "azure-ai",
                    ];
                    if !known.contains(&provider) {
                        warnings.push(format!(
                            "unknown model provider: '{provider}'. Known providers: {}",
                            known.join(", ")
                        ));
                    }
                }
            }

            // Check tools have descriptions
            if let Some(tools) = doc.get("tools").and_then(|v| v.as_array()) {
                for (i, tool) in tools.iter().enumerate() {
                    if let Some(name) = tool.get("name").and_then(|v| v.as_str()) {
                        if tool.get("description").is_none() {
                            warnings.push(format!("tool '{name}' (index {i}) has no description"));
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

fn validate_rust(
    path: &Path,
    _warnings: &mut Vec<String>,
    errors: &mut Vec<String>,
) -> Result<(), String> {
    if !path.exists() {
        errors.push(format!("file not found: {}", path.display()));
        return Ok(());
    }

    // Run cargo check on the file's parent directory if it has a Cargo.toml
    let parent = path.parent().unwrap_or(Path::new("."));
    let cargo_toml = parent.join("Cargo.toml");

    if cargo_toml.exists() {
        let output = std::process::Command::new("cargo")
            .args(["check", "--message-format=json"])
            .current_dir(parent)
            .output()
            .map_err(|e| format!("failed to run cargo check: {e}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            for line in stderr.lines().take(10) {
                if line.contains("error") {
                    errors.push(line.to_string());
                }
            }
            if errors.is_empty() {
                errors.push("cargo check failed (see stderr for details)".to_string());
            }
        }
    } else {
        // Just check if the file is valid Rust syntax by reading it
        let content = fs::read_to_string(path)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        if let Err(e) = syn::parse_file(&content) {
            errors.push(format!("Rust syntax error: {e}"));
        }
    }

    Ok(())
}

// ── Deploy command ──────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
async fn run_deploy(
    environment: String,
    token: Option<String>,
    server: String,
    skip_build: bool,
    dry_run: bool,
    workspace_id_override: Option<String>,
    agent_id: Option<String>,
    stream_output: bool,
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

    if stream_output {
        DeployEvent::new("deploy_init")
            .with_message(&format!("deploying {} v{}", manifest.agent.name, manifest.agent.version))
            .emit();
    } else {
        println!("Deploying agent: {}", manifest.agent.name);
        println!("  version:     {}", manifest.agent.version);
        println!("  environment: {environment}");
        println!("  server:      {server}");
        if let Some(ref aid) = agent_id {
            println!("  agent_id:    {aid}");
        }
        println!();
    }

    // ── Authenticate ────────────────────────────────────────────
    if !stream_output {
        println!("Authenticating...");
    }
    let mut config = DeployClientConfig {
        endpoint: server.clone(),
        token: token.clone(),
        workspace_id: workspace_id_override.clone(),
    };

    // Try loading cached config for workspace_id and token fallback
    if let Ok(cached) = DeployClientConfig::load() {
        if config.token.is_none() && cached.token.is_some() && cached.endpoint == server {
            config.token = cached.token;
            if !stream_output {
                println!("  Using cached credentials");
            }
        }
        if config.workspace_id.is_none() {
            config.workspace_id = cached.workspace_id;
        }
    }

    let mut client = DeployClient::new(config.clone());

    // If we have a token, use it directly. Otherwise, login.
    if let Some(ref token_value) = config.token {
        client = client.with_token(token_value.clone());
        if !stream_output {
            println!("  Using provided token");
        }
    } else {
        if !stream_output {
            println!("  No token provided. Attempting login...");
        }
        let email = std::env::var("ADK_DEPLOY_EMAIL").unwrap_or_else(|_| "cli@local".to_string());
        let login_response = client
            .login_ephemeral(&LoginRequest { email, workspace_name: None })
            .await
            .map_err(|e| format!("login failed: {e}. Provide --token or set ADK_DEPLOY_TOKEN"))?;
        config.workspace_id = Some(login_response.workspace_id.clone());
        if !stream_output {
            println!("  Logged in to workspace: {}", login_response.workspace_id);
        }
    }
    if !stream_output {
        println!();
    }

    // ── Build ───────────────────────────────────────────────────
    if !skip_build {
        if stream_output {
            DeployEvent::new("build_start").emit();
        } else {
            println!("Building release binary...");
        }

        let start = std::time::Instant::now();
        let status = std::process::Command::new("cargo")
            .args(["build", "--release"])
            .status()
            .map_err(|e| format!("failed to run cargo build: {e}"))?;

        if !status.success() {
            return Err("cargo build --release failed".to_string());
        }

        let duration_ms = start.elapsed().as_millis() as u64;
        if stream_output {
            let mut ev = DeployEvent::new("build_complete");
            ev.duration_ms = Some(duration_ms);
            ev.emit();
        } else {
            println!("  Build complete ({duration_ms}ms).");
            println!();
        }
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
    let declared_secrets: Vec<&str> = manifest.secrets.iter().map(|s| s.key.as_str()).collect();
    if !declared_secrets.is_empty() {
        let env_path = Path::new(".env");
        if env_path.exists() {
            if !stream_output {
                println!("Uploading secrets...");
            }
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
                            if !stream_output {
                                println!("  [dry-run] would upload secret ({} chars)", value.len());
                            }
                        } else {
                            client
                                .set_secret(&SecretSetRequest {
                                    environment: environment.clone(),
                                    key: secret_key.clone(),
                                    value: value.to_string(),
                                })
                                .await
                                .map_err(|e| format!("failed to set secret: {e}"))?;
                            if !stream_output {
                                println!("  ✓ uploaded secret");
                            }
                        }
                        uploaded += 1;
                    }
                }
            }
            if uploaded == 0 && !stream_output {
                println!(
                    "  No matching secrets found in .env for {} declared secret(s).",
                    declared_secrets.len()
                );
            }
            if !stream_output {
                println!();
            }
        } else if !stream_output {
            println!(
                "Note: manifest declares {} secret(s) but no .env file found.",
                declared_secrets.len()
            );
            println!("      Set secrets manually or create a .env file.");
            println!();
        }
    }

    // ── Create bundle ───────────────────────────────────────────
    if !stream_output {
        println!("Creating deployment bundle...");
    }
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

    if !stream_output {
        println!("  bundle:   {}", bundle_path.display());
        println!("  size:     {:.1} MB", bundle_size as f64 / 1_048_576.0);
        println!("  checksum: {checksum}");
        println!();
    }

    // ── Push deployment ─────────────────────────────────────────
    if dry_run {
        if stream_output {
            DeployEvent::new("dry_run_complete").with_message("no changes made").emit();
        } else {
            println!("Dry run complete. Would push:");
            println!("  bundle:       {}", bundle_path.display());
            println!("  size:         {:.1} MB", bundle_size as f64 / 1_048_576.0);
            println!("  environment:  {environment}");
            println!("  workspace_id: {:?}", config.workspace_id);
            if let Some(ref aid) = agent_id {
                println!("  agent_id:     {aid}");
            }
            println!("\nNo changes were made to the server.");
        }
        return Ok(());
    }

    if stream_output {
        let mut ev = DeployEvent::new("deploy_start");
        ev.environment = Some(environment.clone());
        ev.emit();
    } else {
        println!("Pushing bundle ({:.1} MB)...", bundle_size as f64 / 1_048_576.0);
    }

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

    if stream_output {
        let mut ev = DeployEvent::new("deploy_complete");
        ev.deployment_id = Some(response.deployment.id.clone());
        ev.status = Some(format!("{:?}", response.deployment.status));
        ev.emit();
    } else {
        println!();
        println!("Deployment successful!");
        println!("  id:       {}", response.deployment.id);
        println!("  version:  {}", response.deployment.version);
        println!("  status:   {:?}", response.deployment.status);
        println!("  endpoint: {}", response.deployment.endpoint_url);
    }

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

    let manifest_bytes =
        fs::read(manifest_path).map_err(|e| format!("failed to read manifest: {e}"))?;
    let mut header = tar::Header::new_gnu();
    header.set_size(manifest_bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    archive
        .append_data(&mut header, "adk-deploy.toml", manifest_bytes.as_slice())
        .map_err(|e| format!("failed to add manifest to bundle: {e}"))?;

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

// ── Templates command ───────────────────────────────────────────

fn get_builtin_templates() -> Vec<TemplateInfo> {
    vec![
        TemplateInfo {
            name: "basic",
            description: "Basic LLM agent with interactive console",
            default_provider: "gemini",
            features: vec!["minimal"],
        },
        TemplateInfo {
            name: "tools",
            description: "Agent with custom function tools using #[tool] macro",
            default_provider: "gemini",
            features: vec!["minimal", "tools"],
        },
        TemplateInfo {
            name: "rag",
            description: "RAG agent with document ingestion and vector search",
            default_provider: "gemini",
            features: vec!["minimal", "rag"],
        },
        TemplateInfo {
            name: "api",
            description: "REST API server with health check and A2A protocol",
            default_provider: "gemini",
            features: vec!["minimal", "server"],
        },
        TemplateInfo {
            name: "openai",
            description: "OpenAI-powered agent (gpt-5.5)",
            default_provider: "openai",
            features: vec!["agents", "models", "openai", "runner", "sessions"],
        },
        TemplateInfo {
            name: "a2a",
            description: "A2A protocol agent with agent card and JSON-RPC endpoint",
            default_provider: "gemini",
            features: vec!["standard"],
        },
    ]
}

fn print_templates(_template_dir: Option<&Path>) {
    println!("Available templates:\n");

    println!("  Agent Types:");
    println!("    {:<14} Single LLM agent with tool calling support", "llm");
    println!("    {:<14} Sequential multi-agent pipeline", "sequential");
    println!("    {:<14} Parallel multi-agent execution", "parallel");
    println!("    {:<14} Loop agent with termination condition", "loop");
    println!("    {:<14} Conditional routing agent", "conditional");
    println!("    {:<14} Graph-based workflow with checkpoints", "graph");
    println!("    {:<14} Real-time voice/audio streaming agent", "realtime");
    println!("    {:<14} Custom agent with manual trait implementation", "custom");

    println!("\n  Enterprise Patterns:");
    println!("    {:<14} LLM + server, auth, sessions, telemetry", "production");
    println!("    {:<14} Supervisor orchestrating sub-agents", "multi-agent");
    println!("    {:<14} Sequential pipeline with state passing", "pipeline");
    println!("    {:<14} Conversational agent with memory + server", "chatbot");
    println!("    {:<14} A2A protocol server with sessions", "a2a-server");

    println!("\n  Legacy (backward-compatible):");
    for t in get_builtin_templates() {
        println!("    {:<14} {}", t.name, t.description);
    }

    println!("\n  Addons (composable with any template):");
    println!("    --addon {:<12} OpenTelemetry tracing", "telemetry");
    println!("    --addon {:<12} API key and JWT authentication", "auth");
    println!("    --addon {:<12} Session state management", "sessions");
    println!("    --addon {:<12} Semantic memory and RAG", "memory");
    println!("    --addon {:<12} MCP tool integration", "mcp");
    println!("    --addon {:<12} Input/output validation", "guardrails");
    println!("    --addon {:<12} Evaluation framework", "eval");
    println!("    --addon {:<12} Browser automation", "browser");
    println!("    --addon {:<12} HTTP server with A2A", "server");

    println!("\nUsage:");
    println!("  cargo adk new my-agent --template llm");
    println!("  cargo adk new my-agent --template llm --addon server --addon sessions");
    println!("  cargo adk new my-agent --template production");
    println!("  cargo adk new my-agent --template graph --provider openai");
}

fn print_templates_json(template_dir: Option<&Path>) {
    let mut templates = get_builtin_templates();

    // Load custom templates from directory if provided
    if let Some(dir) = template_dir {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "toml") {
                    if let Ok(content) = fs::read_to_string(&path) {
                        // Parse custom template manifest (name, description)
                        if let Ok(value) = content.parse::<toml::Value>() {
                            let name =
                                value.get("name").and_then(|v| v.as_str()).unwrap_or("custom");
                            let desc =
                                value.get("description").and_then(|v| v.as_str()).unwrap_or("");
                            let provider =
                                value.get("provider").and_then(|v| v.as_str()).unwrap_or("gemini");
                            // We leak the strings here since TemplateInfo uses &'static str
                            // For JSON output this is fine — process exits after printing
                            templates.push(TemplateInfo {
                                name: Box::leak(name.to_string().into_boxed_str()),
                                description: Box::leak(desc.to_string().into_boxed_str()),
                                default_provider: Box::leak(provider.to_string().into_boxed_str()),
                                features: vec!["minimal"],
                            });
                        }
                    }
                }
            }
        }
    }

    println!("{}", serde_json::to_string_pretty(&templates).unwrap_or_default());
}

// ── Scaffolding commands ────────────────────────────────────────

/// New composable template names.
const COMPOSABLE_TEMPLATES: &[&str] =
    &["llm", "sequential", "parallel", "loop", "conditional", "graph", "realtime", "custom"];

/// Enterprise pattern names.
const ENTERPRISE_PATTERNS: &[&str] =
    &["multi-agent", "production", "pipeline", "chatbot", "a2a-server"];

/// Determine whether to use the composable system for a given template + addons.
fn should_use_composable(template: &str, addons: &[String]) -> bool {
    // If addons are specified, always use the composable system
    if !addons.is_empty() {
        return true;
    }
    // If template is a new composable template name, use composable
    if COMPOSABLE_TEMPLATES.contains(&template) {
        return true;
    }
    // If template is an enterprise pattern, use composable
    if ENTERPRISE_PATTERNS.contains(&template) {
        return true;
    }
    // Otherwise (legacy template with no addons), use legacy
    false
}

#[allow(clippy::too_many_arguments)]
fn create_project(
    name: &str,
    template: &str,
    provider: &str,
    model_override: Option<&str>,
    output_dir: Option<&Path>,
    json_output: bool,
    with_yaml: bool,
    addons: &[String],
    dry_run: bool,
) -> Result<(), String> {
    if should_use_composable(template, addons) {
        return create_project_composable(
            name,
            template,
            provider,
            model_override,
            output_dir,
            json_output,
            addons,
            dry_run,
        );
    }

    // Legacy path: use existing generate_* functions for backward compatibility
    let base_dir = output_dir.unwrap_or_else(|| Path::new("."));
    let project_path = base_dir.join(name);

    if project_path.exists() {
        return Err(format!("directory '{}' already exists", project_path.display()));
    }

    let (cargo_toml, main_rs, env_example) = match template {
        "basic" => generate_basic(name, provider),
        "tools" => generate_tools(name, provider),
        "rag" => generate_rag(name, provider),
        "api" => generate_api(name, provider),
        "openai" => generate_basic(name, "openai"),
        "a2a" => generate_a2a(name, provider, with_yaml),
        _ => {
            return Err(format!(
                "unknown template '{template}'. Run `cargo adk templates` to see options"
            ));
        }
    };

    // Create project structure
    fs::create_dir_all(project_path.join("src")).map_err(|e| e.to_string())?;
    fs::write(project_path.join("Cargo.toml"), &cargo_toml).map_err(|e| e.to_string())?;
    fs::write(project_path.join("src/main.rs"), &main_rs).map_err(|e| e.to_string())?;
    fs::write(project_path.join(".env.example"), &env_example).map_err(|e| e.to_string())?;
    fs::write(project_path.join(".gitignore"), "/target\n.env\n").map_err(|e| e.to_string())?;

    let mut files_created = vec![
        "Cargo.toml".to_string(),
        "src/main.rs".to_string(),
        ".env.example".to_string(),
        ".gitignore".to_string(),
    ];

    // Generate YAML agent definition if requested
    if with_yaml {
        let yaml_content = generate_yaml_definition(name, provider, template);
        fs::create_dir_all(project_path.join("agents")).map_err(|e| e.to_string())?;
        let yaml_filename = format!("agents/{name}.yaml");
        fs::write(project_path.join(&yaml_filename), &yaml_content).map_err(|e| e.to_string())?;
        files_created.push(yaml_filename);
    }

    if json_output {
        let output = NewProjectOutput {
            project_dir: project_path.to_string_lossy().to_string(),
            template: template.to_string(),
            provider: provider.to_string(),
            files_created,
        };
        println!("{}", serde_json::to_string_pretty(&output).unwrap_or_default());
    } else {
        println!("Created ADK agent project: {}/", project_path.display());
        println!("  template: {template}");
        println!("  provider: {provider}");
        if with_yaml {
            println!("  yaml:     agents/{name}.yaml");
        }
        println!();
        println!("Next steps:");
        println!("  cd {}", project_path.display());
        println!("  cp .env.example .env    # add your API key");
        println!("  cargo run");
    }

    Ok(())
}

/// Create a project using the composable system (registry → composition → codegen).
///
/// This handles new templates, enterprise patterns, and legacy templates with addons.
#[allow(clippy::too_many_arguments)]
fn create_project_composable(
    name: &str,
    template: &str,
    provider: &str,
    model_override: Option<&str>,
    output_dir: Option<&Path>,
    json_output: bool,
    addons: &[String],
    dry_run: bool,
) -> Result<(), String> {
    let registry = TemplateRegistry::builtin();

    // Determine the base template and effective addons
    let (base_template, effective_addons) =
        if let Some(pattern) = registry.resolve_pattern(template) {
            // Enterprise pattern: resolve to base_template + pattern addons + user addons
            let mut all_addons: Vec<String> =
                pattern.included_addons.iter().map(|a| a.to_string()).collect();
            for addon in addons {
                if !all_addons.contains(addon) {
                    all_addons.push(addon.clone());
                }
            }
            (pattern.base_template.to_string(), all_addons)
        } else {
            // Direct template (new composable or legacy with addons)
            (template.to_string(), addons.to_vec())
        };

    // Convert addons to &str slice for resolve_composition
    let addon_refs: Vec<&str> = effective_addons.iter().map(|s| s.as_str()).collect();

    // Resolve composition
    let mut manifest = resolve_composition(&registry, &base_template, &addon_refs, provider)
        .map_err(|e| e.to_string())?;

    // Apply model override if provided
    if let Some(model_id) = model_override {
        manifest.model_override = Some(model_id.to_string());
    }

    // Generate project files
    let files = generate_project(&manifest, name);

    // Handle dry-run mode
    if dry_run {
        let dry_output = DryRunOutput {
            files: files
                .iter()
                .map(|f| DryRunFile { path: f.path.clone(), size_bytes: f.content.len() })
                .collect(),
            feature_set: manifest.feature_set.iter().cloned().collect(),
            dependencies: std::iter::once(format!("adk-rust = {ADK_VERSION}"))
                .chain(
                    manifest
                        .dependencies
                        .iter()
                        .map(|d| format!("{} = {}", d.crate_name, d.version)),
                )
                .collect(),
            env_vars: manifest.env_vars.iter().map(|(k, _)| k.clone()).collect(),
        };

        if json_output {
            println!("{}", serde_json::to_string_pretty(&dry_output).unwrap_or_default());
        } else {
            println!("Dry run — files that would be generated:\n");
            for file in &dry_output.files {
                println!("  {:<20} ({} bytes)", file.path, file.size_bytes);
            }
            println!("\nFeatures: [{}]", dry_output.feature_set.join(", "));
            if !dry_output.env_vars.is_empty() {
                println!("Env vars: {}", dry_output.env_vars.join(", "));
            }
            println!("\nNo files were written to disk.");
        }
        return Ok(());
    }

    // Write files to disk
    let base_dir = output_dir.unwrap_or_else(|| Path::new("."));
    let project_path = base_dir.join(name);

    if project_path.exists() {
        return Err(format!("directory '{}' already exists", project_path.display()));
    }

    for file in &files {
        let file_path = project_path.join(&file.path);
        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("failed to create directory: {e}"))?;
        }
        fs::write(&file_path, &file.content)
            .map_err(|e| format!("failed to write {}: {e}", file.path))?;
    }

    // Build output
    let files_created: Vec<String> = files.iter().map(|f| f.path.clone()).collect();

    if json_output {
        let output = NewProjectOutput {
            project_dir: project_path.to_string_lossy().to_string(),
            template: template.to_string(),
            provider: provider.to_string(),
            files_created,
        };
        println!("{}", serde_json::to_string_pretty(&output).unwrap_or_default());
    } else {
        println!("Created ADK agent project: {}/", project_path.display());
        println!("  template: {template}");
        println!("  provider: {provider}");
        if !effective_addons.is_empty() {
            println!("  addons:   {}", effective_addons.join(", "));
        }
        if !manifest.warnings.is_empty() {
            println!();
            for warning in &manifest.warnings {
                println!("  ⚠ {warning}");
            }
        }
        println!();
        println!("Next steps:");
        println!("  cd {}", project_path.display());
        println!("  cp .env.example .env    # add your API key");
        println!("  cargo run");
    }

    Ok(())
}

// ── YAML generation ─────────────────────────────────────────────

fn generate_yaml_definition(name: &str, provider: &str, template: &str) -> String {
    let model_id = match provider {
        "openai" => "gpt-5.5",
        "anthropic" => "claude-sonnet-4-6",
        "deepseek" => "deepseek-v4-flash",
        "ollama" => "gemma4",
        "groq" => "llama-3.3-70b-versatile",
        "openrouter" => "qwen/qwen3.7-max",
        "bedrock" => "anthropic.claude-opus-4-6-v1",
        "azure-ai" => "gpt-5.5",
        _ => "gemini-3.5-flash",
    };

    let tools_section = match template {
        "tools" => "\ntools:\n  - name: greet\n",
        "rag" => "\ntools:\n  - name: rag_search\n",
        _ => "",
    };

    format!(
        r#"# {name} — YAML agent definition
# Hot-reloadable via adk-server (yaml-agent feature)
# Mirrors the Rust agent configuration for runtime use.

name: {name}
description: "A helpful AI assistant"

model:
  provider: {provider}
  model_id: {model_id}

instructions: |
  You are a friendly assistant. Be concise and helpful.
{tools_section}
config:
  temperature: 0.7
"#
    )
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
    match provider {
        "openai" => (
            adk_rust_dep(&provider_features(provider)),
            r#"let model = adk_rust::model::openai::OpenAIClient::new(
        adk_rust::model::openai::OpenAIConfig::new(&api_key, "gpt-5.5"),
    )?;"#,
            "OPENAI_API_KEY",
        ),
        "anthropic" => (
            adk_rust_dep(&provider_features(provider)),
            r#"let model = adk_rust::model::anthropic::AnthropicClient::new(
        adk_rust::model::anthropic::AnthropicConfig::new(&api_key, "claude-sonnet-4-6"),
    )?;"#,
            "ANTHROPIC_API_KEY",
        ),
        _ => (
            adk_rust_dep(&provider_features("gemini")),
            r#"let model = adk_rust::model::GeminiModel::new(&api_key, "gemini-3.5-flash")?;"#,
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

    let pipeline = Arc::new(
        RagPipeline::builder()
            .config(RagConfig::default())
            .embedding_provider(Arc::new(GeminiEmbeddingProvider::new(&gemini_key)?))
            .vector_store(Arc::new(InMemoryVectorStore::new()))
            .chunker(Arc::new(FixedSizeChunker::new(256, 50)))
            .build()?,
    );

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

fn generate_a2a(name: &str, provider: &str, with_yaml: bool) -> (String, String, String) {
    let (_, model_code, env_var) = provider_dep(provider);
    let dep = adk_rust_dep(&["standard"]);

    let yaml_feature = if with_yaml {
        r#"
# Uncomment to enable YAML agent loading:
# adk-rust = { version = "...", features = ["standard", "yaml-agent"] }"#
    } else {
        ""
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
{yaml_feature}"#
    );

    let yaml_commented_code = if with_yaml {
        format!(
            r#"
    // ── YAML agent loading (requires "yaml-agent" feature) ──────────────
    // To use the YAML agent definition instead of the Rust builder above,
    // enable the "yaml-agent" feature in Cargo.toml and replace the agent
    // creation with:
    //
    // use adk_rust::server::YamlAgentLoader;
    // let loader = YamlAgentLoader::from_dir("agents")?;
    // let agent = loader.load("{name}").await?;
    //
    // Then pass `agent` to A2aServer::builder().agent(agent).
    // The YAML definition is at: agents/{name}.yaml
    // ─────────────────────────────────────────────────────────────────────
"#
        )
    } else {
        String::new()
    };

    let main = format!(
        r#"use adk_rust::prelude::*;
use adk_rust::server::A2aServer;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {{
    dotenvy::dotenv().ok();
    let api_key = std::env::var("{env_var}")?;

    {model_code}

    let agent: Arc<dyn Agent> = Arc::new(
        LlmAgentBuilder::new("{name}")
            .description("An A2A-capable AI agent")
            .instruction("You are a helpful assistant exposed via the A2A protocol.")
            .model(Arc::new(model))
            .build()?,
    );
{yaml_commented_code}
    let port = std::env::var("PORT").unwrap_or_else(|_| "8080".to_string());
    let addr = format!("0.0.0.0:{{}}", port);

    let server = A2aServer::builder()
        .agent(agent)
        .bind_addr(&addr)
        .build()?;

    println!("A2A agent server running on http://{{addr}}");
    println!("  GET  /.well-known/agent-card.json — agent card");
    println!("  POST /jsonrpc                     — JSON-RPC endpoint");

    server.serve().await?;
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
    fn create_project_with_output_dir() {
        let tmp = std::env::temp_dir().join("cargo-adk-test-output-dir");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let result = create_project(
            "test-agent",
            "basic",
            "gemini",
            None,
            Some(&tmp),
            false,
            false,
            &[],
            false,
        );
        assert!(result.is_ok());
        assert!(tmp.join("test-agent/Cargo.toml").exists());
        assert!(tmp.join("test-agent/src/main.rs").exists());

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn create_project_with_yaml() {
        let tmp = std::env::temp_dir().join("cargo-adk-test-yaml");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let result = create_project(
            "yaml-agent",
            "tools",
            "gemini",
            None,
            Some(&tmp),
            false,
            true,
            &[],
            false,
        );
        assert!(result.is_ok());
        assert!(tmp.join("yaml-agent/agents/yaml-agent.yaml").exists());

        let yaml_content =
            fs::read_to_string(tmp.join("yaml-agent/agents/yaml-agent.yaml")).unwrap();
        assert!(yaml_content.contains("name: yaml-agent"));
        assert!(yaml_content.contains("provider: gemini"));
        assert!(yaml_content.contains("model_id: gemini-3.5-flash"));
        assert!(yaml_content.contains("- name: greet"));

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn create_project_json_output() {
        let tmp = std::env::temp_dir().join("cargo-adk-test-json");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        // json_output just changes what's printed, project is still created
        let result = create_project(
            "json-agent",
            "basic",
            "gemini",
            None,
            Some(&tmp),
            true,
            false,
            &[],
            false,
        );
        assert!(result.is_ok());
        assert!(tmp.join("json-agent/Cargo.toml").exists());

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn templates_json_output() {
        let templates = get_builtin_templates();
        assert_eq!(templates.len(), 6);
        assert_eq!(templates[0].name, "basic");
        assert_eq!(templates[1].name, "tools");
        assert_eq!(templates[2].name, "rag");
        assert_eq!(templates[3].name, "api");
        assert_eq!(templates[4].name, "openai");
        assert_eq!(templates[5].name, "a2a");
    }

    #[test]
    fn yaml_generation_providers() {
        let gemini_yaml = generate_yaml_definition("test", "gemini", "basic");
        assert!(gemini_yaml.contains("model_id: gemini-3.5-flash"));

        let openai_yaml = generate_yaml_definition("test", "openai", "basic");
        assert!(openai_yaml.contains("model_id: gpt-5.5"));

        let anthropic_yaml = generate_yaml_definition("test", "anthropic", "basic");
        assert!(anthropic_yaml.contains("model_id: claude-sonnet-4-6"));
    }

    #[test]
    fn yaml_generation_tools_template() {
        let yaml = generate_yaml_definition("my-agent", "gemini", "tools");
        assert!(yaml.contains("- name: greet"));
    }

    #[test]
    fn bundle_has_no_dot_slash_prefix() {
        let tmp = std::env::temp_dir().join("cargo-adk-test-bundle");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let manifest_path = tmp.join("adk-deploy.toml");
        fs::write(&manifest_path, b"[agent]\nname = \"test\"\nbinary = \"test\"\n").unwrap();

        let binary_path = tmp.join("test-binary");
        fs::write(&binary_path, b"fake-binary-content").unwrap();

        let bundle_path = tmp.join("test-bundle.tar.gz");
        create_bundle(&bundle_path, &manifest_path, &binary_path, "test-binary").unwrap();

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

        for path in &paths {
            assert!(!path.starts_with("./"), "path should not start with ./: {path}");
        }

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn a2a_template_uses_current_version_and_standard_features() {
        let (cargo_toml, main_rs, _env) = generate_a2a("test-agent", "gemini", false);

        // Verify current version is used
        assert!(
            cargo_toml.contains(&format!(r#"version = "{ADK_VERSION}""#)),
            "a2a template must use the current cargo-adk package version"
        );

        // Verify standard features are included
        assert!(
            cargo_toml.contains(r#"features = ["standard"]"#),
            "a2a template must use the standard feature preset"
        );

        // Verify main.rs references A2aServer
        assert!(main_rs.contains("A2aServer"), "a2a template main.rs must use A2aServer");
    }

    // ── Property-Based Tests ────────────────────────────────────────────────

    mod property_tests {
        use super::*;
        use proptest::prelude::*;

        /// Generate valid project names: alphanumeric with hyphens, 1-64 chars.
        fn arb_project_name() -> impl Strategy<Value = String> {
            "[a-z][a-z0-9-]{0,63}"
                .prop_filter("must not end with hyphen", |s| !s.ends_with('-') && !s.contains("--"))
        }

        /// Generate a supported provider.
        fn arb_provider() -> impl Strategy<Value = &'static str> {
            prop_oneof![Just("gemini"), Just("openai"), Just("anthropic"),]
        }

        // **Feature: a2a-simple-scaffolding, Property 1: Template Generation Completeness**
        // *For any* valid project name (alphanumeric with hyphens, 1-64 chars) and
        // supported provider (gemini, openai, anthropic), the `a2a` template SHALL
        // generate a project containing Cargo.toml, src/main.rs, .env.example, and
        // .gitignore files, and the Cargo.toml SHALL contain the `standard` feature.
        // **Validates: Requirements 1.1, 1.2, 1.4**
        proptest! {
            #![proptest_config(ProptestConfig::with_cases(100))]

            #[test]
            fn prop_a2a_template_generation_completeness(
                name in arb_project_name(),
                provider in arb_provider(),
            ) {
                let tmp = std::env::temp_dir().join(format!("cargo-adk-prop-{name}"));
                let _ = fs::remove_dir_all(&tmp);
                fs::create_dir_all(&tmp).unwrap();

                let result = create_project(&name, "a2a", provider, None, Some(&tmp), false, false, &[], false);
                prop_assert!(result.is_ok(), "create_project failed for name={name}, provider={provider}: {:?}", result.err());

                let project_path = tmp.join(&name);

                // All required files must exist
                prop_assert!(
                    project_path.join("Cargo.toml").exists(),
                    "Cargo.toml missing for name={name}"
                );
                prop_assert!(
                    project_path.join("src/main.rs").exists(),
                    "src/main.rs missing for name={name}"
                );
                prop_assert!(
                    project_path.join(".env.example").exists(),
                    ".env.example missing for name={name}"
                );
                prop_assert!(
                    project_path.join(".gitignore").exists(),
                    ".gitignore missing for name={name}"
                );

                // Cargo.toml must contain the standard feature
                let cargo_content = fs::read_to_string(project_path.join("Cargo.toml")).unwrap();
                prop_assert!(
                    cargo_content.contains(r#"features = ["standard"]"#),
                    "Cargo.toml missing standard feature for name={name}"
                );

                // Cargo.toml must contain the current version
                prop_assert!(
                    cargo_content.contains(&format!(r#"version = "{ADK_VERSION}""#)),
                    "Cargo.toml missing current version for name={name}"
                );

                // main.rs must reference A2aServer
                let main_content = fs::read_to_string(project_path.join("src/main.rs")).unwrap();
                prop_assert!(
                    main_content.contains("A2aServer"),
                    "main.rs missing A2aServer reference for name={name}"
                );

                // Clean up
                let _ = fs::remove_dir_all(&tmp);
            }
        }
    }
}
