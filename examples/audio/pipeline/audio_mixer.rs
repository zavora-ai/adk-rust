//! Audio mixer example — combine multiple audio tracks with volume control.
//!
//! Demonstrates:
//! - Creating a multi-track Mixer
//! - Per-track volume control
//! - Mixing narration + music + sound effects
//! - AudioFrame creation and inspection
//! - WAV codec round-trip
//!
//! # Run
//!
//! ```bash
//! cargo run --example audio_mixer --features audio
//! ```

use adk_audio::{AudioFormat, AudioFrame, Mixer, decode, encode};
use anyhow::Result;

/// Generate a simple sine wave tone as PCM16 samples.
fn generate_tone(
    frequency_hz: f32,
    sample_rate: u32,
    duration_ms: u32,
    amplitude: f32,
) -> AudioFrame {
    let num_samples = (sample_rate as usize * duration_ms as usize) / 1000;
    let mut pcm = Vec::with_capacity(num_samples * 2);
    for i in 0..num_samples {
        let t = i as f32 / sample_rate as f32;
        let sample = (2.0 * std::f32::consts::PI * frequency_hz * t).sin() * amplitude;
        let s16 = (sample * 32767.0).clamp(-32768.0, 32767.0) as i16;
        pcm.extend_from_slice(&s16.to_le_bytes());
    }
    AudioFrame::new(pcm, sample_rate, 1)
}

#[tokio::main]
async fn main() -> Result<()> {
    println!("=== adk-audio: Mixer Example ===\n");

    let sample_rate = 24000;
    let duration_ms = 500;

    // Generate some audio tracks
    println!("1. Generating audio tracks:");
    let narration = generate_tone(220.0, sample_rate, duration_ms, 0.8); // A3 note
    println!(
        "   Narration: {}Hz tone, {}ms, {} samples",
        220,
        narration.duration_ms,
        narration.samples().len()
    );

    let music = generate_tone(440.0, sample_rate, duration_ms, 0.5); // A4 note
    println!(
        "   Music:     {}Hz tone, {}ms, {} samples",
        440,
        music.duration_ms,
        music.samples().len()
    );

    let sfx = generate_tone(880.0, sample_rate, duration_ms, 0.3); // A5 note
    println!(
        "   SFX:       {}Hz tone, {}ms, {} samples\n",
        880,
        sfx.duration_ms,
        sfx.samples().len()
    );

    // Create mixer with three tracks
    println!("2. Mixing tracks:");
    let mut mixer = Mixer::new(sample_rate);
    mixer.add_track("narration", 1.0);
    mixer.add_track("music", 0.3);
    mixer.add_track("sfx", 0.15);

    mixer.push_frame("narration", narration);
    mixer.push_frame("music", music);
    mixer.push_frame("sfx", sfx);

    let mixed = mixer.mix()?;
    println!(
        "   Mixed output: {}ms, {} samples, {} bytes",
        mixed.duration_ms,
        mixed.samples().len(),
        mixed.data.len()
    );

    // Check the mixed samples aren't clipping
    let max_sample = mixed.samples().iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
    let headroom_db = if max_sample > 0 {
        20.0 * (max_sample as f64 / 32767.0).log10()
    } else {
        f64::NEG_INFINITY
    };
    println!("   Peak: {max_sample}/32767 ({headroom_db:.1} dBFS)\n");

    // Volume control demo
    println!("3. Volume control:");
    let tone = generate_tone(440.0, sample_rate, 100, 1.0);
    let peak_original = tone.samples().iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);

    let mut vol_mixer = Mixer::new(sample_rate);
    vol_mixer.add_track("test", 0.5);
    vol_mixer.push_frame("test", tone.clone());
    let half_vol = vol_mixer.mix()?;
    let peak_half = half_vol.samples().iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);

    let mut mute_mixer = Mixer::new(sample_rate);
    mute_mixer.add_track("test", 0.0);
    mute_mixer.push_frame("test", tone);
    let muted = mute_mixer.mix()?;
    let peak_muted = muted.samples().iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);

    println!("   Volume 1.0: peak = {peak_original}");
    println!("   Volume 0.5: peak = {peak_half}");
    println!("   Volume 0.0: peak = {peak_muted}\n");

    // WAV codec round-trip
    println!("4. WAV codec round-trip:");
    let original = generate_tone(440.0, sample_rate, 200, 0.5);
    let wav_bytes = encode(&original, AudioFormat::Wav)?;
    let decoded = decode(&wav_bytes, AudioFormat::Wav)?;
    println!(
        "   Original: {}ms, {}Hz, {} bytes",
        original.duration_ms,
        original.sample_rate,
        original.data.len()
    );
    println!("   WAV size: {} bytes", wav_bytes.len());
    println!(
        "   Decoded:  {}ms, {}Hz, {} bytes",
        decoded.duration_ms,
        decoded.sample_rate,
        decoded.data.len()
    );
    println!("   Round-trip match: {}\n", original == decoded);

    println!("Done!");
    Ok(())
}
