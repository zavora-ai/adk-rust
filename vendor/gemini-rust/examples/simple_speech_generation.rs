use base64::{engine::general_purpose, Engine as _};
use display_error_chain::DisplayErrorChain;
use gemini_rust::{Gemini, GenerationConfig, Part, PrebuiltVoiceConfig, SpeechConfig, VoiceConfig};
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

    info!("starting gemini speech generation example");
    info!("generating audio from text");

    // Create generation config with speech settings
    let generation_config = GenerationConfig {
        response_modalities: Some(vec!["AUDIO".to_string()]),
        speech_config: Some(SpeechConfig {
            voice_config: Some(VoiceConfig {
                prebuilt_voice_config: Some(PrebuiltVoiceConfig {
                    voice_name: "Puck".to_string(),
                }),
            }),
            multi_speaker_voice_config: None,
        }),
        ..Default::default()
    };

    match client
        .generate_content()
        .with_user_message("Hello! This is a demonstration of text-to-speech using Google's Gemini API. The voice you're hearing is generated entirely by AI.")
        .with_generation_config(generation_config)
        .execute()
        .await {
        Ok(response) => {
            info!("speech generation completed");

            // Check if we have candidates
            for (i, candidate) in response.candidates.iter().enumerate() {
                if let Some(parts) = &candidate.content.parts {
                    for (j, part) in parts.iter().enumerate() {
                        match part {
                            // Look for inline data with audio MIME type
                            Part::InlineData { inline_data } => {
                                if inline_data.mime_type.starts_with("audio/") {
                                    info!(mime_type = inline_data.mime_type, "found audio data");

                                    // Decode base64 audio data using the new API
                                    match general_purpose::STANDARD.decode(&inline_data.data) {
                                        Ok(audio_bytes) => {
                                            let filename = format!("speech_output_{}_{}.pcm", i, j);

                                            // Save audio to file
                                            match File::create(&filename) {
                                                Ok(mut file) => {
                                                    if let Err(e) = file.write_all(&audio_bytes) {
                                                        error!(error = %e, "error writing audio file");
                                                    } else {
                                                        info!(filename = filename, "audio saved");
                                                        info!(filename = filename, "you can play with: aplay {} (Linux) or afplay {} (macOS)", filename, filename);
                                                    }
                                                },
                                                Err(e) => error!(error = %e, "error creating audio file"),
                                            }
                                        },
                                        Err(e) => error!(error = %e, "error decoding base64 audio"),
                                    }
                                }
                            },
                            // Display any text content
                            Part::Text { text, thought, thought_signature: _ } => {
                                if thought.unwrap_or(false) {
                                    info!(thought = text, "thought content");
                                } else {
                                    info!(text_content = text, "text content");
                                }
                            },
                            _ => {
                                // Handle other part types if needed
                            }
                        }
                    }
                }
            }

            // Display usage metadata if available
            if let Some(usage_metadata) = &response.usage_metadata {
                info!("usage statistics");
                if let Some(prompt_tokens) = usage_metadata.prompt_token_count {
                    info!(prompt_tokens = prompt_tokens, "prompt tokens");
                }
                if let Some(total_tokens) = usage_metadata.total_token_count {
                    info!(total_tokens = total_tokens, "total tokens");
                }
            }
        },
        Err(e) => {
            error!(error = %e, "error generating speech");
            info!("troubleshooting tips");
            info!("1. make sure GEMINI_API_KEY environment variable is set");
            info!("2. verify you have access to the Gemini TTS model");
            info!("3. check your internet connection");
            info!("4. ensure the model 'gemini-2.5-flash-preview-tts' is available");
        }
    }

    Ok(())
}
