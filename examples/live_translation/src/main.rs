//! # Live Translation — real-time speech-to-speech translation in the browser
//!
//! A web app that translates your speech into a target language in real time,
//! using a **server-side bridge** (the same architecture as the realtime voice
//! example) to dedicated translation models:
//!
//! - **OpenAI** `gpt-realtime-translate`
//! - **Gemini** `gemini-3.5-live-translate-preview`
//!
//! Pick a provider and target language in the UI, speak, and hear + read the
//! translation. The Rust server owns the provider connection; the browser only
//! captures and plays audio.
//!
//! ## Run
//!
//! ```bash
//! cargo run --manifest-path examples/live_translation/Cargo.toml
//! # → open http://127.0.0.1:3055
//!
//! # Headless connectivity probe (no mic) — validates endpoint/auth/model:
//! cargo run --manifest-path examples/live_translation/Cargo.toml -- probe openai
//! cargo run --manifest-path examples/live_translation/Cargo.toml -- probe gemini
//! ```
//!
//! Requires `OPENAI_API_KEY` (OpenAI) and/or `GEMINI_API_KEY` / `GOOGLE_API_KEY`
//! (Gemini). The server listens on `127.0.0.1` unless `BIND_HOST` names another address, and
//! answers only requests from its own page.

mod origin;
mod server;
mod translate;

use tracing::{info, warn};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("failed to install rustls crypto provider");
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("probe") => {
            // probe <provider> [target] [pcm_file]
            let provider =
                translate::Provider::parse(args.get(2).map(String::as_str).unwrap_or("openai"));
            let target = args.get(3).cloned().unwrap_or_else(|| "es".to_string());
            let pcm_file = args.get(4).cloned();
            translate::probe(provider, target, pcm_file).await?;
        }
        _ => {
            let port: u16 = std::env::var("PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(3055);
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
            info!("starting Live Translation on {url}");
            println!();
            println!("╔══════════════════════════════════════════════════════════════╗");
            println!("║     Live Translation — real-time speech translation         ║");
            println!("║                                                            ║");
            println!("║     Open: {url:<49}║");
            println!("║                                                            ║");
            println!("║     Keys: OPENAI_API_KEY and/or GEMINI_API_KEY              ║");
            println!("╚══════════════════════════════════════════════════════════════╝");
            println!();
            server::run_server(listener).await?;
        }
    }
    Ok(())
}
