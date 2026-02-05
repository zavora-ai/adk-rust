//! Example of configuring retry behavior for GeminiModel

use adk_model::gemini::{GeminiModel, RetryConfig};
use adk_core::{Content, Llm, LlmRequest, Part};
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    // Example 1: Default retry configuration (3 retries, 1s initial delay, 2x backoff)
    let _model1 = GeminiModel::new("your-api-key", "models/gemini-2.5-flash").await?;
    println!("Model 1: Using default retry config");

    // Example 2: Using the builder pattern with custom retry configuration
    let model2 = GeminiModel::builder("models/gemini-2.5-flash")
        .api_key("your-api-key")
        .retry_config(
            RetryConfig::new()
                .with_max_retries(5)
                .with_initial_delay(Duration::from_millis(500))
                .with_max_delay(Duration::from_secs(30))
                .with_backoff_multiplier(1.5)
        )
        .build()
        .await?;
    println!("Model 2: 5 retries, 500ms start, 1.5x backoff, 30s max");

    // Example 3: Configure retries using closure in builder
    let _model3 = GeminiModel::builder("models/gemini-2.5-flash")
        .api_key("your-api-key")
        .configure_retries(|config| {
            config
                .with_max_retries(2)
                .with_initial_delay(Duration::from_secs(2))
        })
        .build()
        .await?;
    println!("Model 3: 2 retries, 2s initial delay");

    // Example 4: Disable retries entirely
    let _model4 = GeminiModel::builder("models/gemini-2.5-flash")
        .api_key("your-api-key")
        .retry_config(RetryConfig::disabled())
        .build()
        .await?;
    println!("Model 4: Retries disabled");

    // Example 5: Service account with custom retry config using builder
    use std::env;
    if let (Ok(project_id), Ok(location)) = (
        env::var("GOOGLE_PROJECT_ID"),
        env::var("GOOGLE_LOCATION")
    ) {
        if let Ok(service_account_path) = env::var("GOOGLE_SERVICE_ACCOUNT_PATH") {
            let _model5 = GeminiModel::builder("gemini-2.5-flash")
                .service_account_path(service_account_path)?
                .project_id(project_id)
                .location(location)
                .retry_config(
                    RetryConfig::new()
                        .with_max_retries(10)  // More retries for production
                        .with_initial_delay(Duration::from_millis(100))
                        .with_backoff_multiplier(3.0)  // Aggressive backoff
                )
                .build()
                .await?;
            println!("Model 5: Service account with aggressive retry strategy");
        }
    }

    // Test one of the models
    test_model(&model2).await?;

    Ok(())
}

async fn test_model(model: &GeminiModel) -> Result<(), Box<dyn std::error::Error>> {
    let request = LlmRequest {
        model: model.name().to_string(),
        contents: vec![
            Content {
                role: "user".to_string(),
                parts: vec![
                    Part::Text {
                        text: "Hello! How are you?".to_string(),
                    },
                ],
            },
        ],
        tools: Default::default(),
        config: None,
    };

    println!("\nSending request (retries will be automatic on rate limits)...");
    let mut stream = model.generate_content(request, false).await?;

    while let Some(response) = futures::StreamExt::next(&mut stream).await {
        let response = response?;

        if let Some(content) = response.content {
            for part in content.parts {
                if let Part::Text { text } = part {
                    println!("Response: {}", text);
                }
            }
        }
    }

    Ok(())
}