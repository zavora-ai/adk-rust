//! Shared utilities for desktop audio examples.

use adk_audio::{AudioDevice, AudioFrame, SpeechSegment, VadProcessor};

/// Amplitude-based VAD processor for local speech detection.
/// Classifies frames as speech when any sample's absolute value >= threshold.
pub struct MockVad {
    /// Amplitude threshold for speech detection. Non-negative.
    pub threshold: i16,
}

impl VadProcessor for MockVad {
    fn is_speech(&self, frame: &AudioFrame) -> bool {
        frame.samples().iter().any(|&s| (s as i32).abs() >= self.threshold as i32)
    }

    fn segment(&self, _frame: &AudioFrame) -> Vec<SpeechSegment> {
        vec![]
    }
}

/// Install a human-readable tracing subscriber.
pub fn setup_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("desktop_audio_example=info".parse().unwrap()),
        )
        .try_init();
}

/// Print a formatted list of audio devices.
pub fn print_device_list(label: &str, devices: &[AudioDevice]) {
    println!("\n{label}:");
    if devices.is_empty() {
        println!("  (none found)");
    } else {
        for (i, device) in devices.iter().enumerate() {
            println!("  [{i}] {} (id: {})", device.name(), device.id());
        }
    }
}

/// Print summary info about an `AudioFrame`.
pub fn print_frame_info(frame: &AudioFrame, index: usize) {
    println!(
        "  Frame {index}: {}Hz, {}ch, {}ms, {} bytes",
        frame.sample_rate,
        frame.channels,
        frame.duration_ms,
        frame.data.len()
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_audio::AudioFrame;
    use bytes::Bytes;

    fn make_frame(samples: &[i16]) -> AudioFrame {
        let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        AudioFrame::new(Bytes::from(data), 16000, 1)
    }

    #[test]
    fn test_mock_vad_silence() {
        let vad = MockVad { threshold: 500 };
        let frame = make_frame(&[0, 0, 0, 0]);
        assert!(!vad.is_speech(&frame));
    }

    #[test]
    fn test_mock_vad_speech() {
        let vad = MockVad { threshold: 500 };
        let frame = make_frame(&[0, 0, 1000, 0]);
        assert!(vad.is_speech(&frame));
    }

    #[test]
    fn test_mock_vad_threshold_boundary() {
        let vad = MockVad { threshold: 500 };
        // Exactly at threshold → true
        let frame = make_frame(&[0, 500, 0, 0]);
        assert!(vad.is_speech(&frame));
        // Just below threshold → false
        let frame = make_frame(&[0, 499, 0, 0]);
        assert!(!vad.is_speech(&frame));
    }

    #[test]
    fn test_mock_vad_negative_samples() {
        let vad = MockVad { threshold: 500 };
        let frame = make_frame(&[0, -600, 0, 0]);
        assert!(vad.is_speech(&frame));
    }

    #[test]
    fn test_mock_vad_i16_min() {
        // i16::MIN (-32768) should be detected as speech with any reasonable threshold
        let vad = MockVad { threshold: 500 };
        let frame = make_frame(&[i16::MIN]);
        assert!(vad.is_speech(&frame));
    }
}
