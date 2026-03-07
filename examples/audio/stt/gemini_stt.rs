//! Gemini STT example — generate audio with Gemini TTS, transcribe with Whisper.
//!
//! Demonstrates:
//! - Cross-provider round-trip: Gemini TTS → Whisper STT
//! - Comparing original text with transcription
//!
//! # Setup
//!
//! Set both `GEMINI_API_KEY` and `OPENAI_API_KEY` in your environment or `.env` file.
//! (Whisper API requires an OpenAI key for transcription.)
//!
//! # Run
//!
//! ```bash
//! cargo run --example audio_gemini_stt --features audio
//! ```

use adk_audio::{GeminiTts, SttOptions, SttProvider, TtsProvider, TtsRequest, WhisperApiStt};
use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    println!("=== adk-audio: Gemini TTS → Whisper STT Round-Trip ===\n");

    let tts = GeminiTts::from_env()?;
    let stt = WhisperApiStt::from_env()?;

    let phrases = [
        ("Puck", "Welcome to the Agent Development Kit. Build intelligent agents with Rust."),
        ("Kore", "Speech synthesis and recognition work together to create voice interfaces."),
    ];

    for (voice, original) in &phrases {
        println!("--- Voice: {voice} ---");
        println!("Original: \"{original}\"\n");

        // Synthesize with Gemini
        println!("  Synthesizing with Gemini TTS...");
        let request =
            TtsRequest { text: (*original).into(), voice: (*voice).into(), ..Default::default() };
        let frame = tts.synthesize(&request).await?;
        println!(
            "  Audio: {}ms, {}Hz, {} bytes",
            frame.duration_ms,
            frame.sample_rate,
            frame.data.len()
        );

        // Transcribe with Whisper
        println!("  Transcribing with Whisper API...");
        let opts = SttOptions { language: Some("en".into()), ..Default::default() };
        let transcript = stt.transcribe(&frame, &opts).await?;
        println!("  Result: \"{}\"\n", transcript.text);
    }

    println!("Done!");
    Ok(())
}
