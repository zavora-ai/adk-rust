//! VAD-driven turn-taking demo in HandsFree mode.
//! Demonstrates `VadTurnManager`, `VadConfig`, `MockVad`, and `VoiceActivityEvent`.

use std::sync::Arc;
use std::time::Duration;

use adk_audio::{
    AudioCapture, CaptureConfig, VadConfig, VadMode, VadTurnManager, VoiceActivityEvent,
};
use desktop_audio_example::{MockVad, setup_tracing};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    setup_tracing();
    println!("=== VAD Turn-Taking Demo (HandsFree) ===\n");

    let devices = AudioCapture::list_input_devices()?;
    let device = devices.first().ok_or_else(|| anyhow::anyhow!("no input devices found"))?;
    println!("Using device: {}", device.name());

    let capture_config = CaptureConfig::default();
    capture_config.validate()?;

    let vad_config =
        VadConfig { mode: VadMode::HandsFree, silence_threshold_ms: 500, speech_threshold_ms: 200 };

    let vad: Arc<dyn adk_audio::VadProcessor> = Arc::new(MockVad { threshold: 500 });
    let mut manager = VadTurnManager::new(vad, vad_config)?;

    let mut capture = AudioCapture::new();
    let stream = capture.start_capture(device.id(), &capture_config)?;

    println!("Listening for 10 seconds... speak into your microphone.\n");

    manager.start(stream, |event| match event {
        VoiceActivityEvent::SpeechStarted => println!("🎙️  Speech started"),
        VoiceActivityEvent::SpeechEnded { duration_ms } => {
            println!("🔇 Speech ended (duration: {duration_ms} ms)");
        }
    });

    tokio::time::sleep(Duration::from_secs(10)).await;

    manager.stop();
    capture.stop_capture();
    println!("\nVAD demo complete.");
    Ok(())
}
