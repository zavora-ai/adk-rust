//! Serves a live OpenAI-backed team through the embedded ADK developer UI.

use std::sync::Arc;

use adk_core::{Agent, SingleAgentLoader};
use adk_server::{SecurityConfig, ServerConfig, create_app};
use adk_session::InMemorySessionService;
use team_architectures_example::{print_spec, supervisor_handoff_team};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    adk_core::ensure_crypto_provider();
    let (spec, team) = supervisor_handoff_team()?;
    print_spec(&spec)?;

    let agent = Arc::new(team) as Arc<dyn Agent>;
    let loader = Arc::new(SingleAgentLoader::new(agent));
    let sessions = Arc::new(InMemorySessionService::new());
    let span_exporter = adk_telemetry::init_with_adk_exporter("team-runtime-ui")?;
    let config = ServerConfig::new(loader, sessions)
        .with_security(SecurityConfig::development())
        .with_span_exporter(span_exporter);

    let address = std::env::var("ADK_UI_ADDRESS")
        .unwrap_or_else(|_| "127.0.0.1:8088".to_string());
    let listener = tokio::net::TcpListener::bind(&address).await?;
    println!("Embedded team UI: http://{address}/ui/");
    println!("Choose support_team, start a session, and send a billing or technical request.");
    axum::serve(listener, create_app(config)).await?;
    Ok(())
}
