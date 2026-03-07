//! Local TTS example — synthesize speech offline using Kokoro-82M via adk-audio's OnnxTtsProvider.
//!
//! Demonstrates:
//! - OnnxTtsProvider with KokoroPreprocessor (espeak-ng phonemizer + ONNX Runtime)
//! - Hardware-accelerated inference (CoreML on macOS, CUDA on Linux/Windows)
//! - Multiple voices (American, British, male, female)
//! - Speed control
//! - Round-trip with Whisper STT (if OPENAI_API_KEY is set)
//!
//! No API keys required for TTS — runs entirely on-device.
//! Model files auto-download on first run (~340MB to ~/.cache/kokoros/).
//!
//! # Prerequisites
//!
//! ```bash
//! brew install espeak-ng pkg-config
//! ```
//!
//! # Run
//!
//! ```bash
//! cargo run --example audio_local_kokoro --features kokoro
//! ```

use adk_audio::{
    AudioFrame, KokoroPreprocessor, OnnxExecutionProvider, OnnxModelConfig, OnnxTtsProvider,
    SttProvider, TtsProvider, TtsRequest, Voice,
};
use anyhow::Result;

const OUTPUT_DIR: &str = "samples";
const SAMPLE_RATE: u32 = 24000;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    std::fs::create_dir_all(OUTPUT_DIR)?;

    println!("=== adk-audio: Local Kokoro TTS (OnnxTtsProvider + KokoroPreprocessor) ===\n");

    // Model + voices auto-download to ~/.cache/kokoros/ on first run
    let home = std::env::var("HOME")?;
    let model_dir = format!("{home}/.cache/kokoros");
    let model_path = format!("{model_dir}/kokoro-v1.0.onnx");
    let voices_path = format!("{model_dir}/voices-v1.0.bin");

    // Ensure model files exist (download if needed)
    ensure_model_files(&model_path, &voices_path).await?;

    println!("Loading KokoroPreprocessor (espeak-ng phonemizer + voice embeddings)...");
    let preprocessor = KokoroPreprocessor::new(std::path::Path::new(&voices_path), "en-us")?;

    // Build voice catalog from the preprocessor's loaded voices
    let voice_names = preprocessor.voices().available_voices();
    println!("Available voices ({}):", voice_names.len());
    for v in &voice_names {
        println!("  {v}");
    }
    println!();

    // Build voice catalog for the provider
    let voice_catalog: Vec<Voice> = voice_names
        .iter()
        .map(|name| Voice {
            id: name.clone(),
            name: name.clone(),
            language: "en".into(),
            gender: None,
        })
        .collect();

    println!(
        "Creating OnnxTtsProvider (execution provider: {})...",
        OnnxExecutionProvider::auto_detect()
    );
    let config = OnnxModelConfig {
        model_id: "kokoro".into(),
        execution_provider: OnnxExecutionProvider::auto_detect(),
        num_threads: None,
        max_length: 4096,
        sample_rate: SAMPLE_RATE,
        onnx_filename: "kokoro-v1.0.onnx".into(),
    };

    let mut provider = OnnxTtsProvider::with_preprocessor(config, &model_dir, preprocessor)?;
    provider.set_voices(voice_catalog);
    println!("Provider ready.\n");

    // Synthesize with different voices
    let voices = [
        ("af_sky", "Sky (American female)"),
        ("af_bella", "Bella (American female)"),
        ("am_adam", "Adam (American male)"),
        ("bf_emma", "Emma (British female)"),
        ("bm_george", "George (British male)"),
    ];

    for (voice_id, label) in &voices {
        println!("Synthesizing with {label}...");
        let request = TtsRequest {
            text: format!(
                "Hello from {label}. This is local text to speech, running entirely on your machine."
            ),
            voice: voice_id.to_string(),
            speed: 1.0,
            ..Default::default()
        };
        let frame = provider.synthesize(&request).await?;
        let filename = format!("{OUTPUT_DIR}/kokoro_{voice_id}.wav");
        write_wav(&filename, &frame)?;
        println!("  → {filename}: {} bytes\n", std::fs::metadata(&filename)?.len());
    }

    // Speed control demo
    println!("Speed control (0.8x = slower, more natural)...");
    let request = TtsRequest {
        text: "This sentence is spoken at a slower, more natural pace.".into(),
        voice: "af_sky".into(),
        speed: 0.8,
        ..Default::default()
    };
    let frame = provider.synthesize(&request).await?;
    let filename = format!("{OUTPUT_DIR}/kokoro_slow.wav");
    write_wav(&filename, &frame)?;
    println!("  → {filename}: {} bytes\n", std::fs::metadata(&filename)?.len());

    // Round-trip with Whisper STT (optional)
    if std::env::var("OPENAI_API_KEY").is_ok() {
        println!("Round-trip: OnnxTtsProvider (Kokoro) → Whisper STT:");
        let original = "The quick brown fox jumps over the lazy dog.";
        println!("  Original: \"{original}\"");

        let request = TtsRequest {
            text: original.into(),
            voice: "af_sky".into(),
            speed: 1.0,
            ..Default::default()
        };
        let frame = provider.synthesize(&request).await?;

        let stt = adk_audio::WhisperApiStt::from_env()?;
        let opts = adk_audio::SttOptions { language: Some("en".into()), ..Default::default() };
        let transcript = stt.transcribe(&frame, &opts).await?;
        println!("  Transcribed: \"{}\"\n", transcript.text);
    } else {
        println!("(Set OPENAI_API_KEY to enable Whisper STT round-trip test)\n");
    }

    println!("Done! WAV files written to {OUTPUT_DIR}/");
    Ok(())
}

/// Write an AudioFrame to a WAV file (PCM16, mono).
fn write_wav(path: &str, frame: &AudioFrame) -> Result<()> {
    let spec = hound::WavSpec {
        channels: frame.channels as u16,
        sample_rate: frame.sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec)?;
    // frame.data is PCM16 little-endian bytes
    for chunk in frame.data.chunks_exact(2) {
        let sample = i16::from_le_bytes([chunk[0], chunk[1]]);
        writer.write_sample(sample)?;
    }
    writer.finalize()?;
    Ok(())
}

/// Ensure Kokoro model files are downloaded.
async fn ensure_model_files(model_path: &str, voices_path: &str) -> Result<()> {
    let model_dir = std::path::Path::new(model_path).parent().unwrap_or(std::path::Path::new("."));
    std::fs::create_dir_all(model_dir)?;

    if !std::path::Path::new(model_path).exists() {
        println!("Downloading Kokoro model (~340MB)...");
        let url = "https://github.com/thewh1teagle/kokoro-onnx/releases/download/model-files-v1.0/kokoro-v1.0.onnx";
        download_file(url, model_path).await?;
    }
    if !std::path::Path::new(voices_path).exists() {
        println!("Downloading Kokoro voices...");
        let url = "https://github.com/thewh1teagle/kokoro-onnx/releases/download/model-files-v1.0/voices-v1.0.bin";
        download_file(url, voices_path).await?;
    }
    Ok(())
}

async fn download_file(url: &str, dest: &str) -> Result<()> {
    let response = reqwest::get(url).await?;
    let bytes = response.bytes().await?;
    std::fs::write(dest, &bytes)?;
    println!("  Downloaded to {dest} ({} bytes)", bytes.len());
    Ok(())
}
