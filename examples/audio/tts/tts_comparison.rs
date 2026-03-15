//! Multi-model TTS comparison — synthesize the same text with every enabled provider.
//!
//! Demonstrates:
//! - Side-by-side comparison of Kokoro, Chatterbox, and Qwen3-TTS
//! - Synthesis duration and output file size per provider
//! - Graceful skipping of providers whose feature flags are not enabled
//!
//! Models auto-download from HuggingFace Hub on first run.
//! No API keys required — runs entirely on-device via ONNX Runtime.
//!
//! # Run
//!
//! ```bash
//! # With just the base audio feature (skips all ONNX providers):
//! cargo run --example audio_tts_comparison --features audio
//!
//! # With specific providers:
//! cargo run --example audio_tts_comparison --features kokoro,chatterbox
//!
//! # With all ONNX providers:
//! cargo run --example audio_tts_comparison --features kokoro,chatterbox,qwen3-tts
//! ```

// Feature-gated providers mean some imports/helpers are unused depending on
// which features are enabled. This is by design.
#![allow(unused_imports, dead_code)]

use std::time::Instant;

use adk_audio::AudioFrame;
use anyhow::Result;

const OUTPUT_DIR: &str = "samples";
const SAMPLE_TEXT: &str =
    "The quick brown fox jumps over the lazy dog. This is a multi-model comparison test.";

/// Result from a single provider synthesis attempt.
struct ProviderResult {
    name: &'static str,
    file_size: u64,
    audio_duration_s: f64,
    synth_time_s: f64,
    filename: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt::init();
    std::fs::create_dir_all(OUTPUT_DIR)?;

    println!("=== adk-audio: Multi-Model TTS Comparison ===\n");
    println!("Text: \"{SAMPLE_TEXT}\"\n");

    let mut results: Vec<ProviderResult> = Vec::new();

    // --- Kokoro ---
    if let Some(r) = try_kokoro(SAMPLE_TEXT).await {
        results.push(r);
    }

    // --- Chatterbox ---
    if let Some(r) = try_chatterbox(SAMPLE_TEXT).await {
        results.push(r);
    }

    // --- Qwen3-TTS ---
    if let Some(r) = try_qwen3_tts(SAMPLE_TEXT).await {
        results.push(r);
    }

    // --- Summary ---
    println!("\n=== Comparison Summary ===\n");

    if results.is_empty() {
        println!("No TTS providers were enabled. Enable features to compare providers:");
        println!("  --features kokoro        Kokoro 82M");
        println!("  --features chatterbox    Chatterbox voice cloning");
        println!("  --features qwen3-tts     Qwen3-TTS 0.6B");
        return Ok(());
    }

    println!(
        "{:<16} {:>12} {:>10} {:>12} Output File",
        "Provider", "File Size", "Duration", "Synth Time"
    );
    println!("{}", "-".repeat(72));
    for r in &results {
        println!(
            "{:<16} {:>10} B {:>8.1}s {:>10.2}s   {}",
            r.name, r.file_size, r.audio_duration_s, r.synth_time_s, r.filename
        );
    }

    println!("\nDone! WAV files written to {OUTPUT_DIR}/");
    Ok(())
}

// ---------------------------------------------------------------------------
// Per-provider synthesis functions (feature-gated)
// ---------------------------------------------------------------------------

#[cfg(feature = "kokoro")]
async fn try_kokoro(text: &str) -> Option<ProviderResult> {
    use adk_audio::{OnnxTtsProvider, TtsProvider, TtsRequest};

    println!("--- Kokoro ---");
    println!("Loading Kokoro model (auto-downloads on first run)...");

    let provider = match OnnxTtsProvider::default_kokoro().await {
        Ok(p) => p,
        Err(e) => {
            println!("  ⚠ Skipped: {e}\n");
            return None;
        }
    };

    let request =
        TtsRequest { text: text.into(), voice: "af_sky".into(), speed: 1.0, ..Default::default() };

    let start = Instant::now();
    let frame = match provider.synthesize(&request).await {
        Ok(f) => f,
        Err(e) => {
            println!("  ⚠ Synthesis failed: {e}\n");
            return None;
        }
    };
    let synth_time = start.elapsed().as_secs_f64();

    let filename = format!("{OUTPUT_DIR}/comparison_kokoro.wav");
    write_wav(&filename, &frame).ok()?;
    let file_size = std::fs::metadata(&filename).ok()?.len();
    let audio_duration_s = frame.duration_ms as f64 / 1000.0;

    println!(
        "  → {filename}: {file_size} B, {audio_duration_s:.1}s audio, {synth_time:.2}s synth\n"
    );

    Some(ProviderResult {
        name: "Kokoro",
        file_size,
        audio_duration_s,
        synth_time_s: synth_time,
        filename,
    })
}

