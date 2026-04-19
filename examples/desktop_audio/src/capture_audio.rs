//! Capture audio from the default microphone for 3 seconds.
//! Demonstrates `AudioCapture`, `CaptureConfig`, and `AudioStream`.

use std::time::{Duration, Instant};

use adk_audio::{AudioCapture, CaptureConfig};
use desktop_audio_example::{print_frame_info, setup_tracing};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    setup_tracing();
    println!("=== Microphone Capture Demo ===\n");

    let devices = AudioCapture::list_input_devices()?;
    let device = devices.first().ok_or_else(|| anyhow::anyhow!("no input devices found"))?;
    println!("Using device: {} ({})", device.name(), device.id());

    let config = CaptureConfig::default();
    config.validate()?;
    println!(
        "Config: {}Hz, {}ch, {}ms frames\n",
        config.sample_rate, config.channels, config.frame_duration_ms
    );

    let mut capture = AudioCapture::new();
    let mut stream = capture.start_capture(device.id(), &config)?;

    let start = Instant::now();
    let duration = Duration::from_secs(3);
    let mut frame_count = 0u32;

    println!("Capturing for 3 seconds...");
    while start.elapsed() < duration {
        tokio::select! {
            frame = stream.recv() => {
                if let Some(frame) = frame {
                    frame_count += 1;
                    if frame_count % 50 == 0 {
                        print_frame_info(&frame, frame_count as usize);
                    }
                } else {
                    break;
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(1)) => {}
        }
    }

    capture.stop_capture();
    println!("\nCapture complete: {frame_count} frames in {:.1}s", start.elapsed().as_secs_f64());
    Ok(())
}
