//! Voice Assistant example with Server-side Voice Activity Detection.
//!
//! This example demonstrates a voice assistant using the OpenAI Realtime API
//! with server-side VAD (Voice Activity Detection). The server automatically
//! detects when the user starts/stops speaking.
//!
//! Set OPENAI_API_KEY environment variable before running:
//! ```bash
//! export OPENAI_API_KEY=sk-...
//! cargo run --example realtime_vad --features realtime-openai
//! ```

use adk_realtime::{
    AudioFormat, RealtimeConfig, RealtimeModel, ServerEvent, VadConfig, VadMode,
    openai::OpenAIRealtimeModel,
};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Load environment variables
    dotenvy::dotenv().ok();

    let api_key = std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY must be set");

    println!("=== ADK-Rust Voice Assistant with VAD ===\n");

    // Create the OpenAI Realtime model
    let model = Arc::new(OpenAIRealtimeModel::new(&api_key, "gpt-4o-realtime-preview-2024-12-17"));

    // Configure with server-side VAD
    let vad_config = VadConfig {
        mode: VadMode::ServerVad,
        threshold: Some(0.5),           // Speech detection threshold (0.0 - 1.0)
        prefix_padding_ms: Some(300),   // Audio to include before speech detected
        silence_duration_ms: Some(500), // Silence duration to end turn
        interrupt_response: Some(true), // Allow interrupting assistant
        eagerness: None,
    };

    let config = RealtimeConfig::default()
        .with_instruction(
            "You are a friendly voice assistant named Nova. \
             Speak naturally and conversationally. \
             Keep responses brief and engaging.",
        )
        .with_voice("alloy")
        .with_vad(vad_config)
        .with_modalities(vec!["text".to_string(), "audio".to_string()]);

    println!("Connecting to OpenAI Realtime API with VAD enabled...");

    let session = model.connect(config).await?;

    println!("Connected! Voice Activity Detection is active.\n");
    println!("In a real application, you would:");
    println!("  1. Capture microphone audio");
    println!("  2. Send audio chunks via session.send_audio()");
    println!("  3. The server detects speech automatically");
    println!("  4. Receive audio responses to play through speakers\n");

    // Simulate sending audio by sending a text message instead
    // In a real app, you would send actual audio data
    session.send_text("Hello Nova! How are you today?").await?;
    session.create_response().await?;

    println!("User: Hello Nova! How are you today?\n");

    let mut transcript = String::new();
    let mut audio_chunks_received = 0;

    // Process events
    while let Some(event_result) = session.next_event().await {
        match event_result {
            Ok(event) => match event {
                ServerEvent::SessionCreated { session: sess, .. } => {
                    if let Some(id) = sess.get("id").and_then(|v| v.as_str()) {
                        println!("[Session: {}]\n", id);
                    }
                }
                ServerEvent::SpeechStarted { audio_start_ms, .. } => {
                    // VAD detected user starting to speak
                    println!("[VAD: Speech started at {}ms]", audio_start_ms);
                }
                ServerEvent::SpeechStopped { audio_end_ms, .. } => {
                    // VAD detected user stopped speaking
                    println!("[VAD: Speech stopped at {}ms]", audio_end_ms);
                }
                ServerEvent::AudioDelta { delta, .. } => {
                    // Receive audio response (base64-encoded PCM)
                    audio_chunks_received += 1;
                    if audio_chunks_received == 1 {
                        print!("Assistant (audio): ");
                    }
                    // In a real app, decode and play this audio
                    // let audio_bytes = base64::decode(&delta)?;
                    // audio_player.play(audio_bytes);
                    print!("🔊");
                    use std::io::Write;
                    std::io::stdout().flush().ok();

                    // Decode to show audio stats
                    if let Ok(bytes) =
                        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &delta)
                    {
                        let format = AudioFormat::pcm16_24khz();
                        let duration_ms =
                            bytes.len() as f32 / (format.sample_rate as f32 * 2.0) * 1000.0;
                        if audio_chunks_received % 10 == 0 {
                            print!(" ({:.0}ms) ", duration_ms);
                        }
                    }
                }
                ServerEvent::TranscriptDelta { delta, .. } => {
                    // Real-time transcription of input audio
                    transcript.push_str(&delta);
                }
                ServerEvent::TextDelta { delta, .. } => {
                    // Text version of the response
                    print!("{}", delta);
                    use std::io::Write;
                    std::io::stdout().flush().ok();
                }
                ServerEvent::ResponseDone { response, .. } => {
                    if audio_chunks_received > 0 {
                        println!("\n[Received {} audio chunks]", audio_chunks_received);
                    }
                    if let Some(status) = response.get("status").and_then(|v| v.as_str()) {
                        println!("[Response status: {}]\n", status);
                    }
                    break;
                }
                ServerEvent::Error { error, .. } => {
                    eprintln!("\nError: {} - {}", error.error_type, error.message);
                    break;
                }
                _ => {}
            },
            Err(e) => {
                eprintln!("Error: {}", e);
                break;
            }
        }
    }

    if !transcript.is_empty() {
        println!("Transcript: {}", transcript);
    }

    println!("\n=== VAD Configuration Explained ===");
    println!("• threshold: 0.5 - Sensitivity of speech detection");
    println!("• prefix_padding: 300ms - Audio captured before speech detected");
    println!("• silence_duration: 500ms - How long to wait before ending turn");
    println!("• interrupt_response: true - User can interrupt assistant");

    println!("\n=== Session Complete ===");

    Ok(())
}
