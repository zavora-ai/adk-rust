//! # ADK-Rust Realtime Voice — Mindfulness with Mia
//!
//! A full web UI demonstrating real-time voice capabilities:
//!
//! - **Web interface** matching a mindfulness coaching orchestration engine
//! - **WebSocket bridge** between browser audio and OpenAI/Gemini realtime sessions
//! - **Memory & insights panel** for user context management
//! - **Voice session controls** — mute, hang up, pause
//! - **Tool calling** during live voice conversations
//! - **Mid-session context mutation** for dynamic persona shifts
//!
//! ## Run
//!
//! ```bash
//! cargo run --manifest-path examples/realtime_voice/Cargo.toml -- web
//! ```
//!
//! Then open http://127.0.0.1:3033 in your browser. The server listens on `127.0.0.1`
//! unless `BIND_HOST` names another address, and answers only requests from its own page.

mod origin;
mod server;
mod tools;

use tracing::{info, warn};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("failed to install rustls crypto provider");

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    dotenvy::dotenv().ok();

    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(|s| s.as_str()).unwrap_or("web");

    match mode {
        "probe" => {
            // Headless smoke test of the realtime + integration path.
            // Usage: `cargo run -- probe [openai|gemini]` (default openai).
            let provider = args.get(2).map(|s| s.as_str()).unwrap_or("openai");
            server::run_probe(provider).await?;
        }
        _ => {
            let port: u16 = std::env::var("PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(3033);
            // Loopback unless BIND_HOST widens it: the server has no authentication, every
            // session spends the provider keys it holds, and `/api/memory` reads and resets the
            // user's memory graph.
            let host = std::env::var("BIND_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
            let listener = tokio::net::TcpListener::bind((host.as_str(), port)).await?;
            let addr = listener.local_addr()?;
            if !addr.ip().is_loopback() {
                warn!(
                    %addr,
                    "listening beyond loopback without authentication: anyone who can reach \
                     this address can start sessions on your API keys and read or reset memory"
                );
            }
            let url = format!("http://{addr}");
            info!("starting Mindfulness with Mia on {url}");
            println!();
            println!("╔══════════════════════════════════════════════════════════════╗");
            println!("║     Mindfulness with Mia — Realtime Voice UI                ║");
            println!("║                                                            ║");
            println!("║     Open: {url:<49}║");
            println!("║                                                            ║");
            println!("║     Provider: Set OPENAI_API_KEY or GOOGLE_API_KEY          ║");
            println!("╚══════════════════════════════════════════════════════════════╝");
            println!();

            server::run_server(listener).await?;
        }
    }

    Ok(())
}
