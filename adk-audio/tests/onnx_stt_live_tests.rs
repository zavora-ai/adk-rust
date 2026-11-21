//! Live on-device speech recognition on ONNX Runtime: Whisper tiny transcribes a committed
//! segment of the project podcast. Ignored by default, since it needs the
//! `onnx-community/whisper-tiny` export (about 155 MB). Put `tokenizer.json`,
//! `onnx/encoder_model.onnx` and `onnx/decoder_model_merged.onnx` under
//! `$ADK_AUDIO_MODEL_DIR/onnx-community--whisper-tiny`, or leave the variable unset to let
//! `LocalModelRegistry` download the whole repository into the Hugging Face cache.
//!
//! ```text
//! ADK_AUDIO_MODEL_DIR=/path/to/models cargo nextest run -p adk-audio --features whisper-onnx \
//!     --test onnx_stt_live_tests --run-ignored only --no-capture
//! ```

#![cfg(feature = "whisper-onnx")]

use adk_audio::{
    AudioFormat, LocalModelRegistry, OnnxSttConfig, OnnxSttProvider, SttBackend, SttOptions,
    SttProvider, WhisperModelSize, decode,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs onnx-community/whisper-tiny (about 155 MB); see the module docs"]
async fn whisper_tiny_transcribes_a_podcast_segment() {
    // Episode 2's script gives this line as "Exactly. Semver stability. The whole thing."
    let wav = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../docs/podcast/ep2-segments/03-james-semver.wav"
    ))
    .unwrap();
    let audio = decode(&wav, AudioFormat::Wav).unwrap();
    let config = OnnxSttConfig::builder()
        .stt_backend(SttBackend::Whisper)
        .model_size(WhisperModelSize::Tiny)
        .language("en")
        .build()
        .unwrap();
    let registry =
        std::env::var("ADK_AUDIO_MODEL_DIR").map(LocalModelRegistry::new).unwrap_or_default();

    let stt = OnnxSttProvider::new(config, &registry).await.unwrap();
    let started = std::time::Instant::now();
    let transcript = stt.transcribe(&audio, &SttOptions::default()).await.unwrap();

    println!("{:?} in {:?}", transcript.text, started.elapsed());
    let text = transcript.text.to_lowercase();
    // The tiny model hears "Semver stability" as other words; the rest of the line is stable.
    for words in ["exactly", "the whole thing"] {
        assert!(text.contains(words), "`{words}` is missing from {text:?}");
    }
}
