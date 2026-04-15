//! Cloud STT provider implementations.

mod assemblyai;
mod deepgram;
mod gemini;
mod whisper_api;

pub use assemblyai::AssemblyAiStt;
pub use deepgram::DeepgramStt;
pub use gemini::GeminiStt;
pub use whisper_api::WhisperApiStt;

/// Convert an AudioFrame to WAV bytes for upload to STT APIs.
pub(crate) fn frame_to_wav_bytes(
    frame: &crate::frame::AudioFrame,
) -> crate::error::AudioResult<bytes::Bytes> {
    crate::codec::encode(frame, crate::codec::AudioFormat::Wav)
}