#[cfg(not(feature = "kokoro"))]
async fn try_kokoro(_text: &str) -> Option<ProviderResult> {
    println!("--- Kokoro ---");
    println!("  ⏭ Skipped (enable with --features kokoro)\n");
    None
}

#[cfg(feature = "chatterbox")]
async fn try_chatterbox(text: &str) -> Option<ProviderResult> {
    use adk_audio::{ChatterboxConfig, ChatterboxTtsProvider, TtsProvider, TtsRequest};

    println!("--- Chatterbox ---");
    println!("Loading Chatterbox model (auto-downloads on first run)...");

    let config = ChatterboxConfig::default();
    let provider = match ChatterboxTtsProvider::load(config).await {
        Ok(p) => p,
        Err(e) => {
            println!("  ⚠ Skipped: {e}\n");
            return None;
        }
    };

    let request =
        TtsRequest { text: text.into(), voice: String::new(), speed: 1.0, ..Default::default() };

    let start = Instant::now();
    let frame = match provider.synthesize(&request).await {
        Ok(f) => f,
        Err(e) => {
            println!("  ⚠ Synthesis failed: {e}\n");
            return None;
        }
    };
    let synth_time = start.elapsed().as_secs_f64();

    let filename = format!("{OUTPUT_DIR}/comparison_chatterbox.wav");
    write_wav(&filename, &frame).ok()?;
    let file_size = std::fs::metadata(&filename).ok()?.len();
    let audio_duration_s = frame.duration_ms as f64 / 1000.0;

    println!(
        "  → {filename}: {file_size} B, {audio_duration_s:.1}s audio, {synth_time:.2}s synth\n"
    );

    Some(ProviderResult {
        name: "Chatterbox",
        file_size,
        audio_duration_s,
        synth_time_s: synth_time,
        filename,
    })
}

#[cfg(not(feature = "chatterbox"))]
async fn try_chatterbox(_text: &str) -> Option<ProviderResult> {
    println!("--- Chatterbox ---");
    println!("  ⏭ Skipped (enable with --features chatterbox)\n");
    None
}

#[cfg(feature = "qwen3-tts")]
async fn try_qwen3_tts(text: &str) -> Option<ProviderResult> {
    use adk_audio::{
        LocalModelRegistry, OnnxModelConfig, OnnxTtsProvider, Qwen3TtsPreprocessor,
        Qwen3TtsVariant, TtsProvider, TtsRequest,
    };

    println!("--- Qwen3-TTS (0.6B) ---");
    println!("Loading Qwen3-TTS model (auto-downloads on first run)...");

    let registry = LocalModelRegistry::default();
    let variant = Qwen3TtsVariant::Variant0_6B;
    let model_id = variant.model_id();
    let model_dir = match registry.get_or_download(model_id).await {
        Ok(d) => d,
        Err(e) => {
            println!("  ⚠ Skipped: {e}\n");
            return None;
        }
    };

    let preprocessor = match Qwen3TtsPreprocessor::new(&model_dir, variant) {
        Ok(p) => p,
        Err(e) => {
            println!("  ⚠ Skipped: {e}\n");
            return None;
        }
    };

    let config =
        OnnxModelConfig { model_id: model_id.into(), sample_rate: 24000, ..Default::default() };
    let provider = match OnnxTtsProvider::with_preprocessor(config, &model_dir, preprocessor) {
        Ok(p) => p,
        Err(e) => {
            println!("  ⚠ Skipped: {e}\n");
            return None;
        }
    };

    let request =
        TtsRequest { text: text.into(), voice: "lang:en".into(), speed: 1.0, ..Default::default() };

    let start = Instant::now();
    let frame = match provider.synthesize(&request).await {
        Ok(f) => f,
        Err(e) => {
            println!("  ⚠ Synthesis failed: {e}\n");
            return None;
        }
    };
    let synth_time = start.elapsed().as_secs_f64();

    let filename = format!("{OUTPUT_DIR}/comparison_qwen3_tts.wav");
    write_wav(&filename, &frame).ok()?;
    let file_size = std::fs::metadata(&filename).ok()?.len();
    let audio_duration_s = frame.duration_ms as f64 / 1000.0;

    println!(
        "  → {filename}: {file_size} B, {audio_duration_s:.1}s audio, {synth_time:.2}s synth\n"
    );

    Some(ProviderResult {
        name: "Qwen3-TTS 0.6B",
        file_size,
        audio_duration_s,
        synth_time_s: synth_time,
        filename,
    })
}

#[cfg(not(feature = "qwen3-tts"))]
async fn try_qwen3_tts(_text: &str) -> Option<ProviderResult> {
    println!("--- Qwen3-TTS ---");
    println!("  ⏭ Skipped (enable with --features qwen3-tts)\n");
    None
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Write an AudioFrame to a WAV file (PCM16).
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
