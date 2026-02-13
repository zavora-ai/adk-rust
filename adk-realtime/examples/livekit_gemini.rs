//! LiveKit integration with Gemini Live.
//!
//! This example demonstrates how to bridge audio between a LiveKit room and Gemini Live.
//! It uses the `livekit` crate to connect to a room and `adk-realtime` to manage the
//! Gemini session.
//!
//! # Prerequisites
//!
//! - Set `LIVEKIT_URL`, `LIVEKIT_API_KEY`, and `LIVEKIT_API_SECRET`.
//! - Set `GOOGLE_API_KEY` for Gemini.
//! - Run with: `cargo run --example livekit_gemini --features "livekit gemini"`

use adk_gemini::GeminiLiveBackend;
use adk_realtime::gemini::GeminiRealtimeModel;
use adk_realtime::livekit::{LiveKitEventHandler, bridge_gemini_input};
use adk_realtime::{RealtimeConfig, RealtimeRunner};
use livekit::prelude::*;
use livekit::webrtc::audio_source::native::NativeAudioSource;
use std::env;
use std::sync::Arc;
use tokio::signal;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    // 1. Load configuration
    let livekit_url = env::var("LIVEKIT_URL").expect("LIVEKIT_URL required");
    let api_key = env::var("LIVEKIT_API_KEY").expect("LIVEKIT_API_KEY required");
    let api_secret = env::var("LIVEKIT_API_SECRET").expect("LIVEKIT_API_SECRET required");
    let google_key = env::var("GOOGLE_API_KEY").expect("GOOGLE_API_KEY required");

    // 2. Connect to LiveKit
    println!("Connecting to LiveKit: {}", livekit_url);
    let (room, mut events) = Room::connect(
        &livekit_url,
        &api_key,
        &api_secret,
        RoomOptions {
            auto_subscribe: true,
            adaptive_stream: false,
            dynacast: false,
            ..Default::default()
        },
    )
    .await?;
    println!("Connected to room: {}", room.name());

    // 3. Create a local audio track for the agent's voice
    let source = NativeAudioSource::new(
        livekit::webrtc::audio_source::native::AudioSourceOptions::default(),
        48000,
        1,
    );
    let track =
        LocalAudioTrack::create_audio_track("agent_voice", RtcAudioSource::Native(source.clone()));
    room.local_participant()
        .publish_track(LocalTrack::Audio(track), TrackPublishOptions::default())
        .await?;

    // 4. Initialize Gemini Realtime Runner
    let backend = GeminiLiveBackend::Studio { api_key: google_key };
    let model = GeminiRealtimeModel::new(backend, "models/gemini-live-2.5-flash-native-audio");

    // Create event handler that pipes Gemini audio to LiveKit
    // We pass a NoOp inner handler for events we don't need to log/process further
    struct LogEventHandler;
    #[async_trait::async_trait]
    impl adk_realtime::runner::EventHandler for LogEventHandler {
        async fn on_audio(&self, _audio: &[u8], _item_id: &str) -> adk_realtime::Result<()> {
            Ok(())
        }
        async fn on_text(&self, text: &str, _item_id: &str) -> adk_realtime::Result<()> {
            println!("Agent: {}", text);
            Ok(())
        }
        async fn on_transcript(
            &self,
            transcript: &str,
            _item_id: &str,
        ) -> adk_realtime::Result<()> {
            println!("User: {}", transcript);
            Ok(())
        }
        async fn on_error(&self, error: &adk_realtime::RealtimeError) -> adk_realtime::Result<()> {
            eprintln!("Error: {}", error);
            Ok(())
        }
        // Implement other methods as no-ops...
        async fn on_speech_started(&self, _ms: u64) -> adk_realtime::Result<()> {
            Ok(())
        }
        async fn on_speech_stopped(&self, _ms: u64) -> adk_realtime::Result<()> {
            Ok(())
        }
        async fn on_response_done(&self) -> adk_realtime::Result<()> {
            Ok(())
        }
    }

    let event_handler = LiveKitEventHandler::new(source, Arc::new(LogEventHandler));

    let runner = Arc::new(
        RealtimeRunner::builder()
            .model(Arc::new(model))
            .config(
                RealtimeConfig::default()
                    .with_instruction("You are a helpful assistant in a LiveKit room."),
            )
            .event_handler(event_handler)
            .build()?,
    );

    // Connect to Gemini
    println!("Connecting to Gemini...");
    runner.connect().await?;
    println!("Connected to Gemini Live");

    // Start the runner loop in background
    let runner_clone = runner.clone();
    tokio::spawn(async move {
        if let Err(e) = runner_clone.run().await {
            eprintln!("Runner error: {}", e);
        }
    });

    // 5. Handle LiveKit events to bridge input audio
    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            match event {
                RoomEvent::TrackSubscribed { track, publication: _, participant: _ } => {
                    if let RemoteTrack::Audio(audio_track) = track {
                        println!("Subscribed to audio track");
                        // Bridge this track to Gemini
                        bridge_gemini_input(audio_track, runner.clone());
                    }
                }
                _ => {}
            }
        }
    });

    // Keep running until Ctrl+C
    match signal::ctrl_c().await {
        Ok(()) => {
            println!("Shutting down...");
        }
        Err(err) => {
            eprintln!("Unable to listen for shutdown signal: {}", err);
        }
    }

    room.close().await?;
    Ok(())
}
