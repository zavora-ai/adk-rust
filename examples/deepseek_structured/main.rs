//! DeepSeek Structured Output Example
//!
//! This example demonstrates using DeepSeek with structured JSON output.
//! The agent responds with well-defined JSON structures.
//!
//! Set DEEPSEEK_API_KEY environment variable before running:
//! ```bash
//! cargo run --example deepseek_structured --features deepseek
//! ```

use adk_agent::LlmAgentBuilder;
use adk_model::deepseek::{DeepSeekClient, DeepSeekConfig};
use serde_json::json;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Load .env file if present
    dotenvy::dotenv().ok();

    let api_key = std::env::var("DEEPSEEK_API_KEY").expect("DEEPSEEK_API_KEY must be set");

    // Create DeepSeek client
    let model = Arc::new(DeepSeekClient::new(DeepSeekConfig::chat(api_key))?);

    // Create an agent with a defined output schema
    // This encourages the model to respond with JSON matching this structure
    let agent = LlmAgentBuilder::new("product_analyzer")
        .description("Analyzes products and outputs structured JSON data")
        .model(model)
        .instruction(
            "You are a product analyst. When given a product name or description, \
             analyze it and respond with ONLY valid JSON in the following format:\n\n\
             {\n\
               \"product_name\": \"string\",\n\
               \"category\": \"string (electronics/clothing/food/home/other)\",\n\
               \"price_range\": \"string (budget/mid-range/premium/luxury)\",\n\
               \"target_audience\": [\"string array of demographics\"],\n\
               \"key_features\": [\"string array of 3-5 features\"],\n\
               \"competitors\": [\"string array of 2-3 competing products\"],\n\
               \"rating_prediction\": number (1-5),\n\
               \"summary\": \"string (2-3 sentence summary)\"\n\
             }\n\n\
             Always respond with valid JSON only, no additional text.",
        )
        .output_schema(json!({
            "type": "object",
            "properties": {
                "product_name": {
                    "type": "string",
                    "description": "Name of the product"
                },
                "category": {
                    "type": "string",
                    "enum": ["electronics", "clothing", "food", "home", "other"],
                    "description": "Product category"
                },
                "price_range": {
                    "type": "string",
                    "enum": ["budget", "mid-range", "premium", "luxury"],
                    "description": "Price tier"
                },
                "target_audience": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Target demographics"
                },
                "key_features": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "3-5 key product features"
                },
                "competitors": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "2-3 competing products"
                },
                "rating_prediction": {
                    "type": "number",
                    "minimum": 1,
                    "maximum": 5,
                    "description": "Predicted rating out of 5"
                },
                "summary": {
                    "type": "string",
                    "description": "Brief product summary"
                }
            },
            "required": ["product_name", "category", "price_range", "key_features", "summary"]
        }))
        .build()?;

    println!("=== DeepSeek Structured Output Demo ===\n");
    println!("This agent analyzes products and returns structured JSON.\n");
    println!("Try products like:");
    println!("  - 'iPhone 15 Pro'");
    println!("  - 'Nike Air Max sneakers'");
    println!("  - 'Instant Pot pressure cooker'\n");

    // Run interactive console
    adk_cli::console::run_console(
        Arc::new(agent),
        "deepseek_structured".to_string(),
        "user_1".to_string(),
    )
    .await?;

    Ok(())
}
