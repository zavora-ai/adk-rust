//! mistral.rs text-to-speech (TTS) example.
//!
//! This example demonstrates how to use speech generation models with mistral.rs
//! for text-to-speech synthesis, including multi-speaker dialogue.
//!
//! # Prerequisites
//!
//! Add adk-mistralrs to your Cargo.toml via git dependency:
//! ```toml
//! adk-mistralrs = { git = "https://github.com/zavora-ai/adk-rust" }
//! # With Metal (macOS): features = ["metal"]
//! # With CUDA: features = ["cuda"]
//! ```
//!
//! # Running
//!
//! ```bash
//! cargo run --example mistralrs_speech
//! ```
//!
//! # Environment Variables
//!
//! - `SPEECH_MODEL`: HuggingFace model ID (default: "nari-labs/Dia-1.6B")
//! - `OUTPUT_DIR`: Directory to save audio files (default: "./output")

use adk_mistralrs::{MistralRsSpeechModel, ModelSource, SpeechConfig, VoiceConfig};
use std::path::PathBuf;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    println!("ADK mistral.rs Speech (TTS) Example");
    println!("====================================");
    println!();

    // Get model ID from environment or use default
    let model_id =
        std::env::var("SPEECH_MODEL").unwrap_or_else(|_| "nari-labs/Dia-1.6B".to_string());

    // Get output directory
    let output_dir = std::env::var("OUTPUT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("./output"));

    // Create output directory if it doesn't exist
    std::fs::create_dir_all(&output_dir)?;

    println!("Loading speech model: {}", model_id);
    println!("Output directory: {}", output_dir.display());
    println!();
    println!("This may take several minutes on first run (downloading model)...");
    println!();

    // Create speech model configuration
    let config = SpeechConfig::builder()
        .model_source(ModelSource::huggingface(&model_id))
        .voice(VoiceConfig::new().with_speed(1.0))
        .build();

    // Load the speech model
    let model = MistralRsSpeechModel::new(config).await?;

    println!("Speech model loaded successfully!");
    println!();

    // Example 1: Simple text-to-speech
    println!("Example 1: Simple Text-to-Speech");
    println!("---------------------------------");
    let text = "Hello! Welcome to the ADK mistral.rs speech synthesis example. \
                This demonstrates how to convert text to natural sounding speech.";
    println!("Text: {}", text);
    println!("Generating speech...");

    let audio = model.generate_speech(text).await?;
    let output_path = output_dir.join("simple_speech.wav");
    let wav_bytes = audio.to_wav_bytes()?;
    std::fs::write(&output_path, wav_bytes)?;

    println!("✓ Audio saved to: {}", output_path.display());
    println!("  Duration: {:.2} seconds", audio.duration_secs());
    println!("  Sample rate: {} Hz", audio.sample_rate);
    println!("  Channels: {}", audio.channels);
    println!();

    // Example 2: Multi-speaker dialogue
    println!("Example 2: Multi-Speaker Dialogue");
    println!("----------------------------------");
    let dialogue = "[S1] Hello! How are you doing today? \
                    [S2] I'm doing great, thanks for asking! How about you? \
                    [S1] I'm wonderful! Just testing out this amazing speech synthesis.";
    println!("Dialogue:");
    println!("  Speaker 1: Hello! How are you doing today?");
    println!("  Speaker 2: I'm doing great, thanks for asking! How about you?");
    println!("  Speaker 1: I'm wonderful! Just testing out this amazing speech synthesis.");
    println!("Generating dialogue...");

    let dialogue_audio = model.generate_dialogue(dialogue).await?;
    let dialogue_path = output_dir.join("dialogue.wav");
    let dialogue_wav = dialogue_audio.to_wav_bytes()?;
    std::fs::write(&dialogue_path, dialogue_wav)?;

    println!("✓ Dialogue saved to: {}", dialogue_path.display());
    println!("  Duration: {:.2} seconds", dialogue_audio.duration_secs());
    println!();

    // Example 3: Custom voice configuration
    println!("Example 3: Custom Voice Configuration");
    println!("--------------------------------------");
    let custom_text = "This speech is generated with custom voice settings.";
    println!("Text: {}", custom_text);

    let custom_voice = VoiceConfig::new().with_speaker_id(1).with_speed(1.2); // Slightly faster

    let custom_audio = model.generate_speech_with_voice(custom_text, custom_voice).await?;
    let custom_path = output_dir.join("custom_voice.wav");
    let custom_wav = custom_audio.to_wav_bytes()?;
    std::fs::write(&custom_path, custom_wav)?;

    println!("✓ Custom voice audio saved to: {}", custom_path.display());
    println!("  Duration: {:.2} seconds", custom_audio.duration_secs());
    println!();

    // Summary
    println!("Summary");
    println!("-------");
    println!("Generated {} audio files in {}", 3, output_dir.display());
    println!();
    println!("Files created:");
    println!("  - simple_speech.wav: Basic text-to-speech");
    println!("  - dialogue.wav: Multi-speaker conversation");
    println!("  - custom_voice.wav: Custom voice settings");
    println!();
    println!("Play the files with any audio player to hear the results!");
    println!();

    // Interactive mode
    println!("Interactive Mode");
    println!("----------------");
    println!("Enter text to convert to speech (or 'quit' to exit):");
    println!();

    let mut rl = rustyline::DefaultEditor::new()?;

    loop {
        let readline = rl.readline("Text > ");
        match readline {
            Ok(line) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if line == "quit" || line == "exit" || line == "q" {
                    println!("Goodbye!");
                    break;
                }

                println!("Generating speech...");
                match model.generate_speech(line).await {
                    Ok(audio) => {
                        let filename = format!("speech_{}.wav", chrono::Utc::now().timestamp());
                        let path = output_dir.join(&filename);
                        match audio.to_wav_bytes() {
                            Ok(wav) => {
                                std::fs::write(&path, wav)?;
                                println!(
                                    "✓ Saved to: {} ({:.2}s)",
                                    path.display(),
                                    audio.duration_secs()
                                );
                            }
                            Err(e) => println!("Error encoding WAV: {}", e),
                        }
                    }
                    Err(e) => println!("Error generating speech: {}", e),
                }
                println!();
            }
            Err(rustyline::error::ReadlineError::Interrupted) => {
                println!("Interrupted");
                break;
            }
            Err(rustyline::error::ReadlineError::Eof) => {
                println!("EOF");
                break;
            }
            Err(err) => {
                eprintln!("Error: {}", err);
                break;
            }
        }
    }

    Ok(())
}
