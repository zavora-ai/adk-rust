//! List all available audio input and output devices.
//! Demonstrates `AudioCapture::list_input_devices()` and `AudioPlayback::list_output_devices()`.

use adk_audio::{AudioCapture, AudioPlayback};
use desktop_audio_example::{print_device_list, setup_tracing};

fn main() -> anyhow::Result<()> {
    setup_tracing();
    println!("=== Desktop Audio Device Enumeration ===\n");

    let input_devices = AudioCapture::list_input_devices()?;
    print_device_list("Input Devices (Microphones)", &input_devices);

    let output_devices = AudioPlayback::list_output_devices()?;
    print_device_list("Output Devices (Speakers)", &output_devices);

    println!("\nTotal: {} input, {} output", input_devices.len(), output_devices.len());
    Ok(())
}
