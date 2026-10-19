//! Live and scripted checks for provider and runtime fixes found in the 3.0 README audit.
//!
//! | Item | Check |
//! |------|-------|
//! | 1 | OpenAI Responses background mode: submit, then poll to completion |
//! | 2 | OpenAI Realtime over WebRTC: connect and answer one question (`--features webrtc`) |
//! | 3 | Gemini Live: connect without installing a rustls crypto provider |
//! | 4 | Every `GroqConfig` preset selects a model Groq lists |
//! | 5 | `BenchRunner` reports measured memory and no unmeasured metric |
//! | 6 | A `GraphAgent` turn takes more than 50 super-steps when its graph allows it |
//! | 7 | Ollama caps output at `max_output_tokens` and finishes with `MaxTokens` |
//!
//! ```bash
//! cargo run                       # every item; live items skip without their key
//! cargo run -- 1 3                # only these items
//! cargo run --features webrtc     # include the WebRTC transport (needs cmake)
//! ```

mod bench;
mod graph;
mod groq;
mod ollama;
mod realtime;
mod report;
mod responses;

use report::Report;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // The shell may export stale keys, so the file's values win.
    let env_file = std::env::var("ADK_ENV_FILE")
        .unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env").to_string());
    if dotenvy::from_path_override(&env_file).is_err() {
        let _ = dotenvy::dotenv_override();
    }

    let items: Vec<u8> = std::env::args().skip(1).filter_map(|arg| arg.parse().ok()).collect();
    let selected = |item: u8| items.is_empty() || items.contains(&item);

    let mut report = Report::default();
    // Gemini Live runs first: nothing in this process has installed a crypto provider yet.
    if selected(3) {
        realtime::run_gemini(&mut report).await;
    }
    if selected(1) {
        responses::run(&mut report).await;
    }
    if selected(2) {
        realtime::run_openai_webrtc(&mut report).await;
    }
    if selected(4) {
        groq::run(&mut report).await;
    }
    if selected(5) {
        bench::run(&mut report).await;
    }
    if selected(6) {
        graph::run(&mut report).await;
    }
    if selected(7) {
        ollama::run(&mut report).await;
    }

    if report.summarize() > 0 {
        std::process::exit(1);
    }
    Ok(())
}
