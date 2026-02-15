//! # Vertex AI Live Voice Assistant Example
//!
//! Demonstrates connecting to the Gemini Live API via Vertex AI with
//! Application Default Credentials (ADC), sending a text prompt, and
//! printing response events (text deltas and audio chunk sizes).
//!
//! ## Prerequisites
//!
//! 1. A Google Cloud project with the **Vertex AI API** enabled.
//! 2. Application Default Credentials configured:
//!    ```sh
//!    gcloud auth application-default login
//!    ```
//! 3. The `vertex-live` feature enabled for `adk-realtime`.
//!
//! ## Environment Variables
//!
//! | Variable               | Required | Description                                  |
//! |------------------------|----------|----------------------------------------------|
//! | `GOOGLE_CLOUD_PROJECT` | **Yes**  | Your Google Cloud project ID                 |
//! | `GOOGLE_CLOUD_REGION`  | No       | GCP region (defaults to `us-central1`)       |
//!
//! ## Running
//!
//! ```sh
//! cargo run -p adk-realtime --example vertex_live_voice --features vertex-live
//! ```

use adk_realtime::gemini::{GeminiLiveBackend, GeminiRealtimeModel};
use adk_realtime::{RealtimeConfig, RealtimeModel, ServerEvent};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // --- 1. Load credentials via Application Default Credentials (ADC) ---
    let credentials = google_cloud_auth::credentials::Builder::default().build()?;

    // --- 2. Build the Vertex AI Live backend ---
    let region = std::env::var("GOOGLE_CLOUD_REGION").unwrap_or_else(|_| "us-central1".into());
    let project_id =
        std::env::var("GOOGLE_CLOUD_PROJECT").expect("GOOGLE_CLOUD_PROJECT env var is required");

    let backend = GeminiLiveBackend::Vertex { credentials, region, project_id };

    // --- 3. Create the model and session configuration ---
    let model = GeminiRealtimeModel::new(backend, "models/gemini-live-2.5-flash-native-audio");
    let config = RealtimeConfig::default()
        .with_instruction("You are a helpful voice assistant. Keep responses concise.");

    // --- 4. Connect to Vertex AI Live ---
    println!("Connecting to Vertex AI Live...");
    let session = model.connect(config).await?;
    println!("Connected! Session ID: {}", session.session_id());

    // --- 5. Send a text prompt ---
    session.send_text("Hello! What can you help me with today?").await?;
    println!("Sent text prompt, waiting for response...\n");

    // --- 6. Process response events ---
    while let Some(event) = session.next_event().await {
        match event? {
            ServerEvent::TextDelta { delta, .. } => {
                // Print text as it streams in
                print!("{delta}");
            }
            ServerEvent::AudioDelta { delta, .. } => {
                // Log audio chunk sizes (in a real app you'd play these)
                println!("[audio chunk: {} bytes]", delta.len());
            }
            ServerEvent::TranscriptDelta { delta, .. } => {
                print!("[transcript] {delta}");
            }
            ServerEvent::ResponseDone { .. } => {
                println!("\n--- Response complete ---");
                break;
            }
            ServerEvent::Error { error, .. } => {
                eprintln!("\nError from server: {} - {}", error.error_type, error.message);
                break;
            }
            _ => {
                // Ignore other event types
            }
        }
    }

    // --- 7. Clean up ---
    session.close().await?;
    println!("Session closed.");
    Ok(())
}
