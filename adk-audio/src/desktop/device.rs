//! Audio device descriptor and capture configuration types.
//!
//! Provides [`AudioDevice`] for identifying system audio devices (input or output)
//! and [`CaptureConfig`] for configuring microphone capture parameters.

use crate::error::{AudioError, AudioResult};

/// Descriptor for a system audio device (input or output).
///
/// Wraps an opaque platform-native device identifier and a human-readable
/// display name. Callers should not parse or interpret the `id` value —
/// it is only meaningful when passed back to [`AudioCapture`] or
/// [`AudioPlayback`] methods.
///
/// # Example
///
/// ```
/// use adk_audio::desktop::device::AudioDevice;
///
/// let device = AudioDevice::new("hw:0,0", "Built-in Microphone");
/// assert_eq!(device.id(), "hw:0,0");
/// assert_eq!(device.name(), "Built-in Microphone");
/// ```
#[derive(Debug, Clone)]
pub struct AudioDevice {
    /// Opaque platform-native device identifier.
    id: String,
    /// Human-readable display name (e.g. "Built-in Microphone").
    name: String,
}

impl AudioDevice {
    /// Create a new `AudioDevice` with the given identifier and display name.
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self { id: id.into(), name: name.into() }
    }

    /// Returns the opaque device identifier.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the human-readable display name.
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// Configuration for microphone capture.
///
/// Controls the sample rate, channel count, and frame duration of audio
/// captured by [`AudioCapture`]. Call [`validate()`](CaptureConfig::validate)
/// before passing to `start_capture()` to catch invalid values early.
///
/// # Example
///
/// ```
/// use adk_audio::desktop::device::CaptureConfig;
///
/// let config = CaptureConfig::default();
/// assert_eq!(config.sample_rate, 16000);
/// assert_eq!(config.channels, 1);
/// assert_eq!(config.frame_duration_ms, 20);
/// assert!(config.validate().is_ok());
/// ```
#[derive(Debug, Clone)]
pub struct CaptureConfig {
    /// Sample rate in Hz (e.g. 16000, 44100, 48000).
    pub sample_rate: u32,
    /// Number of channels (1 = mono, 2 = stereo).
    pub channels: u8,
    /// Duration of each produced `AudioFrame` in milliseconds.
    pub frame_duration_ms: u32,
}

impl CaptureConfig {
    /// Validate the configuration, returning an error on invalid values.
    ///
    /// Rejects configurations where any of `sample_rate`, `channels`, or
    /// `frame_duration_ms` is zero.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::Device`] with a descriptive message if any
    /// field is zero.
    pub fn validate(&self) -> AudioResult<()> {
        if self.sample_rate == 0 {
            return Err(AudioError::Device("invalid sample rate: 0".into()));
        }
        if self.channels == 0 {
            return Err(AudioError::Device("invalid channel count: 0".into()));
        }
        if self.frame_duration_ms == 0 {
            return Err(AudioError::Device("invalid frame duration: 0 ms".into()));
        }
        Ok(())
    }
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self { sample_rate: 16000, channels: 1, frame_duration_ms: 20 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audio_device_accessors() {
        let device = AudioDevice::new("dev-123", "Test Microphone");
        assert_eq!(device.id(), "dev-123");
        assert_eq!(device.name(), "Test Microphone");
    }

    #[test]
    fn test_audio_device_clone() {
        let device = AudioDevice::new("id", "name");
        let cloned = device.clone();
        assert_eq!(cloned.id(), device.id());
        assert_eq!(cloned.name(), device.name());
    }

    #[test]
    fn test_capture_config_defaults() {
        let config = CaptureConfig::default();
        assert_eq!(config.sample_rate, 16000);
        assert_eq!(config.channels, 1);
        assert_eq!(config.frame_duration_ms, 20);
    }

    #[test]
    fn test_capture_config_validate_ok() {
        let config = CaptureConfig::default();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_capture_config_validate_zero_sample_rate() {
        let config = CaptureConfig { sample_rate: 0, ..Default::default() };
        let err = config.validate().unwrap_err();
        assert!(matches!(err, AudioError::Device(msg) if msg.contains("sample rate")));
    }

    #[test]
    fn test_capture_config_validate_zero_channels() {
        let config = CaptureConfig { channels: 0, ..Default::default() };
        let err = config.validate().unwrap_err();
        assert!(matches!(err, AudioError::Device(msg) if msg.contains("channel count")));
    }

    #[test]
    fn test_capture_config_validate_zero_frame_duration() {
        let config = CaptureConfig { frame_duration_ms: 0, ..Default::default() };
        let err = config.validate().unwrap_err();
        assert!(matches!(err, AudioError::Device(msg) if msg.contains("frame duration")));
    }
}
