//! Configuration validation demo.
//! Demonstrates `CaptureConfig` and `VadConfig` validation with error handling.

use adk_audio::{CaptureConfig, VadConfig, VadMode};
use desktop_audio_example::setup_tracing;

fn main() -> anyhow::Result<()> {
    setup_tracing();
    println!("=== Configuration Validation Demo ===\n");

    // CaptureConfig validation
    println!("--- CaptureConfig ---\n");

    let bad_sample_rate = CaptureConfig { sample_rate: 0, ..Default::default() };
    match bad_sample_rate.validate() {
        Err(e) => println!("✗ Zero sample rate: {e}"),
        Ok(()) => println!("✓ Zero sample rate: (unexpected pass)"),
    }

    let bad_channels = CaptureConfig { channels: 0, ..Default::default() };
    match bad_channels.validate() {
        Err(e) => println!("✗ Zero channels:    {e}"),
        Ok(()) => println!("✓ Zero channels: (unexpected pass)"),
    }

    let bad_duration = CaptureConfig { frame_duration_ms: 0, ..Default::default() };
    match bad_duration.validate() {
        Err(e) => println!("✗ Zero duration:    {e}"),
        Ok(()) => println!("✓ Zero duration: (unexpected pass)"),
    }

    let good_config = CaptureConfig::default();
    match good_config.validate() {
        Ok(()) => println!(
            "✓ Valid config:     {}Hz, {}ch, {}ms",
            good_config.sample_rate, good_config.channels, good_config.frame_duration_ms
        ),
        Err(e) => println!("✗ Valid config: {e}"),
    }

    // VadConfig validation
    println!("\n--- VadConfig ---\n");

    let bad_silence =
        VadConfig { mode: VadMode::HandsFree, silence_threshold_ms: 0, speech_threshold_ms: 200 };
    match bad_silence.validate() {
        Err(e) => println!("✗ Zero silence threshold: {e}"),
        Ok(()) => println!("✓ Zero silence threshold: (unexpected pass)"),
    }

    let bad_speech =
        VadConfig { mode: VadMode::HandsFree, silence_threshold_ms: 500, speech_threshold_ms: 0 };
    match bad_speech.validate() {
        Err(e) => println!("✗ Zero speech threshold:  {e}"),
        Ok(()) => println!("✓ Zero speech threshold: (unexpected pass)"),
    }

    let good_vad =
        VadConfig { mode: VadMode::HandsFree, silence_threshold_ms: 500, speech_threshold_ms: 200 };
    match good_vad.validate() {
        Ok(()) => println!(
            "✓ Valid VAD config:       {:?}, {}ms silence, {}ms speech",
            good_vad.mode, good_vad.silence_threshold_ms, good_vad.speech_threshold_ms
        ),
        Err(e) => println!("✗ Valid VAD config: {e}"),
    }

    println!("\nValidation demo complete.");
    Ok(())
}
