//! Multi-model STT comparison — transcribe the same audio with every enabled provider.
//!
//! Demonstrates:
//! - Side-by-side comparison of Whisper, Distil-Whisper, and Moonshine
//! - Transcription timing and confidence per provider
//! - Optional WER (Word Error Rate) calculation against a reference transcript
//! - Graceful skipping of providers whose feature flags are not enabled
//!
//! Models auto-download from HuggingFace Hub on first run.
//! No API keys required — runs entirely on-device via ONNX Runtime.
//!
//! # Run
//!
//! ```bash
//! # Transcribe a WAV file with all STT providers:
//! cargo run --example audio_stt_comparison --features whisper-onnx,distil-whisper,moonshine -- audio.wav
//!
//! # With a reference transcript for WER calculation:
//! cargo run --example audio_stt_comparison --features whisper-onnx,distil-whisper,moonshine -- audio.wav --ref "expected text"
//!
//! # With a single provider:
//! cargo run --example audio_stt_comparison --features whisper-onnx -- audio.wav
//! ```

#![allow(unused_imports, dead_code)]

use std::time::Instant;

use adk_audio::AudioFrame;
use anyhow::Result;
use clap::Parser;

/// Multi-model STT comparison tool.
#[derive(Parser)]
#[command(name = "audio_stt_comparison")]
struct Args {
    /// Path to WAV file to transcribe.
    wav_path: String,

    /// Reference transcript for WER calculation.
    #[arg(long = "ref")]
    reference: Option<String>,
}

/// Result from a single provider transcription attempt.
struct ProviderResult {
    name: &'static str,
    transcript: String,
    confidence: f32,
    language: Option<String>,
    elapsed_s: f64,
    wer: Option<f64>,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt::init();

    let args = Args::parse();

    println!("=== adk-audio: Multi-Model STT Comparison ===\n");
    println!("Input: {}\n", args.wav_path);

    let frame = load_wav(&args.wav_path)?;
    println!(
        "Audio: {}ms, {}Hz, {} channel(s), {} bytes\n",
        frame.duration_ms,
        frame.sample_rate,
        frame.channels,
        frame.data.len()
    );

    if let Some(ref text) = args.reference {
        println!("Reference: \"{text}\"\n");
    }

    let mut results: Vec<ProviderResult> = Vec::new();

    // --- Whisper ---
    if let Some(r) = try_whisper(&frame, args.reference.as_deref()).await {
        results.push(r);
    }

    // --- Distil-Whisper ---
    if let Some(r) = try_distil_whisper(&frame, args.reference.as_deref()).await {
        results.push(r);
    }

    // --- Moonshine ---
    if let Some(r) = try_moonshine(&frame, args.reference.as_deref()).await {
        results.push(r);
    }

    // --- Summary ---
    println!("\n=== Comparison Summary ===\n");

    if results.is_empty() {
        println!("No STT providers were enabled. Enable features to compare providers:");
        println!("  --features whisper-onnx     Whisper (tiny–large-v3-turbo)");
        println!("  --features distil-whisper   Distil-Whisper (fast, near-Whisper accuracy)");
        println!("  --features moonshine        Moonshine (ultra-lightweight, edge-optimized)");
        return Ok(());
    }

    if args.reference.is_some() {
        println!("{:<20} {:>10} {:>10} {:>8} Transcript", "Provider", "Time", "Conf.", "WER");
        println!("{}", "-".repeat(80));
        for r in &results {
            let wer_str = r.wer.map_or("N/A".into(), |w| format!("{w:.1}%"));
            println!(
                "{:<20} {:>8.2}s {:>10.2} {:>8} \"{}\"",
                r.name, r.elapsed_s, r.confidence, wer_str, r.transcript
            );
        }
    } else {
        println!("{:<20} {:>10} {:>10} {:>10} Transcript", "Provider", "Time", "Conf.", "Language");
        println!("{}", "-".repeat(80));
        for r in &results {
            let lang = r.language.as_deref().unwrap_or("N/A");
            println!(
                "{:<20} {:>8.2}s {:>10.2} {:>10} \"{}\"",
                r.name, r.elapsed_s, r.confidence, lang, r.transcript
            );
        }
    }

    println!("\nDone!");
    Ok(())
}

// ---------------------------------------------------------------------------
// Per-provider transcription functions (feature-gated)
// ---------------------------------------------------------------------------

