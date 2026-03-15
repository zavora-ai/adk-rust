//! OpenAI TTS example — synthesize speech using the OpenAI API.
//!
//! Demonstrates:
//! - Creating an OpenAI TTS provider from environment
//! - Synthesizing with multiple voices (alloy, nova, onyx)
//! - Using the HD model for higher quality
//! - Encoding output as WAV files
//!
//! # Setup
//!
//! Set `OPENAI_API_KEY` in your environment or `.env` file.
//!
//! # Run
//!
//! ```bash
//! cargo run --example audio_openai_tts --features audio
//! ```

use adk_audio::{AudioFormat, OpenAiTts, TtsProvider, TtsRequest, encode};
use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    println!("=== adk-audio: OpenAI TTS Example ===\n");

    let tts = OpenAiTts::from_env()?;

    // Show available voices
    println!("Available voices:");
    for v in tts.voice_catalog() {
        println!("  - {} ({})", v.name, v.gender.as_deref().unwrap_or("unspecified"));
    }
    println!();

    // Synthesize with different voices
    let samples = [
        ("alloy", "Hello from Alloy! This is the default OpenAI voice."),
        ("nova", "Hi there, I'm Nova. I have a warm female voice."),
        ("onyx", "Greetings. Onyx here, with a deep male tone."),
    ];

    for (voice, text) in &samples {
        println!("Synthesizing with voice '{voice}'...");
        let request =
            TtsRequest { text: (*text).into(), voice: (*voice).into(), ..Default::default() };
        let frame = tts.synthesize(&request).await?;
        let wav = encode(&frame, AudioFormat::Wav)?;
        let filename = format!("openai_tts_{voice}.wav");
        std::fs::write(&filename, &wav)?;
        println!(
            "  → {filename}: {}ms, {}Hz, {} bytes\n",
            frame.duration_ms,
            frame.sample_rate,
            wav.len()
        );
    }

    // HD model example
    println!("Synthesizing with HD model (tts-1-hd)...");
    let hd_tts = OpenAiTts::from_env()?.hd();
    let request = TtsRequest {
        text: "This is the high-definition model. Notice the improved audio quality.".into(),
        voice: "shimmer".into(),
        ..Default::default()
    };
    let frame = hd_tts.synthesize(&request).await?;
    let wav = encode(&frame, AudioFormat::Wav)?;
    std::fs::write("openai_tts_hd.wav", &wav)?;
    println!("  → openai_tts_hd.wav: {}ms, {} bytes\n", frame.duration_ms, wav.len());

    println!("Done! WAV files written to current directory.");
    Ok(())
}
