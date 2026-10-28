//! # Customer Service Agent — multimodal realtime support
//!
//! A next-generation customer-support agent demo: the agent **sees** what you
//! show the camera, **hears** your voice and reads your tone, and takes **real
//! actions** (process a refund, hand off to a human). It runs on either
//! **OpenAI** (`gpt-realtime`) or **Gemini** (`gemini-3.1-flash-live-preview`)
//! via a server-side bridge built on [`IntegratedRealtimeRunner`].
//!
//! ```bash
//! cargo run --manifest-path examples/customer_service/Cargo.toml
//! # → open http://127.0.0.1:3066
//!
//! # Headless smoke test (no browser/mic) — asks for a refund by text and
//! # checks the process_refund tool runs:
//! cargo run --manifest-path examples/customer_service/Cargo.toml -- probe openai
//! cargo run --manifest-path examples/customer_service/Cargo.toml -- probe gemini
//! ```
//!
//! Requires `OPENAI_API_KEY` (OpenAI) and/or `GEMINI_API_KEY` / `GOOGLE_API_KEY`
//! (Gemini). Gemini is the better fit for continuous video; OpenAI receives
//! periodic image snapshots.
//!
//! The server listens on `127.0.0.1` unless `BIND_HOST` names another address, and answers
//! only requests from its own page.

mod origin;
mod server;

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
    match args.get(1).map(String::as_str) {
        Some("probe") => {
            let provider = args.get(2).map(String::as_str).unwrap_or("openai");
            server::run_probe(provider).await?;
        }
        _ => {
            let port: u16 = std::env::var("PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(3066);
            // Loopback unless BIND_HOST widens it: the server has no authentication and every
            // session spends the provider keys it holds.
            let host = std::env::var("BIND_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
            let listener = tokio::net::TcpListener::bind((host.as_str(), port)).await?;
            let addr = listener.local_addr()?;
            if !addr.ip().is_loopback() {
                warn!(
                    %addr,
                    "listening beyond loopback without authentication: anyone who can reach \
                     this address can start sessions on your API keys"
                );
            }
            let url = format!("http://{addr}");
            info!("starting Customer Service Agent on {url}");
            println!();
            println!("╔══════════════════════════════════════════════════════════════╗");
            println!("║   Customer Service Agent — multimodal realtime support       ║");
            println!("║                                                            ║");
            println!("║   Open: {url:<51}║");
            println!("║                                                            ║");
            println!("║   Keys: OPENAI_API_KEY and/or GEMINI_API_KEY                ║");
            println!("╚══════════════════════════════════════════════════════════════╝");
            println!();
            server::run_server(listener).await?;
        }
    }
    Ok(())
}
