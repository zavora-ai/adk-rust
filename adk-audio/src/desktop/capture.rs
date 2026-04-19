//! Microphone capture component.
//!
//! Opens a system input device via `cpal` and produces a stream of
//! [`AudioFrame`] values in PCM-16 LE format. The returned [`AudioStream`]
//! can be consumed directly or passed to [`VadTurnManager`](super::turn::VadTurnManager).
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_audio::desktop::{AudioCapture, CaptureConfig};
//!
//! let mut capture = AudioCapture::new();
//! let devices = AudioCapture::list_input_devices()?;
//! if let Some(device) = devices.first() {
//!     let config = CaptureConfig::default();
//!     let mut stream = capture.start_capture(device.id(), &config)?;
//!     while let Some(frame) = stream.recv().await {
//!         // Process frame...
//!     }
//!     capture.stop_capture();
//! }
//! ```

use std::sync::Mutex;

use bytes::Bytes;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use tokio::sync::mpsc;

use crate::error::{AudioError, AudioResult};
use crate::frame::AudioFrame;

use super::device::{AudioDevice, CaptureConfig};

/// Asynchronous stream of [`AudioFrame`] values from a microphone.
///
/// This is a bounded `tokio::sync::mpsc::Receiver<AudioFrame>` with a
/// capacity of 64 frames, providing backpressure when the consumer falls
/// behind.
pub type AudioStream = mpsc::Receiver<AudioFrame>;

