//! Legacy serve entry point.
//!
//! New code should use [`Launcher`](crate::Launcher) instead, which provides
//! the same server with additional configuration options (security, memory,
//! artifacts, graceful shutdown, etc.).

use adk_core::AgentLoader;
use adk_server::{ServerConfig, create_app, shutdown_signal};
use adk_session::InMemorySessionService;
use anyhow::Result;
use std::sync::Arc;
use tracing::warn;

/// Start a web server with the given agent loader.
///
/// This is a convenience wrapper kept for backward compatibility with
/// existing examples. Prefer [`Launcher`](crate::Launcher) for new code.
pub async fn run_serve(agent_loader: Arc<dyn AgentLoader>, port: u16) -> Result<()> {
    let span_exporter = match adk_telemetry::init_with_adk_exporter("adk-server") {
        Ok(exporter) => Some(exporter),
        Err(e) => {
            warn!("failed to initialize telemetry: {e}");
            None
        }
    };

    let session_service = Arc::new(InMemorySessionService::new());

    let mut config = ServerConfig::new(agent_loader, session_service);

    if let Some(exporter) = span_exporter {
        config = config.with_span_exporter(exporter);
    }

    let app = create_app(config);

    let addr = format!("0.0.0.0:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    println!("ADK Server starting on http://localhost:{port}");
    println!("Press Ctrl+C to stop");

    axum::serve(listener, app).with_graceful_shutdown(shutdown_signal()).await?;

    Ok(())
}