#[cfg(feature = "whisper-onnx")]
async fn try_whisper(frame: &AudioFrame, reference: Option<&str>) -> Option<ProviderResult> {
    use adk_audio::{
        LocalModelRegistry, OnnxSttConfig, OnnxSttProvider, SttBackend, SttOptions, SttProvider,
        WhisperModelSize,
    };

    println!("--- Whisper (base) ---");
    println!("Loading Whisper base model (auto-downloads on first run)...");

    let config = OnnxSttConfig::builder()
        .stt_backend(SttBackend::Whisper)
        .model_size(WhisperModelSize::Base)
        .build()
        .ok()?;
    let registry = LocalModelRegistry::default();
    let provider = match OnnxSttProvider::new(config, &registry).await {
        Ok(p) => p,
        Err(e) => {
            println!("  ⚠ Skipped: {e}\n");
            return None;
        }
    };

    let opts = SttOptions::default();
    let start = Instant::now();
    let transcript = match provider.transcribe(frame, &opts).await {
        Ok(t) => t,
        Err(e) => {
            println!("  ⚠ Transcription failed: {e}\n");
            return None;
        }
    };
    let elapsed = start.elapsed().as_secs_f64();

    let wer = reference.map(|r| compute_wer(r, &transcript.text));

    println!("  Transcript:  \"{}\"", transcript.text);
    println!("  Confidence:  {:.2}", transcript.confidence);
    if let Some(lang) = &transcript.language_detected {
        println!("  Language:    {lang}");
    }
    println!("  Time:        {elapsed:.2}s");
    if let Some(w) = wer {
        println!("  WER:         {w:.1}%");
    }
    println!();

    Some(ProviderResult {
        name: "Whisper (base)",
        transcript: transcript.text,
        confidence: transcript.confidence,
        language: transcript.language_detected,
        elapsed_s: elapsed,
        wer,
    })
}

#[cfg(not(feature = "whisper-onnx"))]
async fn try_whisper(_frame: &AudioFrame, _reference: Option<&str>) -> Option<ProviderResult> {
    println!("--- Whisper ---");
    println!("  ⏭ Skipped (enable with --features whisper-onnx)\n");
    None
}

#[cfg(feature = "distil-whisper")]
async fn try_distil_whisper(frame: &AudioFrame, reference: Option<&str>) -> Option<ProviderResult> {
    use adk_audio::{
        DistilWhisperVariant, LocalModelRegistry, OnnxSttConfig, OnnxSttProvider, SttBackend,
        SttOptions, SttProvider,
    };

    println!("--- Distil-Whisper (distil-small.en) ---");
    println!("Loading Distil-Whisper model (auto-downloads on first run)...");

    let config = OnnxSttConfig::builder()
        .stt_backend(SttBackend::DistilWhisper)
        .distil_variant(DistilWhisperVariant::DistilSmallEn)
        .build()
        .ok()?;
    let registry = LocalModelRegistry::default();
    let provider = match OnnxSttProvider::new(config, &registry).await {
        Ok(p) => p,
        Err(e) => {
            println!("  ⚠ Skipped: {e}\n");
            return None;
        }
    };

    let opts = SttOptions::default();
    let start = Instant::now();
    let transcript = match provider.transcribe(frame, &opts).await {
        Ok(t) => t,
        Err(e) => {
            println!("  ⚠ Transcription failed: {e}\n");
            return None;
        }
    };
    let elapsed = start.elapsed().as_secs_f64();

    let wer = reference.map(|r| compute_wer(r, &transcript.text));

    println!("  Transcript:  \"{}\"", transcript.text);
    println!("  Confidence:  {:.2}", transcript.confidence);
    if let Some(lang) = &transcript.language_detected {
        println!("  Language:    {lang}");
    }
    println!("  Time:        {elapsed:.2}s");
    if let Some(w) = wer {
        println!("  WER:         {w:.1}%");
    }
    println!();

    Some(ProviderResult {
        name: "Distil-Whisper",
        transcript: transcript.text,
        confidence: transcript.confidence,
        language: transcript.language_detected,
        elapsed_s: elapsed,
        wer,
    })
}

#[cfg(not(feature = "distil-whisper"))]
async fn try_distil_whisper(
    _frame: &AudioFrame,
    _reference: Option<&str>,
) -> Option<ProviderResult> {
    println!("--- Distil-Whisper ---");
    println!("  ⏭ Skipped (enable with --features distil-whisper)\n");
    None
}

