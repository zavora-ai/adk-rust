//! Voice Activity Detection trait.
//!
//! # No backend ships with this crate
//!
//! `adk-audio` defines the [`VadProcessor`] boundary but implements it nowhere.
//! Callers supply their own detector; the trait compiles under every feature set.
//!
//! # `&self` is the constraint on any streaming backend
//!
//! [`VadProcessor::is_speech`] takes `&self`, and the trait is `Send + Sync`, so
//! implementors are shared immutably (`Arc<dyn VadProcessor>` throughout
//! [`crate::pipeline`] and the desktop turn detector). Every serious streaming
//! VAD — Silero, TEN, Earshot — is *recurrent*: classifying a frame mutates
//! per-stream state. Such a detector cannot implement this trait without
//! interior mutability, and a `Mutex` in the per-frame path is exactly what a
//! real-time audio loop must not have.
//!
//! So a stateful backend cannot simply implement `VadProcessor`. It needs one
//! detector instance per stream, owned mutably by that stream, with an explicit
//! `reset()` at session boundaries — and a documented compatibility wrapper for
//! existing `Arc<dyn VadProcessor>` callers, not a silent lock.
//!
//! # This is a primitive, not a policy
//!
//! A `VadProcessor` reports whether audio contains speech. It carries no
//! call-control authority on its own: endpointing, answering-machine detection,
//! barge-in, and turn-taking are separate decisions layered above it. Provider
//! server VAD remains the conversational turn-taking authority unless an
//! application explicitly chooses otherwise.

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
