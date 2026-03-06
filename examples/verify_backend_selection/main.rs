//! Verify that the backend trait refactor correctly selects Studio vs Vertex.
//!
//! This example runs against AI Studio (the default REST backend) and validates
//! that all core operations work through the new `GeminiBackend` trait delegation.
//!
//! # Usage
//! ```bash
//! GEMINI_API_KEY=your-key cargo run -p adk-examples --example verify_backend_selection
//! ```

use adk_gemini::{Gemini, GeminiBuilder, Model};
use futures::TryStreamExt;
use std::env;
use std::io::{self, Write};
use std::process::ExitCode;
use tracing::info;

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::builder()
                .with_default_directive(tracing::level_filters::LevelFilter::INFO)
                .from_env_lossy(),
        )
        .init();

    match do_main().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, "execution failed");
            ExitCode::FAILURE
        }
    }
}

async fn do_main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY environment variable required");

    // ── Test 1: Default constructor → Studio backend ────────────────
    info!("━━━ Test 1: Gemini::new() → Studio backend ━━━");
    let client = Gemini::new(&api_key)?;
    let response = client
        .generate_content()
        .with_user_message("Say 'studio backend works' in exactly those words.")
        .execute()
        .await?;
    info!(response = %response.text(), "Gemini::new() works");
    info!("✅ Default constructor (Studio)");

    // ── Test 2: with_model constructor ──────────────────────────────
    info!("━━━ Test 2: Gemini::with_model() ━━━");
    let client = Gemini::with_model(&api_key, Model::Gemini25Flash)?;
    let response = client
        .generate_content()
        .with_user_message("What model are you? Reply in 10 words or less.")
        .execute()
        .await?;
    info!(response = %response.text(), "with_model() works");
    info!("✅ with_model constructor");

    // ── Test 3: GeminiBuilder → Studio backend ──────────────────────
    info!("━━━ Test 3: GeminiBuilder → Studio backend ━━━");
    let client = GeminiBuilder::new(&api_key).with_model(Model::Gemini25Flash).build()?;
    let response = client
        .generate_content()
        .with_user_message("Say 'builder works' in exactly those words.")
        .execute()
        .await?;
    info!(response = %response.text(), "GeminiBuilder works");
    info!("✅ GeminiBuilder (Studio)");

    // ── Test 4: Studio streaming ────────────────────────────────────
    info!("━━━ Test 4: Studio streaming ━━━");
    let mut stream =
        client.generate_content().with_user_message("Count from 1 to 3.").execute_stream().await?;

    let mut chunks = 0u32;
    while let Some(chunk) = stream.try_next().await? {
        let t = chunk.text();
        if !t.is_empty() {
            chunks += 1;
            print!("{t}");
            io::stdout().flush()?;
        }
    }
    println!();
    info!(chunks, "Studio streaming works");
    assert!(chunks > 0);
    info!("✅ Studio streaming ({chunks} chunks)");

    // ── Test 5: Studio embedding ────────────────────────────────────
    info!("━━━ Test 5: Studio embedding ━━━");
    let embed_client = Gemini::with_model(&api_key, Model::GeminiEmbedding001)?;
    let embedding = embed_client
        .embed_content()
        .with_text("backend trait refactor validation")
        .execute()
        .await?;
    let dim = embedding.embedding.values.len();
    info!(dimensions = dim, "embedding received");
    assert!(dim > 0);
    info!("✅ Studio embedding ({dim} dimensions)");

    // ── Test 6: v1 API constructor ──────────────────────────────────
    info!("━━━ Test 6: Gemini::with_v1() ━━━");
    let client = Gemini::with_v1(&api_key)?;
    let response = client.generate_content().with_user_message("Say 'v1 works'.").execute().await?;
    info!(response = %response.text(), "v1 API works");
    info!("✅ v1 API constructor");

    info!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    info!("🎉 All Studio backend tests passed!");
    Ok(())
}
