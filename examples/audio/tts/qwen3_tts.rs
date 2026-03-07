//! Qwen3-TTS example — multilingual speech synthesis with native Candle backend.
//!
//! Demonstrates:
//! - `Qwen3TtsNativeProvider` with predefined speakers (auto-downloads on first run)
//! - Multilingual synthesis: English, Chinese, Japanese
//! - Comparison between 0.6B (faster) and 1.7B (higher quality) variants
//!
//! Models auto-download from HuggingFace Hub on first run.
//! No API keys required — runs entirely on-device via Candle.
//!
//! # Run
//!
//! ```bash
//! cargo run --example audio_qwen3_tts --features qwen3-tts
//! ```

use std::time::Instant;

use adk_audio::{AudioFrame, Qwen3TtsNativeProvider, Qwen3TtsVariant, TtsProvider, TtsRequest};
use anyhow::Result;

const OUTPUT_DIR: &str = "samples";

/// Phrases to synthesize in different languages.
const PHRASES: &[(&str, &str, &str)] = &[
    ("en", "Hello! This is Qwen3 text-to-speech running locally with Candle.", "english"),
    ("zh", "你好！这是通过Candle在本地运行的语音合成。", "chinese"),
    ("ja", "こんにちは！これはCandleで動作する音声合成です。", "japanese"),
];

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt::init();
    std::fs::create_dir_all(OUTPUT_DIR)?;

    println!("=== adk-audio: Qwen3-TTS — Multilingual Synthesis (Native) ===\n");

    // --- Part 1: 0.6B variant (faster) ---
    println!("--- Qwen3-TTS 0.6B (faster, lighter) ---\n");
    let results_06b = synthesize_with_variant(Qwen3TtsVariant::Small).await?;

    // --- Part 2: 1.7B variant (higher quality) ---
    println!("--- Qwen3-TTS 1.7B (higher quality) ---\n");
    let results_17b = synthesize_with_variant(Qwen3TtsVariant::Large).await?;

    // --- Comparison ---
    println!("=== Variant Comparison ===\n");
    println!(
        "{:<10} {:>10} {:>10} {:>12} {:>12}",
        "Language", "0.6B Size", "1.7B Size", "0.6B Time", "1.7B Time"
    );
    println!("{}", "-".repeat(58));
    for (i, (lang, _, label)) in PHRASES.iter().enumerate() {
        let (size_06, time_06) = results_06b.get(i).copied().unwrap_or((0, 0.0));
        let (size_17, time_17) = results_17b.get(i).copied().unwrap_or((0, 0.0));
        println!("{label:<10} {size_06:>8} B {size_17:>8} B {time_06:>10.2}s {time_17:>10.2}s",);
        let _ = lang;
    }

    println!("\nDone! WAV files written to {OUTPUT_DIR}/");
    Ok(())
}

/// Synthesize all phrases with a specific Qwen3-TTS variant.
async fn synthesize_with_variant(variant: Qwen3TtsVariant) -> Result<Vec<(u64, f64)>> {
    let variant_label = variant.to_string();

    println!("Loading Qwen3-TTS {variant_label} model (auto-downloads on first run)...");
    let provider = match Qwen3TtsNativeProvider::new(variant).await {
        Ok(p) => p,
        Err(e) => {
            println!("Failed to load Qwen3-TTS {variant_label}: {e}");
            println!("This may require HF_TOKEN or the model download may have failed.\n");
            return Ok(Vec::new());
        }
    };
    println!("Model loaded (sample rate: {} Hz).\n", provider.sample_rate());

    let mut results = Vec::new();

    for (lang, text, label) in PHRASES {
        println!("Synthesizing ({label}): \"{text}\"");

        let request = TtsRequest {
            text: (*text).into(),
            voice: format!("vivian:{lang}"),
            speed: 1.0,
            ..Default::default()
        };

        let start = Instant::now();
        match provider.synthesize(&request).await {
            Ok(frame) => {
                let elapsed = start.elapsed().as_secs_f64();
                let filename = format!("{OUTPUT_DIR}/qwen3_tts_{variant_label}_{lang}.wav");
                write_wav(&filename, &frame)?;
                let file_size = std::fs::metadata(&filename)?.len();
                println!(
                    "  → {filename}: {file_size} bytes, {:.1}s audio, synthesized in {elapsed:.2}s\n",
                    frame.duration_ms as f64 / 1000.0
                );
                results.push((file_size, elapsed));
            }
            Err(e) => {
                println!("  Synthesis failed for {label}: {e}\n");
            }
        }
    }

    Ok(results)
}

/// Write an AudioFrame to a WAV file.
fn write_wav(path: &str, frame: &AudioFrame) -> Result<()> {
    let spec = hound::WavSpec {
        channels: frame.channels as u16,
        sample_rate: frame.sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec)?;
    for chunk in frame.data.chunks_exact(2) {
        let sample = i16::from_le_bytes([chunk[0], chunk[1]]);
        writer.write_sample(sample)?;
    }
    writer.finalize()?;
    Ok(())
}
