/// Example demonstrating text responses with thoughtSignature support
///
/// This example shows how to handle text responses that include thought signatures,
/// as seen in the Gemini 2.5 Flash API response format.
use display_error_chain::DisplayErrorChain;
use gemini_rust::{Content, GenerationResponse, Part};
use serde_json::json;
use std::process::ExitCode;
use tracing::info;

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::builder()
                .with_default_directive(tracing::level_filters::LevelFilter::INFO.into())
                .from_env_lossy(),
        )
        .init();

    match do_main() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            let error_chain = DisplayErrorChain::new(e.as_ref());
            tracing::error!(error.debug = ?e, error.chained = %error_chain, "execution failed");
            ExitCode::FAILURE
        }
    }
}

fn do_main() -> Result<(), Box<dyn std::error::Error>> {
    info!("starting text with thoughtSignature example");

    // Simulate an API response similar to the one you provided
    let api_response = json!({
        "candidates": [
            {
                "content": {
                    "parts": [
                        {
                            "text": "**Okay, here's what I'm thinking:**\n\nThe user wants me to show them the functions available. I need to figure out what functions are accessible to me in this environment.",
                            "thought": true
                        },
                        {
                            "text": "The following functions are available in the environment: `chat.get_message_count()`",
                            "thoughtSignature": "Cs4BA.../Yw="
                        }
                    ],
                    "role": "model"
                },
                "finishReason": "STOP",
                "index": 0
            }
        ],
        "usageMetadata": {
            "promptTokenCount": 36,
            "candidatesTokenCount": 18,
            "totalTokenCount": 96,
            "promptTokensDetails": [
                {
                    "modality": "TEXT",
                    "tokenCount": 36
                }
            ],
            "thoughtsTokenCount": 42
        },
        "modelVersion": "gemini-2.5-flash",
        "responseId": "gIC..."
    });

    // Parse the response
    let response: GenerationResponse = serde_json::from_value(api_response)?;

    info!("📋 Parsed API Response:");
    info!(
        "Model Version: {}",
        response
            .model_version
            .as_ref()
            .unwrap_or(&"Unknown".to_string())
    );

    // Display usage metadata
    if let Some(usage) = &response.usage_metadata {
        info!("📊 Token Usage:");
        if let Some(prompt_token_count) = usage.prompt_token_count {
            info!("  Prompt tokens: {}", prompt_token_count);
        }
        info!(
            "  Response tokens: {}",
            usage.candidates_token_count.unwrap_or(0)
        );
        if let Some(total_token_count) = usage.total_token_count {
            info!("  Total tokens: {}", total_token_count);
        }
        if let Some(thinking_tokens) = usage.thoughts_token_count {
            info!("  Thinking tokens: {}", thinking_tokens);
        }
    }

    // Extract text parts with thought signatures using the new method
    info!("💭 Text Parts with Thought Analysis:");
    let text_with_thoughts = response.text_with_thoughts();

    for (i, (text, is_thought, thought_signature)) in text_with_thoughts.iter().enumerate() {
        info!("--- Part {} ---", i + 1);
        info!("Is thought: {}", is_thought);
        info!("Has thought signature: {}", thought_signature.is_some());

        if let Some(signature) = thought_signature {
            info!("Thought signature: {}", signature);
            info!("Signature length: {} characters", signature.len());
        }

        info!("Text content: {}", text);
    }

    // Demonstrate creating content with thought signatures
    info!("🔧 Creating Content with Thought Signatures:");

    let custom_content = Content::text_with_thought_signature(
        "This is a custom response with a thought signature",
        "custom_signature_abc123",
    );

    let custom_thought = Content::thought_with_signature(
        "This represents the model's thinking process",
        "thinking_signature_def456",
    );

    info!("Custom content JSON:");
    info!("{}", serde_json::to_string_pretty(&custom_content)?);

    info!("Custom thought JSON:");
    info!("{}", serde_json::to_string_pretty(&custom_thought)?);

    // Show how this would be used in multi-turn conversation context
    info!("🔄 Multi-turn Conversation Context:");
    info!("In a multi-turn conversation, you would include these parts");
    info!("with their thought signatures to maintain context:");

    // Extract the original parts for context preservation
    if let Some(candidate) = response.candidates.first() {
        if let Some(parts) = &candidate.content.parts {
            for (i, part) in parts.iter().enumerate() {
                if let Part::Text {
                    text: _,
                    thought,
                    thought_signature,
                } = part
                {
                    info!(
                        part_number = i + 1,
                        text_type = if *thought == Some(true) {
                            "Thought"
                        } else {
                            "Regular"
                        },
                        has_signature = thought_signature.is_some(),
                        "part analysis"
                    );

                    if let Some(sig) = thought_signature {
                        info!(
                            signature_preview = &sig[..10.min(sig.len())],
                            "preserve signature"
                        );
                    }
                }
            }
        }
    }

    info!("example completed successfully");
    Ok(())
}
