//! Mel spectrogram computation for Whisper / Distil-Whisper ONNX inference.
//!
//! Provides the full audio preprocessing pipeline: stereo→mono downmix,
//! resampling to 16 kHz, STFT with Hanning window, 80-band mel filterbank,
//! and log-mel normalization. The output is a flat `Vec<f32>` of exactly
//! `80 × 3000 = 240 000` elements suitable for the Whisper encoder input
//! tensor `[1, 80, 3000]`.
//!
//! All computation is self-contained — no external FFT crate is required.

use crate::error::{AudioError, AudioResult};
use crate::frame::AudioFrame;

/// Number of mel frequency bands for Whisper.
const N_MELS: usize = 80;

/// FFT size (zero-padded from the 400-sample window).
const N_FFT: usize = 512;

/// Number of frequency bins: `N_FFT / 2 + 1`.
const N_BINS: usize = N_FFT / 2 + 1; // 257

/// Window size in samples (25 ms at 16 kHz).
const WINDOW_SIZE: usize = 400;

/// Hop size in samples (10 ms at 16 kHz).
const HOP_SIZE: usize = 160;

/// Target number of time frames (30 seconds at 10 ms hop).
const TARGET_FRAMES: usize = 3000;

/// Target sample rate for Whisper.
const TARGET_SAMPLE_RATE: u32 = 16_000;

/// Expected output length: `N_MELS × TARGET_FRAMES`.
const OUTPUT_LEN: usize = N_MELS * TARGET_FRAMES;

/// Downmix multi-channel audio to mono by averaging channels.
///
/// If `channels == 1`, returns a clone of the input. If `channels == 2`,
/// averages each pair of samples. Other channel counts are not supported
/// by `AudioFrame` but are handled gracefully by averaging groups.
///
/// # Example
///
/// ```rust,ignore
/// let stereo = vec![100i16, 200, 300, 400];
/// let mono = downmix_to_mono(&stereo, 2);
/// assert_eq!(mono, vec![150, 350]);
/// ```
pub fn downmix_to_mono(samples: &[i16], channels: u8) -> Vec<i16> {
    if channels <= 1 {
        return samples.to_vec();
    }
    let ch = channels as usize;
    let n_frames = samples.len() / ch;
    let mut mono = Vec::with_capacity(n_frames);
    for i in 0..n_frames {
        let mut sum: i32 = 0;
        for c in 0..ch {
            sum += samples[i * ch + c] as i32;
        }
        mono.push((sum / ch as i32) as i16);
    }
    mono
}

/// Resample f32 audio samples to 16 kHz using linear interpolation.
///
/// If `source_rate` is already 16 000, returns a clone. Otherwise performs
/// sample-rate conversion via linear interpolation between adjacent samples.
///
/// # Example
///
/// ```rust,ignore
/// let samples_48k = vec![0.0f32; 48000]; // 1 second at 48 kHz
/// let samples_16k = resample_to_16khz(&samples_48k, 48000);
/// assert_eq!(samples_16k.len(), 16000);
/// ```
pub fn resample_to_16khz(samples: &[f32], source_rate: u32) -> Vec<f32> {
    if source_rate == TARGET_SAMPLE_RATE {
        return samples.to_vec();
    }
    if samples.is_empty() || source_rate == 0 {
        return Vec::new();
    }

    let ratio = source_rate as f64 / TARGET_SAMPLE_RATE as f64;
    let output_len = ((samples.len() as f64) / ratio).round() as usize;
    if output_len == 0 {
        return Vec::new();
    }

    let mut output = Vec::with_capacity(output_len);
    for i in 0..output_len {
        let src_pos = i as f64 * ratio;
        let idx = src_pos as usize;
        let frac = (src_pos - idx as f64) as f32;

        let sample = if idx + 1 < samples.len() {
            samples[idx] * (1.0 - frac) + samples[idx + 1] * frac
        } else if idx < samples.len() {
            samples[idx]
        } else {
            0.0
        };
        output.push(sample);
    }
    output
}

/// Convert frequency in Hz to the HTK mel scale.
fn hz_to_mel(hz: f32) -> f32 {
    2595.0 * (1.0 + hz / 700.0).log10()
}

/// Convert HTK mel value back to Hz.
fn mel_to_hz(mel: f32) -> f32 {
    700.0 * (10.0_f32.powf(mel / 2595.0) - 1.0)
}

