//! Local Whisper STT example — transcribe audio on-device using ONNX Runtime.
//!
//! Demonstrates:
//! - `OnnxSttProvider` with Whisper base model (auto-downloads on first run)
//! - Language auto-detection (no language hint)
//! - WAV file transcription from disk
//! - TTS→STT round-trip: synthesize with Kokoro, transcribe with Whisper
//!
//! No API keys required — runs entirely on-device via ONNX Runtime.
//!
//! # Prerequisites
//!
//! ```bash
//! # macOS (for Kokoro TTS phonemizer)
//! brew install espeak-ng pkg-config
//! ```
//!
//! # Run
//!
//! ```bash
//! # With a WAV file:
//! cargo run --example audio_whisper_local --features whisper-onnx,kokoro -- path/to/audio.wav
//!
//! # Without a WAV file (uses Kokoro TTS→Whisper STT round-trip):
//! cargo run --example audio_whisper_local --features whisper-onnx,kokoro
//! ```

use adk_audio::{
    AudioFrame, OnnxSttConfig, OnnxSttProvider, OnnxTtsProvider, SttBackend, SttOptions,
    SttProvider, TtsProvider, TtsRequest, WhisperModelSize,
};
use anyhow::Result;

const OUTPUT_DIR: &str = "samples";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    std::fs::create_dir_all(OUTPUT_DIR)?;

    println!("=== adk-audio: Local Whisper STT (OnnxSttProvider) ===\n");

    let args: Vec<String> = std::env::args().collect();
    let wav_path = args.get(1);

    // Build Whisper STT provider (auto-downloads model on first run)
    println!("Loading Whisper base model (ONNX)...");
    let config = OnnxSttConfig::builder()
        .stt_backend(SttBackend::Whisper)
        .model_size(WhisperModelSize::Base)
        .build()?;
    let registry = adk_audio::LocalModelRegistry::default();
    let stt = OnnxSttProvider::new(config, &registry).await?;
    println!("Whisper model loaded.\n");

    if let Some(path) = wav_path {
        // --- Part 1: Transcribe a WAV file from disk ---
        transcribe_wav_file(&stt, path).await?;
    } else {
        println!("No WAV file provided. Running TTS→STT round-trip demo instead.\n");
        println!("(Tip: pass a WAV file path as argument to transcribe it)\n");
    }

    // --- Part 2: TTS→STT round-trip with Kokoro ---
    match tts_stt_roundtrip(&stt).await {
        Ok(()) => {}
        Err(e) => {
            println!("TTS→STT round-trip skipped: {e}");
            println!("(This usually means the Kokoro model requires HF_TOKEN for download)");
        }
    }

    println!("\nDone!");
    Ok(())
}

/// Transcribe a WAV file from disk and print results.
async fn transcribe_wav_file(stt: &OnnxSttProvider, path: &str) -> Result<()> {
    println!("--- Transcribing WAV file: {path} ---\n");

    let frame = load_wav(path)?;
    println!(
        "Audio: {}ms, {}Hz, {} channel(s), {} bytes",
        frame.duration_ms,
        frame.sample_rate,
        frame.channels,
        frame.data.len()
    );

    // Transcribe without language hint (auto-detection)
    let opts = SttOptions::default();
    let start = std::time::Instant::now();
    let transcript = stt.transcribe(&frame, &opts).await?;
    let elapsed = start.elapsed();

    println!("Transcript: \"{}\"", transcript.text);
    println!("Confidence: {:.2}", transcript.confidence);
    if let Some(lang) = &transcript.language_detected {
        println!("Detected language: {lang}");
    }
    println!("Inference time: {elapsed:.2?}\n");

    Ok(())
}

/// Synthesize text with Kokoro TTS, then transcribe with Whisper STT.
async fn tts_stt_roundtrip(stt: &OnnxSttProvider) -> Result<()> {
    println!("--- TTS→STT Round-Trip (Kokoro → Whisper) ---\n");

    // Load Kokoro TTS provider
    println!("Loading Kokoro TTS model (ONNX)...");
    let tts = OnnxTtsProvider::default_kokoro().await?;
    println!("Kokoro model loaded.\n");

    let original = "Hello, this is a test of the Whisper speech recognition system.";
    println!("Original text: \"{original}\"");

    // Synthesize with Kokoro
    let request = TtsRequest {
        text: original.into(),
        voice: "af_sky".into(),
        speed: 1.0,
        ..Default::default()
    };
    println!("Synthesizing with Kokoro...");
    let frame = tts.synthesize(&request).await?;
    println!(
        "Synthesized: {}ms, {}Hz, {} bytes",
        frame.duration_ms,
        frame.sample_rate,
        frame.data.len()
    );

    // Save synthesized audio
    let wav_path = format!("{OUTPUT_DIR}/whisper_roundtrip_input.wav");
    write_wav(&wav_path, &frame)?;
    println!("Saved to {wav_path}");

    // Transcribe with Whisper (no language hint — auto-detect)
    println!("Transcribing with Whisper (language auto-detection)...");
    let opts = SttOptions::default();
    let start = std::time::Instant::now();
    let transcript = stt.transcribe(&frame, &opts).await?;
    let elapsed = start.elapsed();

    println!("\nResults:");
    println!("  Transcript:  \"{}\"", transcript.text);
    println!("  Confidence:  {:.2}", transcript.confidence);
    if let Some(lang) = &transcript.language_detected {
        println!("  Language:    {lang}");
    }
    println!("  Inference:   {elapsed:.2?}");

    Ok(())
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
                // Convert other bit depths to i16
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
