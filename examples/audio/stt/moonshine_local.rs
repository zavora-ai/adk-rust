//! Local Moonshine STT example — ultra-lightweight on-device ASR via ONNX Runtime.
//!
//! Demonstrates:
//! - `OnnxSttProvider` with Moonshine tiny and base variants (auto-downloads on first run)
//! - Speed comparison between Moonshine Tiny (27M params) and Base (61M params)
//! - Variable-length input (no 30s padding like Whisper)
//! - WAV file transcription from disk
//! - Synthesized audio fallback when no WAV file is provided
//!
//! No API keys required — runs entirely on-device via ONNX Runtime.
//!
//! # Run
//!
//! ```bash
//! # With a WAV file:
//! cargo run --example audio_moonshine_local --features moonshine -- path/to/audio.wav
//!
//! # Without a WAV file (generates a simple test tone):
//! cargo run --example audio_moonshine_local --features moonshine
//! ```

use adk_audio::{
    AudioFrame, LocalModelRegistry, MoonshineVariant, OnnxSttConfig, OnnxSttProvider, SttBackend,
    SttOptions, SttProvider,
};
use anyhow::Result;

const OUTPUT_DIR: &str = "samples";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    std::fs::create_dir_all(OUTPUT_DIR)?;

    println!("=== adk-audio: Moonshine STT — Tiny vs Base Comparison ===\n");

    let args: Vec<String> = std::env::args().collect();
    let wav_path = args.get(1);

    let audio = if let Some(path) = wav_path {
        println!("Loading WAV file: {path}");
        load_wav(path)?
    } else {
        println!("No WAV file provided — generating a 3-second test tone.");
        println!("(Tip: pass a WAV file path as argument for real transcription)\n");
        generate_test_tone()
    };

    let audio_bytes = audio.data.len();
    println!(
        "Audio: {}ms, {}Hz, {} channel(s), {} bytes\n",
        audio.duration_ms, audio.sample_rate, audio.channels, audio_bytes
    );

    let registry = LocalModelRegistry::default();
    let opts = SttOptions::default();

    // --- Moonshine Tiny (27M parameters) ---
    println!("--- Moonshine Tiny (27M params) ---\n");
    let (tiny_transcript, tiny_elapsed) =
        transcribe_with_variant(&registry, MoonshineVariant::Tiny, &audio, &opts).await?;

    // --- Moonshine Base (61M parameters) ---
    println!("--- Moonshine Base (61M params) ---\n");
    let (base_transcript, base_elapsed) =
        transcribe_with_variant(&registry, MoonshineVariant::Base, &audio, &opts).await?;

    // --- Comparison ---
    println!("=== Comparison ===\n");
    println!("  Moonshine Tiny (27M params):");
    println!("    Transcript: \"{}\"", tiny_transcript);
    println!("    Inference:  {tiny_elapsed:.2?}");
    println!("    Audio size: {audio_bytes} bytes");
    println!();
    println!("  Moonshine Base (61M params):");
    println!("    Transcript: \"{}\"", base_transcript);
    println!("    Inference:  {base_elapsed:.2?}");
    println!("    Audio size: {audio_bytes} bytes");
    println!();

    if tiny_elapsed.as_millis() > 0 && base_elapsed.as_millis() > 0 {
        let speedup = base_elapsed.as_secs_f64() / tiny_elapsed.as_secs_f64();
        println!(
            "  Moonshine Tiny is {speedup:.1}x {} than Moonshine Base",
            if speedup > 1.0 { "faster" } else { "slower" }
        );
    }

    println!("\nDone!");
    Ok(())
}

/// Transcribe audio with a specific Moonshine variant, returning the transcript text and elapsed time.
async fn transcribe_with_variant(
    registry: &LocalModelRegistry,
    variant: MoonshineVariant,
    audio: &AudioFrame,
    opts: &SttOptions,
) -> Result<(String, std::time::Duration)> {
    println!("Loading model (auto-downloads on first run)...");
    let config = OnnxSttConfig::builder()
        .stt_backend(SttBackend::Moonshine)
        .moonshine_variant(variant)
        .build()?;
    let stt = match OnnxSttProvider::new(config, registry).await {
        Ok(s) => s,
        Err(e) => {
            println!("Failed to load Moonshine model: {e}");
            println!("This may require HF_TOKEN for gated models. Set it in .env and retry.\n");
            return Ok(("(model unavailable)".into(), std::time::Duration::ZERO));
        }
    };
    println!("Model loaded.");

    println!("Transcribing...");
    let start = std::time::Instant::now();
    let transcript = stt.transcribe(audio, opts).await?;
    let elapsed = start.elapsed();

    println!("Transcript: \"{}\"", transcript.text);
    println!("Confidence: {:.2}", transcript.confidence);
    if let Some(lang) = &transcript.language_detected {
        println!("Language:   {lang}");
    }
    println!("Time:       {elapsed:.2?}\n");

    Ok((transcript.text, elapsed))
}

/// Load a WAV file into an AudioFrame.
fn load_wav(path: &str) -> Result<AudioFrame> {
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    let channels = spec.channels as u8;
    let sample_rate = spec.sample_rate;

    let samples: Vec<i16> =
        match spec.sample_format {
            hound::SampleFormat::Int => {
                if spec.bits_per_sample == 16 {
                    reader.samples::<i16>().collect::<std::result::Result<Vec<_>, _>>()?
                } else {
                    let shift = spec.bits_per_sample as i32 - 16;
                    reader
                        .samples::<i32>()
                        .map(|s| {
                            s.map(|v| {
                                if shift > 0 { (v >> shift) as i16 } else { (v << -shift) as i16 }
                            })
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

/// Generate a 3-second 440Hz sine wave as a test tone (16kHz mono PCM16).
fn generate_test_tone() -> AudioFrame {
    let sample_rate = 16000u32;
    let duration_secs = 3.0f64;
    let frequency = 440.0f64;
    let num_samples = (sample_rate as f64 * duration_secs) as usize;

    let mut bytes = Vec::with_capacity(num_samples * 2);
    for i in 0..num_samples {
        let t = i as f64 / sample_rate as f64;
        let sample = (t * frequency * 2.0 * std::f64::consts::PI).sin();
        let pcm = (sample * 16000.0) as i16; // moderate amplitude
        bytes.extend_from_slice(&pcm.to_le_bytes());
    }

    AudioFrame::new(bytes, sample_rate, 1)
}