/// Build a triangular mel filterbank matrix.
///
/// Returns a flat `Vec<f32>` of shape `[N_MELS, N_BINS]` where each row
/// is a triangular filter in the frequency domain.
fn build_mel_filterbank() -> Vec<f32> {
    let f_max = TARGET_SAMPLE_RATE as f32 / 2.0; // 8000 Hz Nyquist
    let mel_min = hz_to_mel(0.0);
    let mel_max = hz_to_mel(f_max);

    // N_MELS + 2 equally spaced points on the mel scale
    let mel_points: Vec<f32> = (0..=(N_MELS + 1))
        .map(|i| mel_min + (mel_max - mel_min) * i as f32 / (N_MELS + 1) as f32)
        .collect();
    let hz_points: Vec<f32> = mel_points.iter().map(|&m| mel_to_hz(m)).collect();
    let bin_points: Vec<f32> =
        hz_points.iter().map(|&hz| hz * N_FFT as f32 / TARGET_SAMPLE_RATE as f32).collect();

    let mut filters = vec![0.0f32; N_MELS * N_BINS];
    for mel in 0..N_MELS {
        let left = bin_points[mel];
        let center = bin_points[mel + 1];
        let right = bin_points[mel + 2];

        for bin in 0..N_BINS {
            let b = bin as f32;
            let weight = if b >= left && b <= center && center > left {
                (b - left) / (center - left)
            } else if b > center && b <= right && right > center {
                (right - b) / (right - center)
            } else {
                0.0
            };
            filters[mel * N_BINS + bin] = weight;
        }
    }
    filters
}

/// Build a Hanning window of the given size.
fn hanning_window(size: usize) -> Vec<f32> {
    (0..size)
        .map(|i| {
            let x = std::f32::consts::PI * i as f32 / size as f32;
            x.sin().powi(2)
        })
        .collect()
}

