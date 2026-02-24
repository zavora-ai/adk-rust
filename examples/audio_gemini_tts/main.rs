//! Gemini TTS example — synthesize speech using the Gemini API.
//!
//! Demonstrates:
//! - Creating a Gemini TTS provider from environment
//! - Synthesizing with multiple voices (Puck, Kore, Aoede)
//! - Encoding output as WAV files
//!
//! # Setup
//!
//! Set `GEMINI_API_KEY` in your environment or `.env` file.
//!
//! # Run
//!
//! ```bash
//! cargo run --example audio_gemini_tts --features audio
//! ```

use adk_audio::{AudioFormat, GeminiTts, TtsProvider, TtsRequest, encode};
use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    println!("=== adk-audio: Gemini TTS Example ===\n");

    let tts = GeminiTts::from_env()?;

    // Show available voices
    println!("Available voices:");
    for v in tts.voice_catalog() {
        println!("  - {} ({})", v.name, v.gender.as_deref().unwrap_or("unspecified"));
    }
    println!();

    // Synthesize with different voices
    let samples = [
        ("Puck", "Hello from Puck! I'm the default Gemini voice."),
        ("Kore", "Hi, I'm Kore. A female voice from Gemini."),
        ("Aoede", "Greetings! Aoede here, another female voice option."),
    ];

    for (voice, text) in &samples {
        println!("Synthesizing with voice '{voice}'...");
        let request =
            TtsRequest { text: (*text).into(), voice: (*voice).into(), ..Default::default() };
        let frame = tts.synthesize(&request).await?;
        let wav = encode(&frame, AudioFormat::Wav)?;
        let filename = format!("gemini_tts_{}.wav", voice.to_lowercase());
        std::fs::write(&filename, &wav)?;
        println!(
            "  → {filename}: {}ms, {}Hz, {} bytes\n",
            frame.duration_ms,
            frame.sample_rate,
            wav.len()
        );
    }

    println!("Done! WAV files written to current directory.");
    Ok(())
}
