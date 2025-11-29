use base64::{engine::general_purpose, Engine as _};
use display_error_chain::DisplayErrorChain;
use gemini_rust::{Gemini, GenerationConfig, Part, SpeakerVoiceConfig, SpeechConfig};
use std::fs::File;
use std::io::Write;
use std::process::ExitCode;
use tracing::{error, info};

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::builder()
                .with_default_directive(tracing::level_filters::LevelFilter::INFO.into())
                .from_env_lossy(),
        )
        .init();

    match do_main().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            let error_chain = DisplayErrorChain::new(e.as_ref());
            tracing::error!(error.debug = ?e, error.chained = %error_chain, "execution failed");
            ExitCode::FAILURE
        }
    }
}

async fn do_main() -> Result<(), Box<dyn std::error::Error>> {
    // Load API key from environment variable
    let api_key =
        std::env::var("GEMINI_API_KEY").expect("Please set GEMINI_API_KEY environment variable");

    // Create client with TTS-enabled model
    let client = Gemini::with_model(api_key, "models/gemini-2.5-flash-preview-tts".to_string())
        .expect("unable to create Gemini API client");

    info!("starting gemini multi-speaker speech generation example");

    // Create multi-speaker configuration
    let speakers = vec![
        SpeakerVoiceConfig::new("Alice", "Puck"),
        SpeakerVoiceConfig::new("Bob", "Charon"),
    ];

    // Create generation config with multi-speaker speech settings
    let generation_config = GenerationConfig {
        response_modalities: Some(vec!["AUDIO".to_string()]),
        speech_config: Some(SpeechConfig::multi_speaker(speakers)),
        ..Default::default()
    };

    // Create a dialogue with speaker tags
    let dialogue = r#"
Alice: Hello there! I'm excited to demonstrate multi-speaker text-to-speech with Gemini.

Bob: That's amazing! I can't believe how natural this sounds. The different voices really bring the conversation to life.

Alice: Exactly! Each speaker has their own distinct voice characteristics, making it easy to follow who's speaking.

Bob: This technology opens up so many possibilities for audio content creation, educational materials, and accessibility features.

Alice: I couldn't agree more. It's remarkable how far AI-generated speech has come!
"#;

    match client
        .generate_content()
        .with_user_message(dialogue)
        .with_generation_config(generation_config)
        .execute()
        .await
    {
        Ok(response) => {
            info!("multi-speaker speech generation completed");

            // Check if we have candidates
            for (i, candidate) in response.candidates.iter().enumerate() {
                if let Some(parts) = &candidate.content.parts {
                    for (j, part) in parts.iter().enumerate() {
                        match part {
                            // Look for inline data with audio MIME type
                            Part::InlineData { inline_data } => {
                                if inline_data.mime_type.starts_with("audio/") {
                                    info!("📄 Found audio data: {}", inline_data.mime_type);

                                    // Decode base64 audio data
                                    match general_purpose::STANDARD.decode(&inline_data.data) {
                                        Ok(audio_bytes) => {
                                            let filename =
                                                format!("multi_speaker_dialogue_{}_{}.pcm", i, j);

                                            // Save audio to file
                                            match File::create(&filename) {
                                                Ok(mut file) => {
                                                    if let Err(e) = file.write_all(&audio_bytes) {
                                                        error!(
                                                            "❌ Error writing audio file: {}",
                                                            e
                                                        );
                                                    } else {
                                                        info!(
                                                            "💾 Multi-speaker audio saved as: {}",
                                                            filename
                                                        );
                                                        info!("🎧 Play with: aplay {} (Linux) or afplay {} (macOS)", filename, filename);
                                                        info!("👥 Features Alice (Puck voice) and Bob (Charon voice)");
                                                    }
                                                }
                                                Err(e) => {
                                                    error!("❌ Error creating audio file: {}", e)
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            error!("❌ Error decoding base64 audio: {}", e)
                                        }
                                    }
                                }
                            }
                            // Display any text content
                            Part::Text {
                                text,
                                thought,
                                thought_signature: _,
                            } => {
                                if thought.unwrap_or(false) {
                                    info!("💭 Model thought: {}", text);
                                } else {
                                    info!("📝 Generated text: {}", text);
                                }
                            }
                            _ => {
                                // Handle other part types if needed
                            }
                        }
                    }
                }
            }

            // Display usage metadata if available
            if let Some(usage_metadata) = &response.usage_metadata {
                info!("📊 Usage Statistics:");
                if let Some(prompt_tokens) = usage_metadata.prompt_token_count {
                    info!("   Prompt tokens: {}", prompt_tokens);
                }
                if let Some(total_tokens) = usage_metadata.total_token_count {
                    info!("   Total tokens: {}", total_tokens);
                }
                if let Some(thoughts_tokens) = usage_metadata.thoughts_token_count {
                    info!("   Thinking tokens: {}", thoughts_tokens);
                }
            }
        }
        Err(e) => {
            error!(error = ?e, "error generating multi-speaker speech");
            error!("💡 Troubleshooting tips:");
            error!("   1. Make sure GEMINI_API_KEY environment variable is set");
            error!("   2. Verify you have access to the Gemini TTS model");
            error!("   3. Check your internet connection");
            error!("   4. Ensure speaker names in dialogue match configured speakers");
            error!("   5. Make sure the model 'gemini-2.5-flash-preview-tts' supports multi-speaker TTS");
        }
    }

    info!("🎉 Example completed!");
    info!("💡 Tips for multi-speaker TTS:");
    info!("   • Use clear speaker names (Alice:, Bob:, etc.)");
    info!("   • Configure voice for each speaker beforehand");
    info!("   • Available voices: Puck, Charon, Kore, Fenrir, Aoede");
    info!("   • Each speaker maintains consistent voice characteristics");

    Ok(())
}
