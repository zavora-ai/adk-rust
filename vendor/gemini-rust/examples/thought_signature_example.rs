use display_error_chain::DisplayErrorChain;
/// Comprehensive example demonstrating thoughtSignature support in Gemini 2.5 Pro
///
/// This example shows:
/// 1. How to enable thinking and function calling to receive thought signatures
/// 2. How to extract thought signatures from function call responses
/// 3. How to maintain thought context across multiple turns in a conversation
///
/// Key points about thought signatures:
/// - Only available with Gemini 2.5 series models
/// - Requires both thinking and function calling to be enabled
/// - Must include the entire response with thought signatures in subsequent turns
/// - Don't concatenate or merge parts with signatures
///
/// Thought signatures are encrypted representations of the model's internal
/// thought process that help maintain context across conversation turns.
use gemini_rust::{
    FunctionCallingMode, FunctionDeclaration, FunctionResponse, Gemini, ThinkingConfig, Tool,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::env;
use std::process::ExitCode;
use tracing::info;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
struct WeatherRequest {
    /// City and region, e.g., Kaohsiung Zuoying District
    location: String,
}

impl Default for WeatherRequest {
    fn default() -> Self {
        WeatherRequest {
            location: "".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct WeatherResponse {
    temperature: String,
    condition: String,
    humidity: String,
    wind: String,
    location: String,
}

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
    // Get API key from environment variable
    let api_key = env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY environment variable not set");

    // Create client using Gemini 2.5 Pro which supports thoughtSignature
    let client = Gemini::pro(api_key).expect("unable to create Gemini API client");

    info!("starting gemini 2.5 pro thoughtSignature example");

    // Define the weather function tool
    let weather_function = FunctionDeclaration::new(
        "get_current_weather",
        "Get current weather information for a specified location",
        None,
    )
    .with_parameters::<WeatherRequest>()
    .with_response::<WeatherResponse>();

    let weather_tool = Tool::new(weather_function);

    // Configure thinking to enable thoughtSignature
    let thinking_config = ThinkingConfig::new()
        .with_dynamic_thinking()
        .with_thoughts_included(true);

    // First request: Ask about weather (expecting function call with thoughtSignature)
    info!("step 1: asking about weather (expecting function call)");

    let response = client
        .generate_content()
        .with_system_instruction("Please respond in Traditional Chinese")
        .with_user_message("What's the weather like in Kaohsiung Zuoying District right now?")
        .with_tool(weather_tool)
        .with_function_calling_mode(FunctionCallingMode::Auto)
        .with_thinking_config(thinking_config)
        .execute()
        .await?;

    // Check for function calls with thought signatures
    let function_calls_with_thoughts = response.function_calls_with_thoughts();

    if !function_calls_with_thoughts.is_empty() {
        info!("function calls received");
        for (function_call, thought_signature) in function_calls_with_thoughts {
            info!(
                function_name = function_call.name,
                args = ?function_call.args,
                "function call details"
            );

            if let Some(signature) = thought_signature {
                info!(
                    signature = signature,
                    signature_length = signature.len(),
                    "thought signature details"
                );
            } else {
                info!("no thought signature provided");
            }

            // Parse the function call arguments
            let weather_request: WeatherRequest =
                serde_json::from_value(function_call.args.clone())?;

            // Mock function response
            let weather_data = json!({
                "temperature": "25°C",
                "condition": "sunny",
                "humidity": "60%",
                "wind": "light breeze",
                "location": weather_request.location
            });

            info!(weather_data = ?weather_data, "mock weather response");

            // Continue the conversation with function response
            info!("step 2: providing function response");
            let function_response = FunctionResponse::new(&function_call.name, weather_data);

            let final_response = client
                .generate_content()
                .with_system_instruction("Please respond in Traditional Chinese")
                .with_user_message(
                    "What's the weather like in Kaohsiung Zuoying District right now?",
                )
                .with_function_response(
                    &function_call.name,
                    function_response.response.unwrap_or_default(),
                )?
                .execute()
                .await?;

            info!(final_response = final_response.text(), "final response");

            // Display usage metadata
            if let Some(usage) = &final_response.usage_metadata {
                info!("token usage");
                if let Some(prompt_tokens) = usage.prompt_token_count {
                    info!(prompt_tokens = prompt_tokens, "prompt tokens");
                }
                if let Some(response_tokens) = usage.candidates_token_count {
                    info!(response_tokens = response_tokens, "response tokens");
                }
                if let Some(thinking_tokens) = usage.thoughts_token_count {
                    info!(thinking_tokens = thinking_tokens, "thinking tokens");
                }
                if let Some(total_tokens) = usage.total_token_count {
                    info!(total_tokens = total_tokens, "total tokens");
                }
            }

            // --- Step 3: Multi-turn conversation with thought context ---
            info!("step 3: multi-turn conversation maintaining thought context");
            info!("IMPORTANT: To maintain thought context, we must include the complete previous response with thought signatures in the next turn");

            // Create a multi-turn conversation that includes the previous context
            // We need to include ALL parts from the previous responses to maintain thought context
            let mut conversation_builder = client.generate_content();

            // Add system instruction
            conversation_builder = conversation_builder
                .with_system_instruction("Please respond in Traditional Chinese");

            // Add the original user message
            conversation_builder = conversation_builder.with_user_message(
                "What's the weather like in Kaohsiung Zuoying District right now?",
            );

            // IMPORTANT: Add the model's response with the function call INCLUDING the thought signature
            // This maintains the thought context for the next turn
            // DO NOT concatenate parts or merge signatures - include the complete original part
            let model_content = gemini_rust::Content {
                parts: Some(vec![gemini_rust::Part::FunctionCall {
                    function_call: function_call.clone(),
                    thought_signature: thought_signature.cloned(), // This is crucial for context
                }]),
                role: Some(gemini_rust::Role::Model),
            };
            conversation_builder.contents.push(model_content);

            // Add the function response
            conversation_builder = conversation_builder.with_function_response(
                &function_call.name,
                json!({
                    "temperature": "25°C",
                    "condition": "sunny",
                    "humidity": "60%",
                    "wind": "light breeze",
                    "location": weather_request.location
                }),
            )?;

            // Add the model's text response (complete the conversation history)
            let model_text_content = gemini_rust::Content {
                parts: Some(vec![gemini_rust::Part::Text {
                    text: final_response.text(),
                    thought: None,
                    thought_signature: None,
                }]),
                role: Some(gemini_rust::Role::Model),
            };
            conversation_builder.contents.push(model_text_content);

            // Now ask a follow-up question that can benefit from the thought context
            // The model will have access to its previous reasoning through the thought signature
            conversation_builder = conversation_builder.with_user_message("Is this weather suitable for outdoor sports? Please recommend some appropriate activities.");

            // Add the weather tool again for potential follow-up function calls
            let weather_tool_followup = Tool::new(
                FunctionDeclaration::new(
                    "get_current_weather",
                    "Get current weather information for a specified location",
                    None,
                )
                .with_parameters::<WeatherRequest>()
                .with_response::<WeatherResponse>(),
            );

            conversation_builder = conversation_builder
                .with_tool(weather_tool_followup)
                .with_function_calling_mode(FunctionCallingMode::Auto)
                .with_thinking_config(
                    ThinkingConfig::new()
                        .with_dynamic_thinking()
                        .with_thoughts_included(true),
                );

            let followup_response = conversation_builder.execute().await?;

            info!("follow-up question: Is this weather suitable for outdoor sports? Please recommend some appropriate activities.");
            info!(
                followup_response = followup_response.text(),
                "follow-up response"
            );

            // Check if there are any new function calls with thought signatures in the follow-up
            let followup_function_calls = followup_response.function_calls_with_thoughts();
            if !followup_function_calls.is_empty() {
                info!("follow-up function calls detected");
                for (fc, ts) in followup_function_calls {
                    info!(
                        function_name = fc.name,
                        args = ?fc.args,
                        "follow-up function call"
                    );
                    if let Some(sig) = ts {
                        info!(signature_length = sig.len(), "new thought signature");
                    }
                }
            }

            // Display thinking process for follow-up
            let followup_thoughts = followup_response.thoughts();
            if !followup_thoughts.is_empty() {
                info!("follow-up thinking summaries");
                for (i, thought) in followup_thoughts.iter().enumerate() {
                    info!(
                        thought_number = i + 1,
                        thought = thought,
                        "follow-up thought"
                    );
                }
            }

            // Display follow-up usage metadata
            if let Some(usage) = &followup_response.usage_metadata {
                info!("follow-up token usage");
                if let Some(prompt_tokens) = usage.prompt_token_count {
                    info!(prompt_tokens = prompt_tokens, "prompt tokens");
                }
                if let Some(response_tokens) = usage.candidates_token_count {
                    info!(response_tokens = response_tokens, "response tokens");
                }
                if let Some(thinking_tokens) = usage.thoughts_token_count {
                    info!(thinking_tokens = thinking_tokens, "thinking tokens");
                }
                if let Some(total_tokens) = usage.total_token_count {
                    info!(total_tokens = total_tokens, "total tokens");
                }
            }

            info!("multi-turn conversation completed");
            info!("key takeaways:");
            info!("1. thought signatures help maintain context across conversation turns");
            info!("2. include the complete response (with signatures) in subsequent requests");
            info!("3. don't modify or concatenate parts that contain thought signatures");
            info!("4. thought signatures are only available with thinking + function calling");
            info!(
                "5. the model can build upon its previous reasoning when signatures are preserved"
            );
        }
    } else {
        info!("no function calls in response");
        info!(response_text = response.text(), "response text");
    }

    // Display any thoughts from the initial response
    let thoughts = response.thoughts();
    if !thoughts.is_empty() {
        info!("initial thinking summaries");
        for (i, thought) in thoughts.iter().enumerate() {
            info!(thought_number = i + 1, thought = thought, "initial thought");
        }
    }

    Ok(())
}
