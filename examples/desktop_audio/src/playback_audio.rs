//! Play a silence frame through the default speaker.
//! Demonstrates `AudioPlayback` and `AudioFrame::silence()`.

use std::time::Duration;

use adk_audio::{AudioFrame, AudioPlayback};
use desktop_audio_example::setup_tracing;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    setup_tracing();
    println!("=== Speaker Playback Demo ===\n");

    let devices = AudioPlayback::list_output_devices()?;
    let device = devices.first().ok_or_else(|| anyhow::anyhow!("no output devices found"))?;
    println!("Using device: {} ({})", device.name(), device.id());

    let silence = AudioFrame::silence(16000, 1, 1000);
    println!(
        "Playing 1 second of silence ({}Hz, {}ch, {} bytes)...",
        silence.sample_rate,
        silence.channels,
        silence.data.len()
    );

    let mut playback = AudioPlayback::new();
    playback.play(device.id(), &silence).await?;

    // Wait for playback to complete
    tokio::time::sleep(Duration::from_millis(1200)).await;

    playback.stop();
    println!("Playback complete.");
    Ok(())
}
