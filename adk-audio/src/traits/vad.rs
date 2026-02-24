//! Voice Activity Detection trait.

/// A detected speech segment within an audio frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeechSegment {
    /// Start offset in milliseconds.
    pub start_ms: u32,
    /// End offset in milliseconds.
    pub end_ms: u32,
}

use crate::frame::AudioFrame;

/// Trait for Voice Activity Detection processors.
///
/// Used by the voice agent pipeline to gate STT inference
/// to speech-only segments.
pub trait VadProcessor: Send + Sync {
    /// Returns `true` if the frame contains speech.
    fn is_speech(&self, frame: &AudioFrame) -> bool;

    /// Identify speech segments within the frame.
    fn segment(&self, frame: &AudioFrame) -> Vec<SpeechSegment>;
}