/// Compute the full Whisper mel spectrogram pipeline from an [`AudioFrame`].
///
/// Steps:
/// 1. Downmix to mono if stereo
/// 2. Convert i16 PCM to f32 (divide by 32768.0)
/// 3. Resample to 16 kHz if needed
/// 4. Compute STFT (400-sample Hanning window, 160-sample hop, 512-point DFT)
/// 5. Apply 80-band mel filterbank
/// 6. Take log mel spectrogram with Whisper normalization
/// 7. Pad or truncate to exactly 3000 frames
///
/// Returns a flat `Vec<f32>` of exactly `80 × 3000 = 240 000` elements,
/// laid out as `[n_mels, n_frames]` (mel band major) for the Whisper
/// encoder input tensor `[1, 80, 3000]`.
///
/// # Errors
///
/// Returns [`AudioError::Stt`] if the input frame has zero-length data.
pub fn compute_whisper_mel(audio: &AudioFrame) -> AudioResult<Vec<f32>> {
    let raw_samples = audio.samples();
    if raw_samples.is_empty() {
        return Err(AudioError::Stt {
            provider: "ONNX/Whisper".into(),
            message: "empty audio input for mel spectrogram".into(),
        });
    }

    // 1. Downmix to mono
    let mono = downmix_to_mono(raw_samples, audio.channels);

    // 2. Convert i16 → f32
    let f32_samples: Vec<f32> = mono.iter().map(|&s| s as f32 / 32768.0).collect();

    // 3. Resample to 16 kHz
    let resampled = resample_to_16khz(&f32_samples, audio.sample_rate);

    // 4. Compute STFT magnitude spectrum
    let window = hanning_window(WINDOW_SIZE);
    let n_frames = if resampled.len() >= WINDOW_SIZE {
        (resampled.len() - WINDOW_SIZE) / HOP_SIZE + 1
    } else {
        1
    };

    let mut magnitudes = vec![0.0f32; n_frames * N_BINS];

    for frame_idx in 0..n_frames {
        let start = frame_idx * HOP_SIZE;
        for bin in 0..N_BINS {
            let freq = 2.0 * std::f32::consts::PI * bin as f32 / N_FFT as f32;
            let mut real = 0.0f32;
            let mut imag = 0.0f32;

            // Windowed samples (0..WINDOW_SIZE), then zero-padding (WINDOW_SIZE..N_FFT)
            for (k, &w) in window.iter().enumerate() {
                let sample_idx = start + k;
                let s = if sample_idx < resampled.len() { resampled[sample_idx] } else { 0.0 };
                let windowed = s * w;
                let angle = freq * k as f32;
                real += windowed * angle.cos();
                imag -= windowed * angle.sin();
            }
            // Zero-padded region (k >= WINDOW_SIZE) contributes nothing.
            magnitudes[frame_idx * N_BINS + bin] = (real * real + imag * imag).sqrt();
        }
    }

    // 5. Apply mel filterbank
    let mel_filters = build_mel_filterbank();
    let mut log_mel = vec![0.0f32; n_frames * N_MELS];

    for frame in 0..n_frames {
        for mel in 0..N_MELS {
            let mut sum = 0.0f32;
            for bin in 0..N_BINS {
                sum += magnitudes[frame * N_BINS + bin] * mel_filters[mel * N_BINS + bin];
            }
            // 6. Log with floor to avoid log(0)
            log_mel[frame * N_MELS + mel] = sum.max(1e-10).ln();
        }
    }

    // Whisper normalization: clamp to (max - 8.0), then scale to (value + 4.0) / 4.0
    let max_val = log_mel.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let floor = max_val - 8.0;
    for v in &mut log_mel {
        *v = ((*v).max(floor) + 4.0) / 4.0;
    }

    // 7. Pad or truncate to TARGET_FRAMES, output layout: [n_mels, n_frames]
    let mut output = vec![0.0f32; OUTPUT_LEN];
    let frames_to_copy = n_frames.min(TARGET_FRAMES);

    for mel in 0..N_MELS {
        for frame in 0..frames_to_copy {
            output[mel * TARGET_FRAMES + frame] = log_mel[frame * N_MELS + mel];
        }
        // Remaining frames (if any) stay at 0.0 (silence padding)
    }

    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    #[test]
    fn test_downmix_mono_passthrough() {
        let samples = vec![100i16, 200, 300];
        let result = downmix_to_mono(&samples, 1);
        assert_eq!(result, samples);
    }

    #[test]
    fn test_downmix_stereo() {
        let stereo = vec![100i16, 200, 300, 400];
        let mono = downmix_to_mono(&stereo, 2);
        assert_eq!(mono, vec![150, 350]);
    }

    #[test]
    fn test_resample_passthrough() {
        let samples = vec![1.0f32; 16000];
        let result = resample_to_16khz(&samples, 16000);
        assert_eq!(result.len(), 16000);
    }

    #[test]
    fn test_resample_48k_to_16k() {
        let samples = vec![0.5f32; 48000]; // 1 second at 48 kHz
        let result = resample_to_16khz(&samples, 48000);
        assert_eq!(result.len(), 16000);
    }

    #[test]
    fn test_compute_whisper_mel_output_shape() {
        // 1 second of mono 16 kHz silence
        let n_samples = 16000usize;
        let data: Vec<u8> = vec![0u8; n_samples * 2];
        let frame = AudioFrame::new(Bytes::from(data), 16000, 1);
        let mel = compute_whisper_mel(&frame).unwrap();
        assert_eq!(mel.len(), OUTPUT_LEN);
    }

    #[test]
    fn test_compute_whisper_mel_stereo() {
        // 0.5 second of stereo 44100 Hz
        let n_samples = 44100usize; // 0.5s × 2 channels × 44100
        let data: Vec<u8> = vec![0u8; n_samples * 2];
        let frame = AudioFrame::new(Bytes::from(data), 44100, 2);
        let mel = compute_whisper_mel(&frame).unwrap();
        assert_eq!(mel.len(), OUTPUT_LEN);
    }

    #[test]
    fn test_compute_whisper_mel_empty_error() {
        let frame = AudioFrame::new(Bytes::new(), 16000, 1);
        assert!(compute_whisper_mel(&frame).is_err());
    }

    #[test]
    fn test_mel_filterbank_shape() {
        let filters = build_mel_filterbank();
        assert_eq!(filters.len(), N_MELS * N_BINS);
    }

    #[test]
    fn test_hz_mel_roundtrip() {
        for hz in [0.0, 100.0, 1000.0, 4000.0, 8000.0] {
            let mel = hz_to_mel(hz);
            let back = mel_to_hz(mel);
            assert!((back - hz).abs() < 0.01, "roundtrip failed for {hz} Hz");
        }
    }
}
