use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_core::{Agent, Content};
use adk_model::gemini::GeminiModel;
use adk_runner::Runner;
use adk_runner::sandbox_runner::SandboxRunner;
use adk_sandbox::workspace::ManifestEntry;
use adk_session::InMemorySessionService;
use clap::Parser;
use tracing_subscriber::EnvFilter;

mod config;
mod display;

use config::{build_manifest, build_sandbox_config};
use display::{banner, print_event, print_summary};

/// Sandbox Workspace Agent Example
///
/// Demonstrates the full sandbox-agent-harness lifecycle:
/// Manifest → Provision → Session → Agent loop → Stop → Snapshot
#[derive(Parser, Debug)]
#[command(name = "sandbox-workspace-agent")]
#[command(about = "Demonstrates the sandbox-agent-harness lifecycle")]
pub struct CliArgs {
    /// Use DockerClient instead of LocalUnixClient for sandbox isolation.
    #[arg(long)]
    pub docker: bool,

    /// Enable snapshot/resume demonstration after the agent loop completes.
    #[arg(long)]
    pub snapshot: bool,
}

const AGENT_INSTRUCTIONS: &str = "\
You are a Rust developer assistant working inside a sandbox workspace. \
Your task is to create a simple Rust hello-world project. \
\
Steps: \
1. Use list_dir to see the current workspace contents \
2. Use write_file to create hello-world/Cargo.toml with a basic package definition \
3. Use write_file to create hello-world/src/main.rs with fn main() that prints \"Hello, world!\" \
4. Use exec_command to run 'cargo build' in the hello-world directory \
5. Use exec_command to run the compiled binary at hello-world/target/debug/hello-world \
\
Use only the provided tools. Do not skip steps.";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load .env file if present (ignore errors if not found)
    let _ = dotenvy::dotenv();

    // Initialize tracing with RUST_LOG support, defaulting to "info"
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    // Parse CLI arguments
    let args = CliArgs::parse();

    // Validate GOOGLE_API_KEY environment variable
    let api_key = std::env::var("GOOGLE_API_KEY").map_err(|_| {
        anyhow::anyhow!(
            "GOOGLE_API_KEY environment variable is required.\n\
             Set it in .env or export it: export GOOGLE_API_KEY=your-key-here"
        )
    })?;

    // Print startup banner and configuration
    banner("Sandbox Workspace Agent Example");
    println!("  Backend:  {}", if args.docker { "DockerClient" } else { "LocalUnixClient" });
    println!("  Snapshot: {}", if args.snapshot { "enabled" } else { "disabled" });

    // ─── Phase 1: Manifest Definition ───────────────────────────────────────────
    banner("Phase 1: Manifest Definition");
    let manifest = build_manifest();
    println!("  Manifest entries:");
    for entry in &manifest.entries {
        match entry {
            ManifestEntry::Directory { path } => {
                println!("    📁 {path}/");
            }
            ManifestEntry::File { path, .. } => {
                println!("    📄 {path}");
            }
            ManifestEntry::GitRepo { url, path, .. } => {
                println!("    🔗 {path} (from {url})");
            }
        }
    }

    // ─── Phase 2: SandboxConfig Construction ────────────────────────────────────
    banner("Phase 2: SandboxConfig Construction");
    let sandbox_config = build_sandbox_config(&args).await?;
    println!("  Capabilities: Shell, Filesystem");
    println!("  Session timeout: {:?}", sandbox_config.session_timeout);
    println!("  Command timeout: {:?}", sandbox_config.command_timeout);
    println!("  Snapshot on stop: {}", sandbox_config.snapshot_on_stop);

    // ─── Phase 3: Agent and Runner Construction ─────────────────────────────────
    banner("Phase 3: Agent and Runner Construction");
    println!("  Building LlmAgent with Gemini model...");

    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")
        .map_err(|e| anyhow::anyhow!("Failed to create Gemini model: {e}"))?;

    let agent = LlmAgentBuilder::new("sandbox-workspace-agent")
        .model(Arc::new(model))
        .instruction(AGENT_INSTRUCTIONS)
        .build()
        .map_err(|e| anyhow::anyhow!("Failed to build agent: {e}"))?;

    println!("  ✅ Agent built: {}", agent.name());

    let runner = Runner::builder()
        .app_name("sandbox-workspace-agent")
        .agent(Arc::new(agent))
        .session_service(Arc::new(InMemorySessionService::new()))
        .build()
        .map_err(|e| anyhow::anyhow!("Failed to build runner: {e}"))?;

    let sandbox_runner = SandboxRunner::new(runner);
    println!("  ✅ Runner and SandboxRunner constructed");

    // ─── Phase 4: Managed Sandbox Execution ─────────────────────────────────────
    banner("Phase 4: Managed Sandbox Execution");
    println!("  Provisioning, running, snapshotting, and cleaning up...\n");
    let user_content =
        Content::new("user").with_text("Create the Rust hello-world project as instructed.");
    let result = sandbox_runner
        .run(&sandbox_config, "demo-user", "session-1", user_content)
        .await
        .map_err(|error| anyhow::anyhow!("Sandbox run failed after lifecycle cleanup: {error}"))?;

    for event in &result.events {
        print_event(event);
    }
    let snapshot_id = result.snapshot_id;

    // ─── Phase 5: Results ───────────────────────────────────────────────────────
    banner("Phase 5: Results");
    println!("  ✅ Agent execution completed successfully");
    println!("  ✅ Sandbox session stopped");
    if let Some(id) = snapshot_id.as_ref() {
        println!("  ✅ SnapshotId: {}", id.0);
    } else if sandbox_config.snapshot_on_stop {
        println!("  ❌ Snapshot was requested but not returned");
    } else {
        println!("  Snapshot disabled");
    }

    // ─── Phase 6: Snapshot/Resume Verification (optional) ───────────────────────
    if args.snapshot {
        banner("Phase 6: Snapshot/Resume Verification");
        if let Some(ref snap_id) = snapshot_id {
            println!("  Resuming from snapshot: {}", snap_id.0);
            match sandbox_config.client.resume(snap_id).await {
                Ok(resumed_handle) => {
                    println!("  ✅ Resumed session: {}", resumed_handle.0);
                    match sandbox_config.client.start(&resumed_handle).await {
                        Ok(resumed_session) => {
                            // Verify workspace contents with list_dir
                            match resumed_session.list_dir("hello-world").await {
                                Ok(entries) => {
                                    println!("  Workspace contents after resume:");
                                    for entry in &entries {
                                        println!("    {:?} {}", entry.entry_type, entry.name);
                                    }
                                }
                                Err(e) => println!("  ⚠️  list_dir failed: {e}"),
                            }
                            if let Err(error) =
                                sandbox_config.client.stop(&resumed_handle).await
                            {
                                println!("  ⚠️  Failed to stop resumed session: {error}");
                            }
                        }
                        Err(e) => println!("  ❌ Failed to start resumed session: {e}"),
                    }
                }
                Err(e) => println!("  ❌ Resume failed: {e}"),
            }
        } else {
            println!("  ⚠️  No snapshot available for resume verification");
        }
    }

    // ─── Summary ────────────────────────────────────────────────────────────────
    let phases: Vec<(&str, bool)> = vec![
        ("Manifest definition", true),
        ("SandboxConfig construction", true),
        ("Runner construction", true),
        ("Managed sandbox execution", true),
        ("Stop/cleanup", true),
        ("Snapshot", !args.snapshot || snapshot_id.is_some()),
    ];
    print_summary(&phases);

    Ok(())
}
