//! Basic TTS example — synthesize text to speech using a cloud provider.
//!
//! Demonstrates:
//! - Creating a TTS provider from environment variables
//! - Synthesizing text to an AudioFrame
//! - Encoding the result as a WAV file
//!
//! # Setup
//!
//! Set one of these environment variables:
//! - `OPENAI_API_KEY` for OpenAI TTS
//! - `ELEVENLABS_API_KEY` for ElevenLabs
//!
//! # Run
//!
//! ```bash
//! cargo run --example audio_tts_basic --features audio
//! ```

use std::sync::Arc;

use adk_audio::{AudioFormat, AudioFrame, SentenceChunker, TtsProvider, TtsRequest, encode};
use anyhow::Result;

/// A mock TTS provider for demonstration when no API key is available.
///
/// Generates silent audio frames with the correct duration metadata.
struct MockTtsProvider;

#[async_trait::async_trait]
impl TtsProvider for MockTtsProvider {
    async fn synthesize(
        &self,
        request: &adk_audio::TtsRequest,
    ) -> adk_audio::AudioResult<AudioFrame> {
        // Approximate 150 words per minute, 5 chars per word
        let word_count = request.text.split_whitespace().count();
        let duration_ms = (word_count as u32 * 400).max(100);
        println!("  [mock-tts] synthesizing {} chars → ~{}ms", request.text.len(), duration_ms);
        Ok(AudioFrame::silence(24000, 1, duration_ms))
    }

    async fn synthesize_stream(
        &self,
        request: &adk_audio::TtsRequest,
    ) -> adk_audio::AudioResult<
        std::pin::Pin<Box<dyn futures::Stream<Item = adk_audio::AudioResult<AudioFrame>> + Send>>,
    > {
        let frame = self.synthesize(request).await?;
        Ok(Box::pin(futures::stream::once(async { Ok(frame) })))
    }

    fn voice_catalog(&self) -> &[adk_audio::Voice] {
        &[]
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    println!("=== adk-audio: Basic TTS Example ===\n");

    // Use mock provider (swap with real provider when API keys are set)
    let tts: Arc<dyn TtsProvider> = Arc::new(MockTtsProvider);

    // 1. Simple batch synthesis
    println!("1. Batch synthesis:");
    let request = TtsRequest {
        text: "Hello! Welcome to ADK Audio. This is a text-to-speech demonstration.".into(),
        voice: "default".into(),
        ..Default::default()
    };
    let frame = tts.synthesize(&request).await?;
    println!(
        "   Output: {}ms, {}Hz, {} channel(s), {} bytes\n",
        frame.duration_ms,
        frame.sample_rate,
        frame.channels,
        frame.data.len()
    );

    // 2. Encode to WAV
    println!("2. WAV encoding:");
    let wav_bytes = encode(&frame, AudioFormat::Wav)?;
    println!("   WAV size: {} bytes (PCM: {} bytes)", wav_bytes.len(), frame.data.len());
    println!("   WAV header adds {} bytes\n", wav_bytes.len() - frame.data.len());

    // 3. Sentence-chunked streaming (simulating LLM token output)
    println!("3. Sentence-chunked streaming:");
    let tokens = [
        "The ",
        "weather ",
        "today ",
        "is ",
        "sunny. ",
        "Perfect ",
        "for ",
        "a ",
        "walk! ",
        "Don't ",
        "forget ",
        "sunscreen",
    ];

    let mut chunker = SentenceChunker::new();
    for token in &tokens {
        let sentences = chunker.push(token);
        for sentence in sentences {
            println!("   → Sentence ready: \"{sentence}\"");
            let req = TtsRequest { text: sentence, voice: "default".into(), ..Default::default() };
            let audio = tts.synthesize(&req).await?;
            println!("     Audio: {}ms", audio.duration_ms);
        }
    }
    if let Some(remaining) = chunker.flush() {
        println!("   → Flush: \"{remaining}\"");
        let req = TtsRequest { text: remaining, voice: "default".into(), ..Default::default() };
        let audio = tts.synthesize(&req).await?;
        println!("     Audio: {}ms", audio.duration_ms);
    }

    println!("\nDone!");
    Ok(())
}
