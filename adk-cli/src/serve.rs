//! Legacy serve entry point.
//!
//! New code should use [`Launcher`](crate::Launcher) instead, which provides
//! the same server with additional configuration options (security, memory,
//! artifacts, graceful shutdown, etc.).

use crate::launcher::{bind_socket, listen_until_signal};
use adk_core::AgentLoader;
use adk_server::{ServerConfig, create_app};
use adk_session::InMemorySessionService;
use anyhow::Result;
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::{info, warn};

/// Start a web server with the given agent loader on `127.0.0.1:{port}`.
///
/// The listening address is logged at `info` through `tracing`, which the ADK telemetry
/// subscriber this function installs prints by default.
///
/// This is a convenience wrapper kept for backward compatibility with
/// existing examples. Prefer [`Launcher`](crate::Launcher) for new code; its
/// [`with_bind_address`](crate::Launcher::with_bind_address) listens on other interfaces.
pub async fn run_serve(agent_loader: Arc<dyn AgentLoader>, port: u16) -> Result<()> {
    let span_exporter = match adk_telemetry::init_with_adk_exporter("adk-server") {
        Ok(exporter) => Some(exporter),
        Err(e) => {
            warn!(error = %e, "failed to initialize telemetry");
            None
        }
    };

    let session_service = Arc::new(InMemorySessionService::new());

    let mut config = ServerConfig::new(agent_loader, session_service);

    if let Some(exporter) = span_exporter {
        config = config.with_span_exporter(exporter);
    }

    let app = create_app(config);

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let (listener, stop) = listen_until_signal(bind_socket(addr)?, addr)?;

    info!(server.address = %addr, "adk server listening; press Ctrl+C to stop");

    axum::serve(listener, app).with_graceful_shutdown(stop).await?;

    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use adk_core::{Agent, EventStream, InvocationContext, SingleAgentLoader};
    use async_trait::async_trait;
    use std::time::{Duration, Instant};

    struct Quiet;

    #[async_trait]
    impl Agent for Quiet {
        fn name(&self) -> &str {
            "quiet"
        }

        fn description(&self) -> &str {
            "answers nothing"
        }

        fn sub_agents(&self) -> &[Arc<dyn Agent>] {
            &[]
        }

        async fn run(&self, _ctx: Arc<dyn InvocationContext>) -> adk_core::Result<EventStream> {
            Ok(Box::pin(futures::stream::empty()))
        }
    }

    /// A `SIGTERM` the moment the port answers stops the server cleanly: without a handler by
    /// then, the signal would end this test's process.
    #[tokio::test(flavor = "multi_thread")]
    async fn run_serve_stops_cleanly_on_a_sigterm_as_soon_as_it_listens() {
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let served =
            tokio::spawn(run_serve(Arc::new(SingleAgentLoader::new(Arc::new(Quiet))), port));
        let started = Instant::now();
        while tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_err() {
            assert!(!served.is_finished(), "run_serve ended before it listened");
            assert!(started.elapsed() < Duration::from_secs(60), "run_serve did not listen");
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        // SAFETY: signals this process, whose server catches SIGTERM.
        unsafe { libc::kill(libc::getpid(), libc::SIGTERM) };
        let stopped = tokio::time::timeout(Duration::from_secs(60), served).await;
        stopped.expect("run_serve stops on SIGTERM").unwrap().unwrap();
    }
}