/// Newtype wrapper around `cpal::Stream` to provide `Sync`.
///
/// `cpal::Stream` is `Send` but not `Sync` on all platforms. We only
/// access the stream through `&mut self` methods, so wrapping it in a
/// newtype with `unsafe impl Sync` is safe — there is no concurrent
/// shared access.
struct SyncStream(#[allow(dead_code)] cpal::Stream);

// SAFETY: `cpal::Stream` is `Send`. We only access it through `&mut self`
// methods on `AudioCapture`, so there is no concurrent shared access.
// The `Sync` bound is required so that `AudioCapture` itself is `Sync`.
unsafe impl Sync for SyncStream {}

// SAFETY: `cpal::Stream` is already `Send`.
unsafe impl Send for SyncStream {}

/// Microphone capture component.
///
/// Opens a system input device via `cpal` and produces a stream of
/// [`AudioFrame`] values in PCM-16 LE format.
///
/// # Thread Safety
///
/// `AudioCapture` is `Send + Sync`, making it safe to share across
/// async tasks in a Tokio application.
pub struct AudioCapture {
    /// Active cpal stream handle (kept alive to maintain capture).
    stream: Option<SyncStream>,
}

impl AudioCapture {
    /// Create a new `AudioCapture` instance with no active capture session.
    pub fn new() -> Self {
        Self { stream: None }
    }

    /// List all available input (microphone) devices.
    ///
    /// Returns an empty list if no devices are available.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::Device`] if the system audio host is unavailable.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let devices = AudioCapture::list_input_devices()?;
    /// for device in &devices {
    ///     println!("{}: {}", device.id(), device.name());
    /// }
    /// ```
    pub fn list_input_devices() -> AudioResult<Vec<AudioDevice>> {
        let host = cpal::default_host();
        let devices = host.input_devices().map_err(|e| {
            AudioError::Device(format!(
                "failed to enumerate input devices: {e}. Check that audio drivers are installed."
            ))
        })?;

        let result: Vec<AudioDevice> = devices
            .filter_map(|device| {
                let name = device.name().unwrap_or_default();
                if name.is_empty() { None } else { Some(AudioDevice::new(name.clone(), name)) }
            })
            .collect();

        Ok(result)
    }

    /// Start capturing audio from the specified device.
    ///
    /// Validates the [`CaptureConfig`], finds the device by ID, opens a
    /// `cpal` input stream, and returns an [`AudioStream`] that produces
    /// [`AudioFrame`] values at the configured frame duration interval.
    ///
    /// The returned channel is bounded with capacity 64. If the consumer
    /// falls behind, frames are dropped and a warning is logged.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::Device`] if:
    /// - The config is invalid (zero sample rate, channels, or frame duration)
    /// - The device is not found
    /// - The device fails to open
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let mut capture = AudioCapture::new();
    /// let config = CaptureConfig::default();
    /// let mut stream = capture.start_capture("Built-in Microphone", &config)?;
    /// ```
    pub fn start_capture(
        &mut self,
        device_id: &str,
        config: &CaptureConfig,
    ) -> AudioResult<AudioStream> {
        config.validate()?;

        // Find the device by matching name against device_id.
        let host = cpal::default_host();
        let devices = host.input_devices().map_err(|e| {
            AudioError::Device(format!(
                "failed to enumerate input devices: {e}. Check that audio drivers are installed."
            ))
        })?;

        let device = devices
            .into_iter()
            .find(|d| d.name().unwrap_or_default() == device_id)
            .ok_or_else(|| {
                AudioError::Device(format!(
                    "input device not found: '{device_id}'. Use list_input_devices() to see available devices."
                ))
            })?;

        let stream_config = cpal::StreamConfig {
            channels: config.channels as u16,
            sample_rate: cpal::SampleRate(config.sample_rate),
            buffer_size: cpal::BufferSize::Default,
        };

        let (tx, rx) = mpsc::channel::<AudioFrame>(64);

        // Calculate how many samples per channel constitute one frame.
        let samples_per_frame = (config.sample_rate as usize
            * config.channels as usize
            * config.frame_duration_ms as usize)
            / 1000;

        let sample_rate = config.sample_rate;
        let channels = config.channels;

        // Shared buffer for accumulating samples across cpal callbacks.
        // Uses std::sync::Mutex because the cpal callback is a non-async context.
        let buffer: Mutex<Vec<i16>> = Mutex::new(Vec::with_capacity(samples_per_frame));

        let cpal_stream = device
            .build_input_stream(
                &stream_config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    // Convert f32 samples to i16 (PCM-16).
                    let mut buf = buffer.lock().expect("audio buffer lock poisoned");
                    for &sample in data {
                        let clamped = sample.clamp(-1.0, 1.0);
                        let as_i16 = (clamped * i16::MAX as f32) as i16;
                        buf.push(as_i16);

                        if buf.len() >= samples_per_frame {
                            // Convert accumulated i16 samples to PCM-16 LE bytes.
                            let pcm_bytes: Vec<u8> = buf
                                .drain(..samples_per_frame)
                                .flat_map(|s| s.to_le_bytes())
                                .collect();

                            let frame =
                                AudioFrame::new(Bytes::from(pcm_bytes), sample_rate, channels);

                            if tx.try_send(frame).is_err() {
                                tracing::warn!(
                                    "audio capture channel full — frame dropped. Consumer may be too slow."
                                );
                            }
                        }
                    }
                },
                move |err| {
                    tracing::error!("cpal input stream error: {err}");
                },
                None, // No timeout
            )
            .map_err(|e| {
                AudioError::Device(format!("failed to open input stream on '{device_id}': {e}"))
            })?;

        cpal_stream.play().map_err(|e| {
            AudioError::Device(format!("failed to start input stream on '{device_id}': {e}"))
        })?;

        self.stream = Some(SyncStream(cpal_stream));

        Ok(rx)
    }

    /// Stop the active capture session and release the device.
    ///
    /// No-op if no capture session is active. Dropping the `cpal::Stream`
    /// stops the OS audio callback.
    pub fn stop_capture(&mut self) {
        self.stream = None;
    }
}

impl Default for AudioCapture {
    fn default() -> Self {
        Self::new()
    }
}

// Static assertions for Send + Sync.
#[allow(dead_code)]
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<AudioCapture>();
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stop_capture_idempotent() {
        let mut capture = AudioCapture::new();
        // Calling stop_capture on a fresh instance should be a no-op.
        capture.stop_capture();
        assert!(capture.stream.is_none());
        // Calling again should also be fine.
        capture.stop_capture();
        assert!(capture.stream.is_none());
    }

    #[test]
    fn test_audio_capture_default() {
        let capture = AudioCapture::default();
        assert!(capture.stream.is_none());
    }

    #[test]
    fn test_audio_capture_is_send_sync() {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}
        assert_send::<AudioCapture>();
        assert_sync::<AudioCapture>();
    }
}