#[cfg(feature = "moonshine")]
async fn try_moonshine(frame: &AudioFrame, reference: Option<&str>) -> Option<ProviderResult> {
    use adk_audio::{
        LocalModelRegistry, MoonshineVariant, OnnxSttConfig, OnnxSttProvider, SttBackend,
        SttOptions, SttProvider,
    };

    println!("--- Moonshine (tiny) ---");
    println!("Loading Moonshine tiny model (auto-downloads on first run)...");

    let config = OnnxSttConfig::builder()
        .stt_backend(SttBackend::Moonshine)
        .moonshine_variant(MoonshineVariant::Tiny)
        .build()
        .ok()?;
    let registry = LocalModelRegistry::default();
    let provider = match OnnxSttProvider::new(config, &registry).await {
        Ok(p) => p,
        Err(e) => {
            println!("  ⚠ Skipped: {e}\n");
            return None;
        }
    };

    let opts = SttOptions::default();
    let start = Instant::now();
    let transcript = match provider.transcribe(frame, &opts).await {
        Ok(t) => t,
        Err(e) => {
            println!("  ⚠ Transcription failed: {e}\n");
            return None;
        }
    };
    let elapsed = start.elapsed().as_secs_f64();

    let wer = reference.map(|r| compute_wer(r, &transcript.text));

    println!("  Transcript:  \"{}\"", transcript.text);
    println!("  Confidence:  {:.2}", transcript.confidence);
    if let Some(lang) = &transcript.language_detected {
        println!("  Language:    {lang}");
    }
    println!("  Time:        {elapsed:.2}s");
    if let Some(w) = wer {
        println!("  WER:         {w:.1}%");
    }
    println!();

    Some(ProviderResult {
        name: "Moonshine (tiny)",
        transcript: transcript.text,
        confidence: transcript.confidence,
        language: transcript.language_detected,
        elapsed_s: elapsed,
        wer,
    })
}

#[cfg(not(feature = "moonshine"))]
async fn try_moonshine(_frame: &AudioFrame, _reference: Option<&str>) -> Option<ProviderResult> {
    println!("--- Moonshine ---");
    println!("  ⏭ Skipped (enable with --features moonshine)\n");
    None
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Compute Word Error Rate (WER) between reference and hypothesis.
///
/// Returns WER as a percentage (0.0 = perfect, 100.0 = all wrong).
fn compute_wer(reference: &str, hypothesis: &str) -> f64 {
    let ref_words = normalize_for_wer(reference);
    let hyp_words = normalize_for_wer(hypothesis);

    if ref_words.is_empty() {
        return if hyp_words.is_empty() { 0.0 } else { 100.0 };
    }

    // Levenshtein distance on word sequences
    let n = ref_words.len();
    let m = hyp_words.len();
    let mut dp = vec![vec![0usize; m + 1]; n + 1];

    for i in 0..=n {
        dp[i][0] = i;
    }
    for j in 0..=m {
        dp[0][j] = j;
    }

    for i in 1..=n {
        for j in 1..=m {
            let cost = if ref_words[i - 1] == hyp_words[j - 1] { 0 } else { 1 };
            dp[i][j] = (dp[i - 1][j] + 1).min(dp[i][j - 1] + 1).min(dp[i - 1][j - 1] + cost);
        }
    }

    (dp[n][m] as f64 / n as f64) * 100.0
}

/// Normalize text for WER: lowercase, strip punctuation, split into words.
fn normalize_for_wer(text: &str) -> Vec<String> {
    text.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .map(String::from)
        .collect()
}

/// Load a WAV file into an AudioFrame.
fn load_wav(path: &str) -> Result<AudioFrame> {
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    let channels = spec.channels as u8;
    let sample_rate = spec.sample_rate;

    let samples: Vec<i16> = match spec.sample_format {
        hound::SampleFormat::Int => {
            if spec.bits_per_sample == 16 {
                reader.samples::<i16>().collect::<std::result::Result<Vec<_>, _>>()?
            } else {
                let shift = spec.bits_per_sample as i32 - 16;
                reader
                    .samples::<i32>()
                    .map(|s| {
                        s.map(
                            |v| if shift > 0 { (v >> shift) as i16 } else { (v << -shift) as i16 },
                        )
                    })
                    .collect::<std::result::Result<Vec<_>, _>>()?
            }
        }
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .map(|s| s.map(|v| (v * 32767.0).clamp(-32768.0, 32767.0) as i16))
            .collect::<std::result::Result<Vec<_>, _>>()?,
    };

    let mut bytes = Vec::with_capacity(samples.len() * 2);
    for s in &samples {
        bytes.extend_from_slice(&s.to_le_bytes());
    }

    Ok(AudioFrame::new(bytes, sample_rate, channels))
}
