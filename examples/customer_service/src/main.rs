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
//! # → open http://localhost:3066
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

mod server;

use tracing::info;

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
            info!("starting Customer Service Agent on http://localhost:{port}");
            println!();
            println!("╔══════════════════════════════════════════════════════════════╗");
            println!("║   Customer Service Agent — multimodal realtime support       ║");
            println!("║                                                            ║");
            println!("║   Open: http://localhost:{port:<5}                              ║");
            println!("║                                                            ║");
            println!("║   Keys: OPENAI_API_KEY and/or GEMINI_API_KEY                ║");
            println!("╚══════════════════════════════════════════════════════════════╝");
            println!();
            server::run_server(port).await?;
        }
    }
    Ok(())
}
