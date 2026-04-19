//! Speaker playback component.
//!
//! Routes [`AudioFrame`] values to a system output device via `cpal`.
//! The returned audio is written through a shared sample buffer that the
//! cpal output callback drains. If the buffer is empty, silence (zeros)
//! is written to the output device.
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_audio::desktop::{AudioPlayback, AudioDevice};
//! use adk_audio::AudioFrame;
//!
//! let mut playback = AudioPlayback::new();
//! let devices = AudioPlayback::list_output_devices()?;
//! if let Some(device) = devices.first() {
//!     let frame = AudioFrame::silence(16000, 1, 100);
//!     playback.play(device.id(), &frame).await?;
//!     playback.stop();
//! }
//! ```

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::error::{AudioError, AudioResult};
use crate::frame::AudioFrame;

use super::device::AudioDevice;

/// Newtype wrapper around `cpal::Stream` to provide `Sync`.
///
/// `cpal::Stream` is `Send` but not `Sync` on all platforms. We only
/// access the stream through `&mut self` methods, so wrapping it in a
/// newtype with `unsafe impl Sync` is safe — there is no concurrent
/// shared access.
struct SyncStream(#[allow(dead_code)] cpal::Stream);

// SAFETY: `cpal::Stream` is `Send`. We only access it through `&mut self`
// methods on `AudioPlayback`, so there is no concurrent shared access.
// The `Sync` bound is required so that `AudioPlayback` itself is `Sync`.
unsafe impl Sync for SyncStream {}

// SAFETY: `cpal::Stream` is already `Send`.
unsafe impl Send for SyncStream {}

/// Speaker playback component.
///
/// Routes [`AudioFrame`] values to a system output device via `cpal`.
/// On the first call to [`play()`](AudioPlayback::play), a cpal output
/// stream is opened on the specified device. Subsequent calls queue
/// samples into a shared buffer that the cpal callback drains.
///
/// # Thread Safety
///
/// `AudioPlayback` is `Send + Sync`, making it safe to share across
/// async tasks in a Tokio application.
pub struct AudioPlayback {
    /// Active cpal output stream handle (kept alive to maintain playback).
    stream: Option<SyncStream>,
    /// Shared sample buffer between `play()` and the cpal output callback.
    sample_buffer: Option<Arc<Mutex<VecDeque<i16>>>>,
    /// The device ID of the currently open output device.
    current_device_id: Option<String>,
}

impl AudioPlayback {
    /// Create a new `AudioPlayback` instance with no active playback session.
    pub fn new() -> Self {
        Self { stream: None, sample_buffer: None, current_device_id: None }
    }

    /// List all available output (speaker) devices.
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
    /// let devices = AudioPlayback::list_output_devices()?;
    /// for device in &devices {
    ///     println!("{}: {}", device.id(), device.name());
    /// }
    /// ```
    pub fn list_output_devices() -> AudioResult<Vec<AudioDevice>> {
        let host = cpal::default_host();
        let devices = host.output_devices().map_err(|e| {
            AudioError::Device(format!(
                "failed to enumerate output devices: {e}. Check that audio drivers are installed."
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

    /// Play an [`AudioFrame`] through the specified output device.
    ///
    /// Opens the device on the first call (or if the device changed).
    /// The frame's PCM-16 LE samples are pushed into a shared buffer
    /// that the cpal output callback drains. If the buffer is empty,
    /// the callback writes silence (zeros) to the output.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::Device`] if:
    /// - The device is not found
    /// - The device fails to open
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let frame = AudioFrame::silence(16000, 1, 100);
    /// playback.play("Built-in Output", &frame).await?;
    /// ```
    pub async fn play(&mut self, device_id: &str, frame: &AudioFrame) -> AudioResult<()> {
        // Open a new stream if we don't have one or the device changed.
        let needs_open = match &self.current_device_id {
            Some(current) => current != device_id,
            None => true,
        };

        if needs_open {
            // Drop existing stream first to release the previous device.
            self.stream = None;
            self.sample_buffer = None;
            self.current_device_id = None;

            self.open_stream(device_id, frame.sample_rate, frame.channels)?;
        }

        // Convert the frame's PCM-16 LE bytes to i16 samples and push
        // them into the shared buffer.
        let samples = frame.samples();
        if let Some(buffer) = &self.sample_buffer {
            let mut buf = buffer.lock().expect("playback sample buffer lock poisoned");
            buf.extend(samples.iter().copied());
        }

        Ok(())
    }

    /// Stop active playback and release the output device.
    ///
    /// No-op if no playback is active. Dropping the `cpal::Stream`
    /// stops the OS audio callback.
    pub fn stop(&mut self) {
        self.stream = None;
        self.sample_buffer = None;
        self.current_device_id = None;
    }

    /// Open a cpal output stream on the specified device.
    fn open_stream(&mut self, device_id: &str, sample_rate: u32, channels: u8) -> AudioResult<()> {
        let host = cpal::default_host();
        let devices = host.output_devices().map_err(|e| {
            AudioError::Device(format!(
                "failed to enumerate output devices: {e}. Check that audio drivers are installed."
            ))
        })?;

        let device = devices
            .into_iter()
            .find(|d| d.name().unwrap_or_default() == device_id)
            .ok_or_else(|| {
                AudioError::Device(format!(
                    "output device not found: '{device_id}'. Use list_output_devices() to see available devices."
                ))
            })?;

        let stream_config = cpal::StreamConfig {
            channels: channels as u16,
            sample_rate: cpal::SampleRate(sample_rate),
            buffer_size: cpal::BufferSize::Default,
        };

        let buffer: Arc<Mutex<VecDeque<i16>>> = Arc::new(Mutex::new(VecDeque::new()));
        let callback_buffer = Arc::clone(&buffer);

        let cpal_stream = device
            .build_output_stream(
                &stream_config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    let mut buf =
                        callback_buffer.lock().expect("playback sample buffer lock poisoned");
                    for sample in data.iter_mut() {
                        if let Some(s) = buf.pop_front() {
                            // Convert i16 back to f32 for the output device.
                            *sample = s as f32 / i16::MAX as f32;
                        } else {
                            // No samples available — write silence.
                            *sample = 0.0;
                        }
                    }
                },
                move |err| {
                    tracing::error!("cpal output stream error: {err}");
                },
                None, // No timeout
            )
            .map_err(|e| {
                AudioError::Device(format!("failed to open output stream on '{device_id}': {e}"))
            })?;

        cpal_stream.play().map_err(|e| {
            AudioError::Device(format!("failed to start output stream on '{device_id}': {e}"))
        })?;

        self.stream = Some(SyncStream(cpal_stream));
        self.sample_buffer = Some(buffer);
        self.current_device_id = Some(device_id.to_string());

        Ok(())
    }
}

impl Default for AudioPlayback {
    fn default() -> Self {
        Self::new()
    }
}

// Static assertions for Send + Sync.
#[allow(dead_code)]
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<AudioPlayback>();
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stop_idempotent() {
        let mut playback = AudioPlayback::new();
        // Calling stop on a fresh instance should be a no-op.
        playback.stop();
        assert!(playback.stream.is_none());
        assert!(playback.sample_buffer.is_none());
        assert!(playback.current_device_id.is_none());
        // Calling again should also be fine.
        playback.stop();
        assert!(playback.stream.is_none());
    }

    #[test]
    fn test_audio_playback_default() {
        let playback = AudioPlayback::default();
        assert!(playback.stream.is_none());
        assert!(playback.sample_buffer.is_none());
        assert!(playback.current_device_id.is_none());
    }

    #[test]
    fn test_audio_playback_is_send_sync() {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}
        assert_send::<AudioPlayback>();
        assert_sync::<AudioPlayback>();
    }
}
