//! Built-in audio processors (DSP effects).
//!
//! Requires the `fx` feature flag.

mod compressor;
mod noise;
mod normalizer;
mod pitch;
mod resampler;
mod trimmer;

pub use compressor::DynamicRangeCompressor;
pub use noise::NoiseSuppressor;
pub use normalizer::LoudnessNormalizer;
pub use pitch::PitchShifter;
pub use resampler::Resampler;
pub use trimmer::SilenceTrimmer;
