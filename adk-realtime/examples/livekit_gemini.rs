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
//! 2. A Gemini API key with realtime access.
//! 3. The `livekit` and `gemini` features enabled for `adk-realtime`.
//!
//! ## Environment Variables
//!
//! | Variable          | Required | Description                                      |
//! |-------------------|----------|--------------------------------------------------|
//! | `GEMINI_API_KEY`  | **Yes**  | Gemini API key with realtime model access        |
//! | `LIVEKIT_URL`     | **Yes**  | LiveKit server WebSocket URL (e.g. `ws://localhost:7880`) |
//! | `LIVEKIT_TOKEN`   | **Yes**  | LiveKit access token for the room                |
//!
//! ## Running
//!
//! ```sh
//! cargo run -p adk-realtime --example livekit_gemini --features "livekit,gemini"
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
//!                                        │ (Gemini session) │
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
use adk_realtime::gemini::{GeminiLiveBackend, GeminiRealtimeModel};
use adk_realtime::livekit::{LiveKitEventHandler, bridge_input};
use adk_realtime::runner::{EventHandler, RealtimeRunner};

use livekit::options::TrackPublishOptions;
use livekit::prelude::*;
use livekit::webrtc::audio_source::native::NativeAudioSource;
use livekit::webrtc::audio_source::{AudioSourceOptions, RtcAudioSource};

/// Connect to a LiveKit room and return the room handle, a native audio source
/// for publishing model audio, and the first remote audio track from a participant.
///
/// In a production app you'd handle track subscriptions more robustly.
async fn connect_to_livekit()
-> Result<(Room, NativeAudioSource, livekit::track::RemoteAudioTrack), Box<dyn std::error::Error>> {
    let url = std::env::var("LIVEKIT_URL").expect("LIVEKIT_URL env var is required");
    let token = std::env::var("LIVEKIT_TOKEN").expect("LIVEKIT_TOKEN env var is required");

    // --- Connect to the room ---
    let (room, mut room_events) = Room::connect(&url, &token, RoomOptions::default()).await?;
    println!("Connected to LiveKit room: {}", room.name());

    // --- Create a native audio source for publishing model audio ---
    let audio_source = NativeAudioSource::new(
        AudioSourceOptions::default(),
        24000, // 24kHz for Gemini
        1,     // mono
        100,   // queue_size_ms
    );

    // Publish the audio source as a local track
    let rtc_source = RtcAudioSource::Native(audio_source.clone());
    let local_track = LocalAudioTrack::create_audio_track("ai-agent-audio", rtc_source);
    let publish_options = TrackPublishOptions::default();
    room.local_participant().publish_track(LocalTrack::Audio(local_track), publish_options).await?;
    println!("Published AI agent audio track to room.");

    // --- Wait for a remote participant's audio track ---
    println!("Waiting for a remote participant's audio track...");
    let remote_track = loop {
        if let Some(RoomEvent::TrackSubscribed { track: RemoteTrack::Audio(audio_track), .. }) =
            room_events.recv().await
        {
            println!("Subscribed to remote audio track.");
            break audio_track;
        }
    };

    Ok((room, audio_source, remote_track))
}

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
    // --- 1. Create the Gemini realtime model ---
    let api_key = std::env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY env var is required");
    let backend = GeminiLiveBackend::Studio { api_key };
    let model =
        GeminiRealtimeModel::new(backend, "models/gemini-2.5-flash-native-audio-preview-12-2025");

    // --- 2. Connect to LiveKit room ---
    let (_room, audio_source, remote_track) = connect_to_livekit().await?;

    // --- 3. Wrap event handler with LiveKit audio output ---
    // The LiveKitEventHandler intercepts on_audio to push model audio to the
    // NativeAudioSource, which publishes it to the LiveKit room.
    let inner_handler = PrintingEventHandler;
    let lk_handler = LiveKitEventHandler::new(inner_handler, audio_source, 24000, 1);

    // --- 4. Build the RealtimeRunner ---
    let config = RealtimeConfig::default()
        .with_instruction("You are a helpful voice assistant in a LiveKit room.")
        .with_voice("Aura");

    let runner = Arc::new(
        RealtimeRunner::builder()
            .model(Arc::new(model))
            .config(config)
            .event_handler(lk_handler)
            .build()?,
    );

    // --- 5. Connect the runner to the AI model ---
    runner.connect().await?;
    println!("Connected to Gemini Realtime API.");

    // --- 6. Bridge participant audio to the model ---
    // Spawn a task that reads audio from the remote participant's track
    // and feeds it to the AI model via the runner.
    let bridge_runner = Arc::clone(&runner);
    let bridge_handle = tokio::spawn(async move {
        if let Err(e) = bridge_input(remote_track, &bridge_runner).await {
            eprintln!("Bridge input error: {e}");
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
