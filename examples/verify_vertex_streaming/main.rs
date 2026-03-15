//! Verify Vertex AI streaming support.
//!
//! This example validates that the backend trait refactor correctly enables
//! streaming on Vertex AI (which previously returned `GoogleCloudUnsupported`).
//!
//! # Usage
//!
//! ## With API key:
//! ```bash
//! GOOGLE_CLOUD_PROJECT=your-project \
//! GOOGLE_CLOUD_LOCATION=us-central1 \
//! GEMINI_API_KEY=your-key \
//!   cargo run -p adk-examples --example verify_vertex_streaming
//! ```
//!
//! ## With Application Default Credentials:
//! ```bash
//! GOOGLE_CLOUD_PROJECT=your-project \
//! GOOGLE_CLOUD_LOCATION=us-central1 \
//! VERTEX_USE_ADC=true \
//!   cargo run -p adk-examples --example verify_vertex_streaming
//! ```
//!
//! ## With service account JSON:
//! ```bash
//! GOOGLE_APPLICATION_CREDENTIALS=/path/to/sa.json \
//! GOOGLE_CLOUD_LOCATION=us-central1 \
//!   cargo run -p adk-examples --example verify_vertex_streaming
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
                .with_default_directive(tracing::level_filters::LevelFilter::INFO.into())
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
    let project = env::var("GOOGLE_CLOUD_PROJECT")
        .expect("GOOGLE_CLOUD_PROJECT environment variable required");
    let location = env::var("GOOGLE_CLOUD_LOCATION").unwrap_or_else(|_| "us-central1".to_string());

    info!(project = %project, location = %location, "building Vertex AI client");

    let client = build_vertex_client(&project, &location)?;

    // ── Test 1: Non-streaming (gRPC with REST fallback) ─────────────
    info!("━━━ Test 1: Non-streaming generate ━━━");
    let response = client
        .generate_content()
        .with_user_message("Say 'hello from Vertex AI' in exactly those words.")
        .execute()
        .await?;

    let text = response.text();
    info!(response = %text, "non-streaming response received");
    assert!(!text.is_empty(), "non-streaming response should not be empty");
    info!("✅ Non-streaming generate works");

    // ── Test 2: Streaming (REST SSE — the main fix) ─────────────────
    info!("━━━ Test 2: Streaming generate (REST SSE) ━━━");
    let mut stream = client
        .generate_content()
        .with_user_message("Count from 1 to 5, one number per line.")
        .execute_stream()
        .await?;

    let mut chunk_count = 0u32;
    let mut full_text = String::new();
    while let Some(chunk) = stream.try_next().await? {
        let t = chunk.text();
        if !t.is_empty() {
            chunk_count += 1;
            full_text.push_str(&t);
            print!("{t}");
            io::stdout().flush()?;
        }
    }
    println!();

    info!(chunks = chunk_count, total_len = full_text.len(), "streaming completed");
    assert!(chunk_count > 0, "should have received at least one chunk");
    assert!(!full_text.is_empty(), "streamed text should not be empty");
    info!("✅ Streaming generate works (received {chunk_count} chunks)");

    // ── Test 3: Embedding ───────────────────────────────────────────
    info!("━━━ Test 3: Embedding ━━━");
    let embed_client =
        build_vertex_client_with_model(&project, &location, Model::GeminiEmbedding001)?;

    let embedding =
        embed_client.embed_content().with_text("Vertex AI streaming now works").execute().await?;

    let dim = embedding.embedding.values.len();
    info!(dimensions = dim, "embedding received");
    assert!(dim > 0, "embedding should have dimensions");
    info!("✅ Embedding works ({dim} dimensions)");

    info!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    info!("🎉 All Vertex AI backend tests passed!");
    Ok(())
}

fn build_vertex_client(project: &str, location: &str) -> Result<Gemini, adk_gemini::ClientError> {
    build_vertex_client_with_model(project, location, Model::default())
}

fn build_vertex_client_with_model(
    project: &str,
    location: &str,
    model: Model,
) -> Result<Gemini, adk_gemini::ClientError> {
    // Try service account JSON file first
    if let Ok(sa_path) = env::var("GOOGLE_APPLICATION_CREDENTIALS") {
        let sa_json = std::fs::read_to_string(&sa_path)
            .unwrap_or_else(|e| panic!("failed to read {sa_path}: {e}"));
        info!(path = %sa_path, "using service account JSON");
        return Gemini::with_google_cloud_service_account_json(&sa_json, project, location, model);
    }

    // Try ADC
    if env::var("VERTEX_USE_ADC").is_ok() {
        info!("using Application Default Credentials");
        return GeminiBuilder::new("")
            .with_model(model)
            .with_google_cloud(project, location)
            .with_google_cloud_adc()?
            .build();
    }

    // Fall back to API key
    let api_key = env::var("GEMINI_API_KEY").expect(
        "one of GOOGLE_APPLICATION_CREDENTIALS, VERTEX_USE_ADC, or GEMINI_API_KEY required",
    );
    info!("using API key authentication");
    Gemini::with_google_cloud_model(api_key, project, location, model)
}
