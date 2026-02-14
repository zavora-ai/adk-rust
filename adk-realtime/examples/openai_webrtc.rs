//! # OpenAI WebRTC Realtime Example
//!
//! Demonstrates using WebRTC transport for lower-latency audio with OpenAI's
//! Realtime API. This example connects via WebRTC (instead of WebSocket),
//! sends a text prompt, and prints response events.
//!
//! ## Prerequisites
//!
//! 1. An OpenAI API key with access to the Realtime API.
//! 2. `cmake` installed on your system (required by the `audiopus` crate to
//!    build the Opus C library from source).
//! 3. The `openai-webrtc` feature enabled for `adk-realtime`.
//!
//! ## Environment Variables
//!
//! | Variable         | Required | Description                              |
//! |------------------|----------|------------------------------------------|
//! | `OPENAI_API_KEY` | **Yes**  | OpenAI API key with realtime access      |
//!
//! ## Running
//!
//! ```sh
//! cargo run -p adk-realtime --example openai_webrtc --features openai-webrtc
//! ```
//!
//! ## How It Works
//!
//! 1. Creates an `OpenAIRealtimeModel` with `OpenAITransport::WebRTC`.
//! 2. The `connect()` call performs SDP signaling with OpenAI's endpoint:
//!    - Generates a local SDP offer with an audio track and data channel
//!    - Obtains an ephemeral token from OpenAI
//!    - Exchanges the SDP offer/answer to establish the WebRTC connection
//! 3. Audio flows over WebRTC media tracks (Opus-encoded), while JSON events
//!    (text, tool calls, session updates) flow over the "oai-events" data channel.
//! 4. The `RealtimeSession` trait abstracts this — you use the same `send_text`,
//!    `next_event`, etc. methods as with WebSocket transport.

use adk_realtime::openai::{OpenAIRealtimeModel, OpenAITransport};
use adk_realtime::{RealtimeConfig, RealtimeModel, ServerEvent};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // --- 1. Create the model with WebRTC transport ---
    let api_key = std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY env var is required");

    let model = OpenAIRealtimeModel::new(api_key, "gpt-4o-realtime-preview-2024-12-17")
        .with_transport(OpenAITransport::WebRTC);

    // --- 2. Configure the session ---
    let config = RealtimeConfig::default()
        .with_instruction("You are a helpful voice assistant. Keep responses concise.")
        .with_voice("alloy");

    // --- 3. Connect via WebRTC ---
    println!("Connecting to OpenAI Realtime API via WebRTC...");
    let session = model.connect(config).await?;
    println!("Connected! Session ID: {}", session.session_id());

    // --- 4. Send a text prompt ---
    session.send_text("Hello! Tell me a short joke.").await?;
    println!("Sent text prompt, waiting for response...\n");

    // --- 5. Process response events ---
    while let Some(event) = session.next_event().await {
        match event? {
            ServerEvent::TextDelta { delta, .. } => {
                // Print text as it streams in
                print!("{delta}");
            }
            ServerEvent::AudioDelta { delta, .. } => {
                // Log audio chunk sizes (in a real app you'd decode and play these)
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

    // --- 6. Clean up ---
    session.close().await?;
    println!("Session closed.");
    Ok(())
}
