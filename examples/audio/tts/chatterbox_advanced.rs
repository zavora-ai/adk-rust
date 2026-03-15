//! Chatterbox Advanced TTS example — variant comparison and voice cloning.
//!
//! Demonstrates:
//! - All four Chatterbox quantization variants (fp32, fp16, q4, q4f16)
//! - Synthesis duration and output file size comparison across variants
//! - Voice cloning from a reference WAV file
//!
//! Models auto-download from HuggingFace Hub on first run.
//! No API keys required — runs entirely on-device.
//!
//! # Run
//!
//! ```bash
//! cargo run --example audio_chatterbox_advanced --features chatterbox
//! ```

use std::path::{Path, PathBuf};
use std::time::Instant;

use adk_audio::{
    AudioFrame, ChatterboxConfig, ChatterboxTtsProvider, ChatterboxVariant, TtsProvider, TtsRequest,
};
use anyhow::Result;

const OUTPUT_DIR: &str = "samples";

const SAMPLE_TEXT: &str = "The quick brown fox jumps over the lazy dog. \
    This sentence demonstrates every letter in the English alphabet.";

/// All available Chatterbox quantization variants.
const VARIANTS: &[(ChatterboxVariant, &str)] = &[
    (ChatterboxVariant::Fp32, "fp32"),
    (ChatterboxVariant::Fp16, "fp16"),
    (ChatterboxVariant::Q4, "q4"),
    (ChatterboxVariant::Q4F16, "q4f16"),
];

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt::init();
    std::fs::create_dir_all(OUTPUT_DIR)?;

    println!("=== adk-audio: Chatterbox Advanced (variant comparison + voice cloning) ===\n");

    // --- Part 1: Compare all quantization variants ---
    println!("--- Part 1: Quantization Variant Comparison ---\n");
    println!("Text: \"{SAMPLE_TEXT}\"\n");

    let reference_wav = find_reference_wav();
    println!("Reference WAV: {}\n", reference_wav.display());

    let mut results: Vec<(&str, u64, f64, f64)> = Vec::new();

    for (variant, label) in VARIANTS {
        println!("Loading Chatterbox ({label})...");
        let config = ChatterboxConfig {
            variant: *variant,
            reference_wav: Some(reference_wav.clone()),
            ..Default::default()
        };

        let provider = match ChatterboxTtsProvider::load(config).await {
            Ok(p) => p,
            Err(e) => {
                println!("  ⚠ Skipping {label}: {e}\n");
                continue;
            }
        };

        let request = TtsRequest {
            text: SAMPLE_TEXT.into(),
            voice: String::new(),
            speed: 1.0,
            ..Default::default()
        };

        let start = Instant::now();
        let frame = provider.synthesize(&request).await?;
        let elapsed = start.elapsed().as_secs_f64();

        let filename = format!("{OUTPUT_DIR}/chatterbox_{label}.wav");
        write_wav(&filename, &frame)?;
        let file_size = std::fs::metadata(&filename)?.len();
        let duration_s = frame.duration_ms as f64 / 1000.0;

        println!(
            "  {label}: {file_size} bytes, {duration_s:.1}s audio, synthesized in {elapsed:.2}s"
        );
        results.push((label, file_size, duration_s, elapsed));
    }

    // Print comparison table
    if !results.is_empty() {
        println!("\n{:<8} {:>12} {:>10} {:>14}", "Variant", "File Size", "Duration", "Synth Time");
        println!("{}", "-".repeat(48));
        for (label, size, dur, synth) in &results {
            println!("{label:<8} {size:>10} B {dur:>8.1}s {synth:>12.2}s");
        }
    }

    // --- Part 2: Voice cloning demonstration ---
    println!("\n--- Part 2: Voice Cloning ---\n");

    let clone_ref = find_reference_wav();
    println!("Cloning voice from: {}", clone_ref.display());

    let config = ChatterboxConfig {
        variant: ChatterboxVariant::Fp32,
        reference_wav: Some(clone_ref),
        exaggeration: 0.7,
        ..Default::default()
    };

    let provider = match ChatterboxTtsProvider::load(config).await {
        Ok(p) => p,
        Err(e) => {
            println!("  ⚠ Skipping voice cloning: {e}\n");
            println!("\nDone! WAV files written to {OUTPUT_DIR}/");
            return Ok(());
        }
    };

    let phrases = [
        "Hello! I am a cloned voice speaking through Chatterbox.",
        "Voice cloning lets you replicate any speaker from a short audio sample.",
    ];

    for (i, text) in phrases.iter().enumerate() {
        let request = TtsRequest {
            text: (*text).into(),
            voice: String::new(),
            speed: 1.0,
            ..Default::default()
        };

        let start = Instant::now();
        match provider.synthesize(&request).await {
            Ok(frame) => {
                let elapsed = start.elapsed().as_secs_f64();
                let filename = format!("{OUTPUT_DIR}/chatterbox_clone_{i}.wav");
                write_wav(&filename, &frame)?;
                let file_size = std::fs::metadata(&filename)?.len();
                println!(
                    "  Phrase {}: {file_size} bytes, {:.1}s audio, {elapsed:.2}s synth",
                    i + 1,
                    frame.duration_ms as f64 / 1000.0
                );
            }
            Err(e) => {
                println!("  Phrase {} synthesis failed: {e}", i + 1);
            }
        }
    }

    println!("\nDone! WAV files written to {OUTPUT_DIR}/");
    Ok(())
}

/// Find a reference WAV file for voice cloning.
fn find_reference_wav() -> PathBuf {
    let candidates = [
        "samples/openai_tts_alloy.wav",
        "samples/kokoro_af_sky.wav",
        "samples/gemini_tts_kore.wav",
    ];
    for path in &candidates {
        let p = Path::new(path);
        if p.exists() {
            return p.to_path_buf();
        }
    }

    // Generate a simple reference tone if no WAV exists
    let path = PathBuf::from("samples/reference_tone.wav");
    if !path.exists() {
        println!("No reference WAV found, generating a simple tone...");
        let sample_rate = 24000u32;
        let duration_secs = 3.0f64;
        let num_samples = (sample_rate as f64 * duration_secs) as usize;
        let mut samples = Vec::with_capacity(num_samples);
        for i in 0..num_samples {
            let t = i as f64 / sample_rate as f64;
            let sample = (t * 440.0 * 2.0 * std::f64::consts::PI).sin() * 0.3;
            samples.push((sample * 32767.0) as i16);
        }
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&path, spec).expect("create WAV");
        for s in &samples {
            writer.write_sample(*s).expect("write sample");
        }
        writer.finalize().expect("finalize WAV");
    }
    path
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
