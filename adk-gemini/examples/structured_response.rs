use adk_gemini::{Gemini, GenerationResponse};
use display_error_chain::DisplayErrorChain;
use serde_json::json;
use std::env;
use std::process::ExitCode;
use tracing::info;

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

    // Create client
    let client = Gemini::new(api_key).expect("unable to create Gemini API client");

    // Using response_schema for structured output
    info!("starting structured response example");

    // Define a JSON schema for the response
    let schema = json!({
        "type": "object",
        "properties": {
            "name": {
                "type": "string",
                "description": "Name of the programming language"
            },
            "year_created": {
                "type": "integer",
                "description": "Year the programming language was created"
            },
            "creator": {
                "type": "string",
                "description": "Person or organization who created the language"
            },
            "key_features": {
                "type": "array",
                "items": {
                    "type": "string"
                },
                "description": "Key features of the programming language"
            },
            "popularity_score": {
                "type": "integer",
                "description": "Subjective popularity score from 1-10"
            }
        },
        "required": ["name", "year_created", "creator", "key_features", "popularity_score"]
    });

    let response: GenerationResponse = client
        .generate_content()
        .with_system_prompt("You provide information about programming languages in JSON format.")
        .with_user_message("Tell me about the Rust programming language.")
        .with_response_mime_type("application/json")
        .with_response_schema(schema)
        .execute()
        .await?;

    info!(response = response.text(), "structured json response received");

    // Parse the JSON response
    let json_response: serde_json::Value = serde_json::from_str(&response.text())?;

    info!(
        language = json_response["name"].as_str().unwrap_or("unknown"),
        year = json_response["year_created"].as_i64().unwrap_or(0),
        creator = json_response["creator"].as_str().unwrap_or("unknown"),
        popularity = json_response["popularity_score"].as_i64().unwrap_or(0),
        "parsed structured response fields"
    );

    if let Some(features) = json_response["key_features"].as_array() {
        for (i, feature) in features.iter().enumerate() {
            info!(index = i + 1, feature = feature.as_str().unwrap_or("unknown"), "key feature");
        }
    }

    Ok(())
}
