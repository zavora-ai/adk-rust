//! # LiveKit WebRTC Bridge Example
//!
//! Demonstrates bridging a LiveKit room with a realtime AI model using the
//! `adk-realtime` LiveKit bridge module. Shows how to:
//!
//! - Connect to a LiveKit room
//! - Wrap an event handler with [`LiveKitEventHandler`] to publish model audio
//! - Use [`bridge_input`] to feed participant audio to the AI model
//! - Run the event loop via [`RealtimeRunner`]
//!
//! ## Prerequisites
//!
//! 1. A running [LiveKit](https://livekit.io/) server (local or cloud).
//! 2. An OpenAI API key with realtime access (or swap for Gemini).
//! 3. The `livekit` and `openai` features enabled for `adk-realtime`.
//!
//! ## Environment Variables
//!
//! | Variable          | Required | Description                                      |
//! |-------------------|----------|--------------------------------------------------|
//! | `OPENAI_API_KEY`  | **Yes**  | OpenAI API key with realtime model access        |
//! | `LIVEKIT_URL`     | **Yes**  | LiveKit server WebSocket URL (e.g. `ws://localhost:7880`) |
//! | `LIVEKIT_TOKEN`   | **Yes**  | LiveKit access token for the room                |
//!
//! ## Running
//!
//! ```sh
//! cargo run -p adk-realtime --example livekit_bridge --features "livekit,openai"
//! ```
//!
//! ## Architecture
//!
//! ```text
//! ┌──────────────┐     audio frames      ┌──────────────────┐
//! │  LiveKit Room │ ──────────────────▶  │  bridge_input()  │
//! │  (participant) │  RemoteAudioTrack   │  PCM16 → Runner  │
//! └──────────────┘                       └────────┬─────────┘
//!                                                 │
//!                                                 ▼
//!                                        ┌──────────────────┐
//!                                        │ RealtimeRunner   │
//!                                        │ (OpenAI session) │
//!                                        └────────┬─────────┘
//!                                                 │
//!                                                 ▼
//! ┌──────────────┐     audio publish     ┌──────────────────────┐
//! │  LiveKit Room │ ◀────────────────── │ LiveKitEventHandler  │
//! │  (AI agent)   │  NativeAudioSource  │ wraps inner handler  │
//! └──────────────┘                       └──────────────────────┘
//! ```
//!
//! ## Note
//!
//! This example requires a real LiveKit server and room. The `connect_to_livekit()`
//! function below shows the setup pattern — you'll need to adapt it to your
//! LiveKit deployment. See <https://docs.livekit.io/> for setup instructions.

use std::sync::Arc;

use adk_realtime::RealtimeConfig;
use adk_realtime::livekit::{LiveKitConfig, LiveKitEventHandler, LiveKitRoomBuilder, bridge_input};
use adk_realtime::openai::OpenAIRealtimeModel;
use adk_realtime::runner::{EventHandler, RealtimeRunner};
use livekit::prelude::*;

/// A simple event handler that prints text and transcript events.
struct PrintingEventHandler;

#[async_trait::async_trait]
impl EventHandler for PrintingEventHandler {
    async fn on_text(&self, text: &str, _item_id: &str) -> adk_realtime::Result<()> {
        print!("{text}");
        Ok(())
    }

    async fn on_transcript(&self, transcript: &str, _item_id: &str) -> adk_realtime::Result<()> {
        print!("[transcript] {transcript}");
        Ok(())
    }

    async fn on_response_done(&self) -> adk_realtime::Result<()> {
        println!("\n--- Response complete ---");
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("Failed to install rustls default crypto provider");
    tracing_subscriber::fmt::init();

    // --- 1. Create the OpenAI realtime model ---
    let api_key = std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY env var is required");
    let model = OpenAIRealtimeModel::new(api_key, "gpt-4o-realtime-preview-2024-12-17");

    // --- 2. Build the LiveKit Config ---
    let lk_url = std::env::var("LIVEKIT_URL").expect("LIVEKIT_URL is required");
    let lk_api_key = std::env::var("LIVEKIT_API_KEY").expect("LIVEKIT_API_KEY is required");
    let lk_api_secret =
        std::env::var("LIVEKIT_API_SECRET").expect("LIVEKIT_API_SECRET is required");

    let lk_config = LiveKitConfig::new(lk_url, lk_api_key, lk_api_secret)?;

    let bundle = LiveKitRoomBuilder::new(lk_config)
        .identity("openai-agent-01")
        .name("OpenAI Agent")
        .room_name("my-room")
        .auto_subscribe(true)
        .with_audio(24000, 1)
        .connect()
        .await?;

    let _room = bundle.room;
    let mut room_events = bundle.events;
    let audio_source = bundle.audio_source.expect("Audio source was not created by builder");

    // --- 3. Wrap event handler with LiveKit audio output ---
    // The LiveKitEventHandler intercepts on_audio to push model audio to the
    // NativeAudioSource, which publishes it to the LiveKit room.
    let inner_handler = PrintingEventHandler;
    let lk_handler = LiveKitEventHandler::new(inner_handler, audio_source, 24000, 1);

    // --- 4. Build the RealtimeRunner ---
    let config = RealtimeConfig::default()
        .with_instruction("You are a helpful voice assistant in a LiveKit room.")
        .with_voice("alloy");

    let runner = Arc::new(
        RealtimeRunner::builder()
            .model(Arc::new(model))
            .config(config)
            .event_handler(lk_handler)
            .build()?,
    );

    // --- 5. Connect the runner to the AI model ---
    runner.connect().await?;
    println!("Connected to OpenAI Realtime API.");

    // --- 6. Bridge incoming participant audio to the model (in background) ---
    let bridge_runner = Arc::clone(&runner);
    let bridge_handle = tokio::spawn(async move {
        while let Some(event) = room_events.recv().await {
            if let RoomEvent::TrackSubscribed { track: RemoteTrack::Audio(audio_track), .. } = event
            {
                tracing::info!("Subscribed to remote audio track. Bridging input...");
                let r = bridge_runner.clone();
                tokio::spawn(async move {
                    if let Err(e) = bridge_input(audio_track, &r).await {
                        tracing::error!("Bridge error: {e}");
                    }
                });
            }
        }
    });

    // --- 7. Run the event loop ---
    // This processes model responses and routes them through the
    // LiveKitEventHandler (which publishes audio back to the room).
    println!("Running event loop — speak into the LiveKit room...\n");
    if let Err(e) = runner.run().await {
        eprintln!("Runner error: {e}");
    }

    bridge_handle.abort();
    runner.close().await?;
    println!("Session closed.");
    Ok(())
}
