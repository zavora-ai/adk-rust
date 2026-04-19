//! Desktop audio I/O module.
//!
//! Provides microphone capture, speaker playback, and VAD-driven turn-taking
//! for desktop applications. All types use the `cpal` crate for cross-platform
//! system audio access (CoreAudio on macOS, ALSA/PulseAudio on Linux, WASAPI
//! on Windows).
//!
//! This module is gated behind the `desktop-audio` feature flag.

pub mod capture;
pub mod device;
pub mod playback;
pub mod turn;

pub use capture::{AudioCapture, AudioStream};
pub use device::{AudioDevice, CaptureConfig};
pub use playback::AudioPlayback;
pub use turn::{VadConfig, VadMode, VadTurnManager, VoiceActivityEvent};
