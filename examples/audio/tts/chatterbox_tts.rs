//! Chatterbox TTS example — voice cloning with a 4-model ONNX pipeline.
//!
//! Demonstrates:
//! - ChatterboxTtsProvider with automatic model download from HuggingFace Hub
//! - Voice cloning from a reference WAV file
//! - Multiple quantization variants (fp32, fp16, q4, q8)
//! - Optional round-trip with Whisper STT (if OPENAI_API_KEY is set)
//!
//! No API keys required for TTS — runs entirely on-device.
//! Models auto-download on first run (~1.4GB for fp32).
//!
//! # Run
//!
//! ```bash
//! # From the examples/ directory:
//! cargo run --example audio_chatterbox_tts --features chatterbox
//! ```

use adk_audio::{
    AudioFrame, ChatterboxConfig, ChatterboxTtsProvider, ChatterboxVariant, SttProvider,
    TtsProvider, TtsRequest,
};
use anyhow::Result;

const OUTPUT_DIR: &str = "samples";

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt::init();
    std::fs::create_dir_all(OUTPUT_DIR)?;

    println!("=== adk-audio: Chatterbox TTS (4-model ONNX pipeline) ===\n");

    // Find a reference WAV for voice cloning
    let reference_wav = find_reference_wav();
    println!("Reference WAV: {}\n", reference_wav.display());

    let config = ChatterboxConfig {
        repo_id: "onnx-community/chatterbox-ONNX".into(),
        variant: ChatterboxVariant::Fp32,
        reference_wav: Some(reference_wav),
        max_new_tokens: 2000,
        repetition_penalty: 1.2,
        ..Default::default()
    };

    println!("Loading Chatterbox models (auto-downloads ~1.4GB on first run)...");
    let provider = ChatterboxTtsProvider::load(config).await?;
    println!("Models loaded.\n");

    // Synthesize speech
    let text = "Hello! This is Chatterbox, a voice cloning text to speech model. \
                It can clone any voice from just a short audio sample.";
    println!("Synthesizing: \"{text}\"");

    let request = TtsRequest {
        text: text.into(),
        voice: String::new(), // uses reference_wav from config
        speed: 1.0,
        ..Default::default()
    };

    let frame = provider.synthesize(&request).await?;
    let filename = format!("{OUTPUT_DIR}/chatterbox_output.wav");
    write_wav(&filename, &frame)?;
    println!(
        "  → {filename}: {} bytes, {:.1}s\n",
        std::fs::metadata(&filename)?.len(),
        frame.duration_ms as f64 / 1000.0
    );

    // Round-trip with Whisper STT (optional)
    if std::env::var("OPENAI_API_KEY").is_ok() {
        println!("Round-trip: Chatterbox TTS → Whisper STT:");
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

/// Find a reference WAV file for voice cloning.
fn find_reference_wav() -> std::path::PathBuf {
    // Try audio_samples directory first
    let candidates = [
        "samples/openai_tts_alloy.wav",
        "samples/kokoro_af_sky.wav",
        "samples/gemini_tts_kore.wav",
        "samples/elevenlabs_rachel.wav",
    ];
    for path in &candidates {
        let p = std::path::Path::new(path);
        if p.exists() {
            return p.to_path_buf();
        }
    }

    // Generate a simple reference WAV with silence + tone
    let path = std::path::PathBuf::from("samples/reference_tone.wav");
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
