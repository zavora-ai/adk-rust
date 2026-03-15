//! Basic OpenAI provider smoke test — text only, no vision.

use adk_core::{Content, GenerateContentConfig, Llm, LlmRequest, Part};
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    let api_key = std::env::var("OPENAI_API_KEY")?;
    println!("API key length: {}", api_key.len());

    let model_name = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string());
    let config = adk_model::openai::OpenAIConfig::new(&api_key, &model_name);
    let model: Arc<dyn Llm> = Arc::new(adk_model::openai::OpenAIClient::new(config)?);

    let request = LlmRequest {
        model: model.name().to_string(),
        contents: vec![Content {
            role: "user".to_string(),
            parts: vec![Part::Text { text: "Say hello in one sentence.".to_string() }],
        }],
        config: Some(GenerateContentConfig { max_output_tokens: Some(256), ..Default::default() }),
        tools: HashMap::new(),
    };

    println!("Sending text-only request to {}...", model.name());
    let mut stream = model.generate_content(request, false).await?;
    let mut full_text = String::new();

    while let Some(result) = stream.next().await {
        match result {
            Ok(response) => {
                if let Some(content) = &response.content {
                    for part in &content.parts {
                        if let Some(text) = part.text() {
                            full_text.push_str(text);
                        }
                    }
                }
            }
            Err(e) => {
                println!("ERROR: {e}");
                return Err(e.into());
            }
        }
    }

    println!("Response: {full_text}");
    println!("✓ PASS");
    Ok(())
}
