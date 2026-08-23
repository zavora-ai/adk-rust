//! # Vertex AI Agent Engine sandbox — managed code execution
//!
//! Creates a code-execution sandbox under a reasoning engine, runs a Python
//! snippet against an input file, prints the captured stdout, and deletes
//! the sandbox again.
//!
//! ```bash
//! gcloud auth application-default login
//! cargo run --manifest-path examples/vertex_sandbox/Cargo.toml
//! ```
//!
//! Requires `GOOGLE_CLOUD_PROJECT`, `GOOGLE_CLOUD_LOCATION`, and
//! `GOOGLE_CLOUD_AGENT_ENGINE_ID` naming a provisioned Agent Engine.

use adk_code::vertex_sandbox::{
    CreateSandboxRequest, InputFile, VertexSandboxClient, VertexSandboxConfig,
};
use tracing::info;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    println!();
    println!("╔════════════════════════════════════════════════════════════╗");
    println!("║   Vertex AI Agent Engine sandbox — managed code execution  ║");
    println!("║                                                            ║");
    println!("║   Env: GOOGLE_CLOUD_PROJECT, GOOGLE_CLOUD_LOCATION,        ║");
    println!("║        GOOGLE_CLOUD_AGENT_ENGINE_ID                        ║");
    println!("╚════════════════════════════════════════════════════════════╝");
    println!();

    let config = VertexSandboxConfig::from_env()?;
    let engine = std::env::var("GOOGLE_CLOUD_AGENT_ENGINE_ID")
        .map_err(|_| anyhow::anyhow!("set GOOGLE_CLOUD_AGENT_ENGINE_ID to the engine ID"))?;
    let client = VertexSandboxClient::new_with_adc(config)?;

    // 1. Create a sandbox (waits the create operation, then re-fetches it).
    let sandbox = client
        .create_sandbox(&engine, CreateSandboxRequest::new("adk-rust-example").with_ttl("600s"))
        .await?;
    let name = sandbox.name.expect("created sandbox has a name");
    info!(sandbox.name = name.as_str(), sandbox.state = ?sandbox.state, "sandbox created");

    // 2. Run code against an input file; `:execute` is synchronous.
    let files = [InputFile::new("data.csv", "text/csv", b"lang,score\nrust,10\n".to_vec())];
    let result =
        client.execute_code(&name, "print(open('data.csv').read().upper())", &files).await?;

    println!("─── stdout ──────────────────────────────────────────────────");
    println!("{}", result.stdout);
    if !result.stderr.is_empty() {
        println!("─── stderr ──────────────────────────────────────────────────");
        println!("{}", result.stderr);
    }
    for file in &result.output_files {
        println!("output file: {} ({} bytes)", file.name, file.data.len());
    }

    // 3. Clean up (waits the delete operation).
    client.delete_sandbox(&name).await?;
    info!(sandbox.name = name.as_str(), "sandbox deleted");

    Ok(())
}
