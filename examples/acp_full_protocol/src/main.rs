//! # ACP full-protocol reference server
//!
//! Serves a deterministic ADK-Rust agent over the stable ACP v1 stdio transport
//! through a real [`Runner`](adk_runner::Runner)-backed `AcpServer` — with **no
//! API key required**. It exists so the Phase 2 ACP server-direction features
//! (embedded-resource prompts, multimodal prompts, the permission bridge,
//! `session/load` replay, and usage / tool-call updates) have a runnable,
//! tested reference.
//!
//! The behavior lives in the [`acp_full_protocol`] library so the crate's
//! integration test can drive the exact same agent through the SDK's in-process
//! duplex channel. See the README for the full protocol walk-through.
//!
//! ## Run
//!
//! ```bash
//! cd examples/acp_full_protocol
//! cargo run
//! ```
//!
//! Then connect an ACP client (editor or SDK) over stdio. Diagnostics go to
//! stderr so they never corrupt the JSON protocol on stdout.

use std::sync::Arc;

use acp_full_protocol::{
    ReferenceControls, ScriptedAgent, WorkspaceToken, build_delete_tool, build_session_service,
};
use adk_acp::server::{AcpServer, AcpServerConfigBuilder, TransportConfig};
use adk_core::Agent;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    // Tracing goes to stderr so stdout stays a clean ACP JSON stream.
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,adk_acp=debug")),
        )
        .with_writer(std::io::stderr)
        .init();

    // A real, confirmation-gated tool plus the deterministic agent that gates it.
    let delete_tool = build_delete_tool();
    let agent: Arc<dyn Agent> = Arc::new(ScriptedAgent::new(delete_tool));
    let session_service = build_session_service();

    let mut config = AcpServerConfigBuilder::new()
        .agent(agent)
        .session_service(session_service)
        .agent_name(acp_full_protocol::AGENT_NAME)
        .agent_description(
            "Deterministic ADK-Rust agent exercising ACP v1 server-direction features",
        )
        .session_controls(Arc::new(ReferenceControls))
        .transport(TransportConfig::Stdio);
    // With a token configured, clients must authenticate with `workspace-token`.
    let token = std::env::var("ACP_EXAMPLE_TOKEN").unwrap_or_default();
    if !token.is_empty() {
        config = config.authenticator(Arc::new(WorkspaceToken::new(token)));
    }
    let config = config.build()?;

    eprintln!("╔══════════════════════════════════════════════════════════════╗");
    eprintln!("║  ACP full-protocol reference server                          ║");
    eprintln!("╚══════════════════════════════════════════════════════════════╝");
    eprintln!();
    eprintln!("  Agent: {}", acp_full_protocol::AGENT_NAME);
    eprintln!("  Transport: stdio (newline-delimited JSON)");
    eprintln!("  No API key required — the agent is deterministic.");
    eprintln!();
    eprintln!("  Exercises: embedded-resource prompts, image/audio prompts,");
    eprintln!("  the permission bridge (confirmation-gated delete_file),");
    eprintln!("  session/load replay, and usage / tool-call updates.");
    eprintln!("  ACP v5: system prompts (send \"instructions?\"), end-of-turn usage,");
    eprintln!("  session title / status / archive / options / fork / resume,");
    eprintln!("  modes and config options, and auth when ACP_EXAMPLE_TOKEN is set.");
    eprintln!();
    eprintln!("  Press Ctrl+C or close stdin (Ctrl+D) to stop.");
    eprintln!("──────────────────────────────────────────────────────────────────");

    let handle = AcpServer::run(config).await?;
    tracing::info!("ACP full-protocol server running — waiting for messages on stdin");
    handle.wait().await?;
    tracing::info!("ACP full-protocol server shut down cleanly");
    Ok(())
}
