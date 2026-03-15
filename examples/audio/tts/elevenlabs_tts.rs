//! ElevenLabs TTS example — synthesize speech with emotion control.
//!
//! Demonstrates:
//! - Creating an ElevenLabs TTS provider from environment
//! - Synthesizing with built-in voices (Rachel, Antoni)
//! - Emotion-based voice settings (happy, calm, whisper)
//! - Streaming synthesis
//! - Round-trip with Whisper STT
//!
//! # Setup
//!
//! Set `ELEVENLABS_API_KEY` and `OPENAI_API_KEY` in your environment or `.env` file.
//!
//! # Run
//!
//! ```bash
//! cargo run --example audio_elevenlabs_tts --features audio
//! ```

use adk_audio::{
    AudioFormat, ElevenLabsTts, Emotion, SttOptions, SttProvider, TtsProvider, TtsRequest,
    WhisperApiStt, encode,
};
use anyhow::Result;
use futures::StreamExt;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    println!("=== adk-audio: ElevenLabs TTS Example ===\n");

    let tts = ElevenLabsTts::from_env()?;

    // Show available voices
    println!("Available voices:");
    for v in tts.voice_catalog() {
        println!("  - {} [{}] ({})", v.name, v.id, v.gender.as_deref().unwrap_or("unspecified"));
    }
    println!();

    // 1. Basic synthesis with different voices
    let voices = [("21m00Tcm4TlvDq8ikWAM", "Rachel"), ("ErXwobaYiN019PkySvjV", "Antoni")];

    for (voice_id, name) in &voices {
        println!("Synthesizing with {name}...");
        let request = TtsRequest {
            text: format!(
                "Hello, I'm {name} from ElevenLabs. This is high-quality neural speech synthesis."
            ),
            voice: (*voice_id).into(),
            ..Default::default()
        };
        let frame = tts.synthesize(&request).await?;
        let wav = encode(&frame, AudioFormat::Wav)?;
        let filename = format!("elevenlabs_{}.wav", name.to_lowercase());
        std::fs::write(&filename, &wav)?;
        println!(
            "  → {filename}: {}ms, {}Hz, {} bytes\n",
            frame.duration_ms,
            frame.sample_rate,
            wav.len()
        );
    }

    // 2. Emotion control
    println!("Emotion-controlled synthesis (Rachel):");
    let emotions = [
        (
            Some(Emotion::Happy),
            "happy",
            "This is wonderful news! I'm so excited to share it with you.",
        ),
        (Some(Emotion::Calm), "calm", "Take a deep breath. Everything is going to be just fine."),
        (Some(Emotion::Whisper), "whisper", "Let me tell you a secret. Come a little closer."),
    ];

    for (emotion, label, text) in &emotions {
        println!("  [{label}] Synthesizing...");
        let request = TtsRequest {
            text: (*text).into(),
            voice: "21m00Tcm4TlvDq8ikWAM".into(),
            emotion: *emotion,
            ..Default::default()
        };
        let frame = tts.synthesize(&request).await?;
        let wav = encode(&frame, AudioFormat::Wav)?;
        let filename = format!("elevenlabs_emotion_{label}.wav");
        std::fs::write(&filename, &wav)?;
        println!("    → {filename}: {}ms, {} bytes", frame.duration_ms, wav.len());
    }
    println!();

    // 3. Streaming synthesis
    println!("Streaming synthesis:");
    let request = TtsRequest {
        text: "Streaming allows audio to arrive in chunks, reducing time to first byte for real-time applications.".into(),
        voice: "21m00Tcm4TlvDq8ikWAM".into(),
        ..Default::default()
    };
    let mut stream = tts.synthesize_stream(&request).await?;
    let mut chunk_count = 0;
    let mut total_bytes = 0;
    while let Some(result) = stream.next().await {
        let frame = result?;
        chunk_count += 1;
        total_bytes += frame.data.len();
        println!("  chunk {chunk_count}: {}ms, {} bytes", frame.duration_ms, frame.data.len());
    }
    println!("  Total: {chunk_count} chunks, {total_bytes} bytes\n");

    // 4. Round-trip with Whisper STT
    if std::env::var("OPENAI_API_KEY").is_ok() {
        println!("Round-trip: ElevenLabs TTS → Whisper STT:");
        let original =
            "The Agent Development Kit makes it easy to build voice-powered applications.";
        println!("  Original: \"{original}\"");

        let request = TtsRequest {
            text: original.into(),
            voice: "21m00Tcm4TlvDq8ikWAM".into(),
            ..Default::default()
        };
        let frame = tts.synthesize(&request).await?;

        let stt = WhisperApiStt::from_env()?;
        let opts = SttOptions { language: Some("en".into()), ..Default::default() };
        let transcript = stt.transcribe(&frame, &opts).await?;
        println!("  Transcribed: \"{}\"\n", transcript.text);
    }

    println!("Done! WAV files written to current directory.");
    Ok(())
}
