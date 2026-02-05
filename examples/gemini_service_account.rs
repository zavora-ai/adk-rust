//! Example of using GeminiModel with service account authentication

use adk_model::gemini::GeminiModel;
use adk_core::{Content, Llm, LlmRequest, Part};
use std::env;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing (optional but recommended)
    tracing_subscriber::fmt::init();

    // Application code handles reading environment variables
    let project_id = env::var("GOOGLE_PROJECT_ID")
        .expect("GOOGLE_PROJECT_ID environment variable must be set");
    let location = env::var("GOOGLE_LOCATION")
        .unwrap_or_else(|_| "us-central1".to_string());

    println!("Using Vertex AI endpoint");
    println!("Project: {}, Location: {}", project_id, location);

    // Option 1: From service account file
    let service_account_path = env::var("GOOGLE_SERVICE_ACCOUNT_PATH")
        .unwrap_or_else(|_| "service-account.json".to_string());

    if std::path::Path::new(&service_account_path).exists() {
        println!("Using service account file: {}", service_account_path);

        let model = GeminiModel::new_with_service_account_path(
            &service_account_path,
            &project_id,
            &location,
            "gemini-2.5-flash"
        ).await?;

        println!("Testing with service account from file...");
        test_model(model).await?;
    }

    // Option 2: From service account JSON in environment variable
    if let Ok(service_account_json) = env::var("GOOGLE_SERVICE_ACCOUNT_JSON") {
        println!("Using service account from environment variable");

        let model = GeminiModel::new_with_service_account_json(
            service_account_json,
            &project_id,
            &location,
            "gemini-2.5-flash"
        ).await?;

        println!("Testing with service account from env...");
        test_model(model).await?;
    }

    if !std::path::Path::new(&service_account_path).exists()
        && env::var("GOOGLE_SERVICE_ACCOUNT_JSON").is_err() {
        eprintln!("No service account found!");
        eprintln!("Please set either:");
        eprintln!("  - GOOGLE_SERVICE_ACCOUNT_PATH to point to a service account JSON file");
        eprintln!("  - GOOGLE_SERVICE_ACCOUNT_JSON with the JSON content");
    }

    Ok(())
}

async fn test_model(model: GeminiModel) -> Result<(), Box<dyn std::error::Error>> {
    // Create a simple request
    let request = LlmRequest {
        model: model.name().to_string(),
        contents: vec![
            Content {
                role: "user".to_string(),
                parts: vec![
                    Part::Text {
                        text: "What is the capital of France?".to_string(),
                    },
                ],
            },
        ],
        tools: Default::default(),
        config: None,
    };

    // Generate content
    println!("Sending request to Gemini...");
    let mut stream = model.generate_content(request, false).await?;

    // Process response
    while let Some(response) = futures::StreamExt::next(&mut stream).await {
        let response = response?;

        if let Some(content) = response.content {
            for part in content.parts {
                if let Part::Text { text } = part {
                    println!("Response: {}", text);
                }
            }
        }

        if let Some(usage) = response.usage_metadata {
            println!("\nUsage:");
            println!("  Prompt tokens: {}", usage.prompt_token_count);
            println!("  Response tokens: {}", usage.candidates_token_count);
            println!("  Total tokens: {}", usage.total_token_count);
        }
    }

    Ok(())
}