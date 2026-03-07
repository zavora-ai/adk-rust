//! OpenAI STT example — transcribe audio using Whisper API.
//!
//! Demonstrates:
//! - Generating audio via OpenAI TTS, then transcribing it back with Whisper
//! - Round-trip: text → speech → text
//! - Word-level timestamps
//!
//! # Setup
//!
//! Set `OPENAI_API_KEY` in your environment or `.env` file.
//!
//! # Run
//!
//! ```bash
//! cargo run --example audio_openai_stt --features audio
//! ```

use adk_audio::{OpenAiTts, SttOptions, SttProvider, TtsProvider, TtsRequest, WhisperApiStt};
use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    println!("=== adk-audio: OpenAI STT (Whisper) Example ===\n");

    let tts = OpenAiTts::from_env()?;
    let stt = WhisperApiStt::from_env()?;

    // Generate audio from text, then transcribe it back
    let original = "The quick brown fox jumps over the lazy dog. \
                    Artificial intelligence is transforming how we build software.";

    println!("Original text:\n  \"{original}\"\n");

    // Step 1: Synthesize
    println!("Step 1: Synthesizing with OpenAI TTS...");
    let request = TtsRequest { text: original.into(), voice: "alloy".into(), ..Default::default() };
    let frame = tts.synthesize(&request).await?;
    println!(
        "  Audio: {}ms, {}Hz, {} bytes\n",
        frame.duration_ms,
        frame.sample_rate,
        frame.data.len()
    );

    // Step 2: Transcribe
    println!("Step 2: Transcribing with Whisper API...");
    let opts =
        SttOptions { language: Some("en".into()), word_timestamps: true, ..Default::default() };
    let transcript = stt.transcribe(&frame, &opts).await?;

    println!("Transcribed text:\n  \"{}\"\n", transcript.text);

    if !transcript.words.is_empty() {
        println!("Word timestamps:");
        for w in &transcript.words {
            println!(
                "  [{:>6}ms - {:>6}ms] \"{}\" (confidence: {:.2})",
                w.start_ms, w.end_ms, w.text, w.confidence
            );
        }
        println!();
    }

    if let Some(lang) = &transcript.language_detected {
        println!("Detected language: {lang}");
    }

    println!("\nDone!");
    Ok(())
}
