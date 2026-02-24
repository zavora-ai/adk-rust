//! Log-mel spectrogram computation for Whisper STT.

use crate::error::{AudioError, AudioResult};

/// Mel spectrogram data.
pub struct MelSpectrogram {
    /// Flattened mel data (n_frames × n_mels).
    pub data: Vec<f32>,
    /// Number of time frames.
    pub n_frames: usize,
    /// Number of mel frequency bins.
    pub n_mels: usize,
}

/// Compute a log-mel spectrogram suitable for Whisper models.
///
/// Parameters:
/// - `samples`: f32 audio samples normalized to [-1.0, 1.0]
/// - `sample_rate`: input sample rate (should be 16000 for Whisper)
///
/// Uses 80 mel bins, 25ms window (400 samples at 16kHz), 10ms hop (160 samples).
pub fn compute_log_mel_spectrogram(
    samples: &[f32],
    _sample_rate: u32,
) -> AudioResult<MelSpectrogram> {
    let n_fft = 400; // 25ms at 16kHz
    let hop_length = 160; // 10ms at 16kHz
    let n_mels: usize = 80;

    if samples.is_empty() {
        return Err(AudioError::Stt {
            provider: "MLX".into(),
            message: "empty audio input for spectrogram".into(),
        });
    }

    // Compute number of frames
    let n_frames =
        if samples.len() >= n_fft { (samples.len() - n_fft) / hop_length + 1 } else { 1 };

    // Build Hann window
    let hann: Vec<f32> = (0..n_fft)
        .map(|i| {
            let x = std::f32::consts::PI * i as f32 / n_fft as f32;
            x.sin().powi(2)
        })
        .collect();

    // Compute power spectrogram via DFT
    let n_bins = n_fft / 2 + 1;
    let mut power_spec = vec![0.0f32; n_frames * n_bins];

    for frame_idx in 0..n_frames {
        let start = frame_idx * hop_length;
        for bin in 0..n_bins {
            let freq = 2.0 * std::f32::consts::PI * bin as f32 / n_fft as f32;
            let mut real = 0.0f32;
            let mut imag = 0.0f32;
            for (k, &hann_k) in hann.iter().enumerate() {
                let sample_idx = start + k;
                let s = if sample_idx < samples.len() { samples[sample_idx] * hann_k } else { 0.0 };
                real += s * (freq * k as f32).cos();
                imag -= s * (freq * k as f32).sin();
            }
            power_spec[frame_idx * n_bins + bin] = real * real + imag * imag;
        }
    }

    // Build mel filterbank (simplified triangular filters)
    let mel_filters = build_mel_filterbank(16000, n_fft, n_mels);

    // Apply mel filterbank and log
    let mut mel_data = vec![0.0f32; n_frames * n_mels];
    for frame in 0..n_frames {
        for mel in 0..n_mels {
            let mut sum = 0.0f32;
            for bin in 0..n_bins {
                sum += power_spec[frame * n_bins + bin] * mel_filters[mel * n_bins + bin];
            }
            mel_data[frame * n_mels + mel] = sum.max(1e-10).ln();
        }
    }

    Ok(MelSpectrogram { data: mel_data, n_frames, n_mels })
}

/// Convert frequency in Hz to mel scale.
fn hz_to_mel(hz: f32) -> f32 {
    2595.0 * (1.0 + hz / 700.0).log10()
}

/// Convert mel scale to frequency in Hz.
fn mel_to_hz(mel: f32) -> f32 {
    700.0 * (10.0_f32.powf(mel / 2595.0) - 1.0)
}

/// Build a triangular mel filterbank.
fn build_mel_filterbank(sample_rate: u32, n_fft: usize, n_mels: usize) -> Vec<f32> {
    let n_bins = n_fft / 2 + 1;
    let f_max = sample_rate as f32 / 2.0;
    let mel_min = hz_to_mel(0.0);
    let mel_max = hz_to_mel(f_max);

    // Equally spaced mel points
    let mel_points: Vec<f32> = (0..=(n_mels + 1))
        .map(|i| mel_min + (mel_max - mel_min) * i as f32 / (n_mels + 1) as f32)
        .collect();
    let hz_points: Vec<f32> = mel_points.iter().map(|&m| mel_to_hz(m)).collect();
    let bin_points: Vec<f32> =
        hz_points.iter().map(|&hz| hz * n_fft as f32 / sample_rate as f32).collect();

    let mut filters = vec![0.0f32; n_mels * n_bins];
    for mel in 0..n_mels {
        let left = bin_points[mel];
        let center = bin_points[mel + 1];
        let right = bin_points[mel + 2];

        for bin in 0..n_bins {
            let b = bin as f32;
            let weight = if b >= left && b <= center && center > left {
                (b - left) / (center - left)
            } else if b > center && b <= right && right > center {
                (right - b) / (right - center)
            } else {
                0.0
            };
            filters[mel * n_bins + bin] = weight;
        }
    }
    filters
}
