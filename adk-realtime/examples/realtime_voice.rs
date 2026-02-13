//! Gemini Live Voice Test
//!
//! This example demonstrates a direct connection to the Gemini Live API
//! using the adk-realtime crate, sending text input and receiving audio (TTS) output.
//!
//! # Usage
//!
//! ```bash
//! export GOOGLE_API_KEY="your-api-key"
//! cargo run --example realtime_voice --features gemini
//! ```
//!
//! This example:
//! 1. Connects to the Gemini Live WebSocket API
//! 2. Sends a text prompt
//! 3. Receives and displays audio response events (TTS)
//! 4. Closes the session

use adk_gemini::GeminiLiveBackend;
use adk_realtime::config::RealtimeConfig;
use adk_realtime::events::ServerEvent;
use adk_realtime::gemini::GeminiRealtimeModel;
use adk_realtime::model::RealtimeModel;

use std::io::Write;
use std::process::ExitCode;
use tracing::{error, info, warn};

const TEST_PROMPT: &str = "Hello! Please introduce yourself briefly.";
const OUTPUT_FILE: &str = "gemini_tts.pcm";

async fn run_realtime_test(api_key: &str) -> Result<(), Box<dyn std::error::Error>> {
    info!("Initializing Gemini Live connection...");

    // 1. Configure the backend (Public API with API Key)
    let backend = GeminiLiveBackend::Studio { api_key: api_key.to_string() };

    // 2. Create the realtime model
    let model =
        GeminiRealtimeModel::new(backend, "models/gemini-2.5-flash-native-audio-preview-12-2025");
    info!(model_id = model.model_id(), provider = model.provider(), "Model configured");

    // 3. Build config with system instruction
    let config = RealtimeConfig::default()
        .with_instruction("You are a friendly assistant. Respond naturally and concisely.");

    // 4. Connect to the Live API
    info!("Connecting to Gemini Live API...");
    let session = model.connect(config).await?;
    info!(session_id = session.session_id(), "Connected successfully!");

    // 5. Send text input
    info!(prompt = TEST_PROMPT, "Sending text prompt...");
    session.send_text(TEST_PROMPT).await?;

    // 5. Create output file
    let mut file = std::fs::File::create(OUTPUT_FILE)?;
    info!(file = OUTPUT_FILE, "Created output file for audio");

    // 6. Receive and process events
    info!("Waiting for response events...");
    let mut audio_chunks_received = 0;
    let mut text_received = String::new();

    // Note: Gemini Live generates responses automatically after input
    let timeout = tokio::time::Duration::from_secs(30);
    let start = tokio::time::Instant::now();

    loop {
        if start.elapsed() > timeout {
            warn!("Timeout waiting for response");
            break;
        }

        match tokio::time::timeout(tokio::time::Duration::from_secs(5), session.next_event()).await
        {
            Ok(Some(Ok(event))) => match event {
                ServerEvent::SessionCreated { .. } => {
                    info!("Session created");
                }
                ServerEvent::SpeechStarted { .. } => {
                    info!("🎤 Agent started speaking...");
                }
                ServerEvent::SpeechStopped { .. } => {
                    info!("🔇 Agent stopped speaking");
                }
                ServerEvent::AudioDelta { delta, item_id, .. } => {
                    audio_chunks_received += 1;
                    // delta is already Vec<u8> decoded by session
                    file.write_all(&delta)?;
                    let bytes = delta.len();
                    info!(
                        chunk = audio_chunks_received,
                        bytes = bytes,
                        item_id = %item_id,
                        "📢 Received audio chunk (24kHz PCM)"
                    );
                }
                ServerEvent::TextDelta { delta, .. } => {
                    text_received.push_str(&delta);
                    info!(text = %delta, "📝 Received text delta");
                }
                ServerEvent::ResponseDone { .. } => {
                    info!("✅ Response complete!");
                    break;
                }
                ServerEvent::Error { error, .. } => {
                    error!(
                        error = %error.message,
                        type = %error.error_type,
                        "❌ Server error"
                    );
                    break;
                }
                _ => {}
            },
            Ok(Some(Err(e))) => {
                error!(error = %e, "Event error");
                break;
            }
            Ok(None) => {
                info!("Session closed by server");
                break;
            }
            Err(_) => {
                // Timeout on single event, continue waiting
                continue;
            }
        }
    }

    // 7. Print summary
    info!("========================================");
    info!("📊 VERIFICATION SUMMARY");
    info!("========================================");
    info!(audio_chunks = audio_chunks_received, "Total audio chunks received (TTS output)");
    if !text_received.is_empty() {
        info!(text = %text_received, "Text response");
    }

    if audio_chunks_received > 0 {
        info!("✅ SUCCESS: Gemini Live API returned audio (TTS) response!");
    } else if !text_received.is_empty() {
        info!("⚠️  PARTIAL: Received text but no audio. Check modalities configuration.");
    } else {
        error!("❌ FAILED: No response received from Gemini Live API.");
    }

    // 8. Close session
    session.close().await?;
    info!("Session closed");

    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::builder()
                .with_default_directive(tracing::level_filters::LevelFilter::INFO.into())
                .from_env_lossy(),
        )
        .init();

    // Get API key
    let api_key = match std::env::var("GOOGLE_API_KEY") {
        Ok(key) => key,
        Err(_) => {
            error!("GOOGLE_API_KEY environment variable is required");
            error!("Set it with: export GOOGLE_API_KEY=\"your-api-key\"");
            return ExitCode::FAILURE;
        }
    };

    info!("🚀 Starting Gemini Live Voice Test");
    info!("API Key: {}...{}", &api_key[..4], &api_key[api_key.len() - 4..]);

    match run_realtime_test(&api_key).await {
        Ok(()) => {
            info!("🎉 Test completed successfully!");
            ExitCode::SUCCESS
        }
        Err(e) => {
            error!(error = %e, "Test failed");
            ExitCode::FAILURE
        }
    }
}
