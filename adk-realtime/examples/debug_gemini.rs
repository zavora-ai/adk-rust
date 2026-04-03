//! Standalone diagnostic for testing Gemini Realtime API connectivity.
//!
//! Usage:
//! ```sh
//! GEMINI_API_KEY=xxx cargo run --example debug_gemini --features gemini
//! ```

use adk_realtime::RealtimeConfig;
use adk_realtime::gemini::{GeminiLiveBackend, GeminiRealtimeModel};
use adk_realtime::runner::{EventHandler, RealtimeRunner};
use std::sync::Arc;

struct PrintingEventHandler;

#[async_trait::async_trait]
impl EventHandler for PrintingEventHandler {
    async fn on_text(&self, text: &str, _id: &str) -> adk_realtime::Result<()> {
        println!("AI: {text}");
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("Failed to install rustls default crypto provider");
    tracing_subscriber::fmt::init();

    // Initialize rustls
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let api_key = std::env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY is required");
    let model_name = std::env::var("GEMINI_MODEL")
        .unwrap_or_else(|_| "models/gemini-2.5-flash-native-audio-latest".to_string());

    println!("--- Gemini Diagnostic ---");
    println!("Model: {model_name}");

    let backend = GeminiLiveBackend::studio(api_key);
    let model = GeminiRealtimeModel::new(backend, &model_name);
    let config = RealtimeConfig::default().with_voice("Aoede");

    let runner = Arc::new(
        RealtimeRunner::builder()
            .model(Arc::new(model))
            .config(config)
            .event_handler(PrintingEventHandler)
            .build()?,
    );

    println!("Connecting to Gemini...");
    match runner.connect().await {
        Ok(_) => println!("SUCCESS: Connected to Gemini BiDi API"),
        Err(e) => {
            eprintln!("FAILURE: Failed to connect: {e}");
            return Err(e.into());
        }
    }

    let runner_clone = Arc::clone(&runner);
    tokio::spawn(async move {
        if let Err(e) = runner_clone.run().await {
            eprintln!("Runner loop error: {e}");
        }
    });

    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    println!("Injecting ping message...");
    runner.send_text("Hello, answer with 'pong' if you hear me.").await?;

    println!("Waiting for response (5s)...");
    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

    println!("Closing session...");
    runner.close().await?;

    println!("Diagnostic complete.");
    Ok(())
}
