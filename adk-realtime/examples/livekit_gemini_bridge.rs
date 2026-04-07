//! # LiveKit WebRTC Bridge Example (Gemini Live)
//!
//! Demonstrates bridging a LiveKit room with the Gemini Live realtime AI model using the
//! `adk-realtime` LiveKit bridge module. Shows how to:
//!
//! - Use `bridge_gemini_input` to resample participant audio (24kHz to 16kHz) for the AI model
//! - Automatically inject a text message to prompt the AI to speak
//!
//! ## Prerequisites
//!
//! 1. A running [LiveKit](https://livekit.io/) server.
//! 2. A Gemini Studio API key with realtime access.
//! 3. The `livekit` and `gemini` features enabled for `adk-realtime`.
//!
//! ## Environment Variables
//!
//! | Variable          | Required | Description                                      |
//! |-------------------|----------|--------------------------------------------------|
//! | `GEMINI_API_KEY`  | **Yes**  | Gemini Studio API key                            |
//! | `LIVEKIT_URL`     | **Yes**  | LiveKit server WebSocket URL (e.g. `ws://localhost:7880`) |
//! | `LIVEKIT_API_KEY` | **Yes**  | LiveKit server API Key                           |
//! | `LIVEKIT_API_SECRET`| **Yes**| LiveKit server API Secret                        |
//!
//! ## Running
//!
//! ```sh
//! cargo run -p adk-realtime --example livekit_gemini_bridge --features "livekit,gemini"
//! ```

use std::sync::Arc;
use tokio::time::{Duration, sleep};

use adk_realtime::RealtimeConfig;
use adk_realtime::gemini::{GeminiLiveBackend, GeminiRealtimeModel};
use adk_realtime::livekit::prelude::*;
use adk_realtime::livekit::{
    LiveKitConfig, LiveKitEventHandler, LiveKitRoomBuilder, bridge_gemini_input,
};
use adk_realtime::runner::{EventHandler, RealtimeRunner};

/// A simple event handler that prints text and transcript events.
struct PrintingEventHandler;

#[async_trait::async_trait]
impl EventHandler for PrintingEventHandler {
    async fn on_text(&self, text: &str, _item_id: &str) -> adk_realtime::Result<()> {
        print!("{text}");
        Ok(())
    }

    async fn on_transcript(&self, transcript: &str, _item_id: &str) -> adk_realtime::Result<()> {
        print!("{transcript}");
        Ok(())
    }

    async fn on_response_done(&self) -> adk_realtime::Result<()> {
        println!("\n\n--- [AI Finished Speaking] ---");
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Setup basic tracing so we can see the internal connection logs
    tracing_subscriber::fmt::init();

    // Initialize rustls explicitly when `ring` is used.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    // Gemini Live expects a 16kHz audio sample rate (as opposed to OpenAI's 24kHz)
    const GEMINI_SAMPLE_RATE: u32 = 16000;
    const NUM_CHANNELS: u32 = 1;

    // --- 1. Create the Gemini realtime model ---
    let api_key = std::env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY env var is required");

    // Explicitly use the 2.5 flash native audio model as requested by user.
    let model_name = std::env::var("GEMINI_MODEL")
        .unwrap_or_else(|_| "models/gemini-2.5-flash-native-audio-latest".to_string());
    let backend = GeminiLiveBackend::studio(api_key);
    let model = GeminiRealtimeModel::new(backend, model_name);

    // --- 2. Build the LiveKit Config ---
    let lk_url = std::env::var("LIVEKIT_URL").expect("LIVEKIT_URL is required");
    let lk_api_key = std::env::var("LIVEKIT_API_KEY").expect("LIVEKIT_API_KEY is required");
    let lk_api_secret =
        std::env::var("LIVEKIT_API_SECRET").expect("LIVEKIT_API_SECRET is required");

    let lk_config = LiveKitConfig::new(lk_url, lk_api_key, lk_api_secret)?;

    let bundle = LiveKitRoomBuilder::new(lk_config)
        .identity("gemini-agent-01")
        .name("Gemini Agent")
        .room_name("my-room")
        .auto_subscribe(true)
        .with_audio(GEMINI_SAMPLE_RATE, NUM_CHANNELS)
        .connect()
        .await?;

    let room = bundle.room;
    let mut room_events = bundle.events;
    let audio_source = bundle.audio_source.expect("Audio source was not created by builder");
    let _audio_track = bundle.audio_track.expect("Audio track was not created by builder");

    tracing::info!("Connected to LiveKit room '{}' and published audio track.", room.name());

    // --- 4. Wrap event handler with LiveKit audio output ---
    // The LiveKitEventHandler intercepts `on_audio` events emitted by the
    // RealtimeRunner and pushes those PCM bytes to the NativeAudioSource.
    let inner_handler = PrintingEventHandler;
    let lk_handler =
        LiveKitEventHandler::new(inner_handler, audio_source, GEMINI_SAMPLE_RATE, NUM_CHANNELS);

    // --- 5. Build the RealtimeRunner ---
    let config = RealtimeConfig::default()
        .with_instruction("You are a helpful voice assistant in a LiveKit room.")
        .with_voice("Aoede"); // Use a Gemini 3.1 specific voice

    let runner = Arc::new(
        RealtimeRunner::builder()
            .model(Arc::new(model))
            .config(config)
            .event_handler(lk_handler)
            .build()?,
    );

    // --- 6. Connect the runner to the AI model ---
    runner.connect().await?;
    tracing::info!("Connected to Gemini Live BIDI API.");

    // --- 7. Bridge incoming participant audio to the model (in background) ---
    let bridge_runner = Arc::clone(&runner);
    let bridge_handle = tokio::spawn(async move {
        // IMPORTANT: Gemini closes the WebSocket if it receives audio (RealtimeInput)
        // before the SetupComplete message. Give the runner loop a second to
        // complete the handshake before we start bridging LiveKit audio.
        tokio::time::sleep(tokio::time::Duration::from_millis(1500)).await;

        while let Some(event) = room_events.recv().await {
            if let RoomEvent::TrackSubscribed { track: RemoteTrack::Audio(audio_track), .. } = event
            {
                tracing::info!("Subscribed to remote audio track. Bridging input...");
                let r = bridge_runner.clone();
                tokio::spawn(async move {
                    if let Err(e) = bridge_gemini_input(audio_track, &r).await {
                        tracing::error!("Bridge error: {e}");
                    }
                });
            }
        }
    });

    // --- 8. Run the event loop ---
    tracing::info!("Running agent event loop (waiting for response)...\n");
    let runner_clone = Arc::clone(&runner);
    let runner_handle = tokio::spawn(async move {
        if let Err(e) = runner_clone.run().await {
            tracing::error!("Runner error: {e}");
        }
    });

    // Short delay to let sockets stabilize
    sleep(Duration::from_millis(500)).await;

    // --- 9. Inject a prompt to trigger the model ---
    tracing::info!("Injecting automated prompt to trigger Gemini speech...");
    runner.send_text("Hello! Can you hear me? Please say 'Connection successful!' loudly so I know you are there.").await?;

    // Let the agent speak for 15 seconds, then exit gracefully
    sleep(Duration::from_secs(15)).await;

    tracing::info!("Test complete. Closing session.");
    bridge_handle.abort();
    runner.close().await?;
    runner_handle.abort();

    Ok(())
}
