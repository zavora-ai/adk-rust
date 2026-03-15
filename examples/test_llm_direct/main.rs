//! Direct test of OpenAI LLM with tools to debug the issue.

use adk_core::{Content, Llm, LlmRequest, Part};
use adk_model::openai::{OpenAIClient, OpenAIConfig};
use futures::StreamExt;
use std::collections::HashMap;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY must be set");
    let model = OpenAIClient::new(OpenAIConfig::new(api_key, "gpt-5-mini"))?;

    // Define a simple tool
    let mut tools = HashMap::new();
    tools.insert(
        "calculator".to_string(),
        serde_json::json!({
            "description": "Performs basic arithmetic operations",
            "parameters": {
                "type": "object",
                "properties": {
                    "operation": {
                        "type": "string",
                        "enum": ["add", "subtract", "multiply", "divide"]
                    },
                    "a": { "type": "number" },
                    "b": { "type": "number" }
                },
                "required": ["operation", "a", "b"]
            }
        }),
    );

    // Create request
    let request = LlmRequest {
        model: "gpt-5-mini".to_string(),
        contents: vec![Content {
            role: "user".to_string(),
            parts: vec![Part::Text { text: "What is 25 * 17?".to_string() }],
        }],
        tools,
        config: None,
    };

    println!("Sending request with tools to OpenAI...\n");

    let mut stream = model.generate_content(request, true).await?;
    let mut response_count = 0;

    while let Some(result) = stream.next().await {
        response_count += 1;
        match result {
            Ok(response) => {
                println!("--- Response {} ---", response_count);
                println!("  partial: {}", response.partial);
                println!("  turn_complete: {}", response.turn_complete);
                println!("  finish_reason: {:?}", response.finish_reason);

                if let Some(content) = &response.content {
                    println!("  role: {}", content.role);
                    println!("  parts ({}):", content.parts.len());
                    for (i, part) in content.parts.iter().enumerate() {
                        match part {
                            Part::Text { text } => {
                                println!("    [{}] Text: {}", i, text);
                            }
                            Part::FunctionCall { name, args, id, .. } => {
                                println!("    [{}] FunctionCall:", i);
                                println!("        name: {}", name);
                                println!("        args: {}", args);
                                println!("        id: {:?}", id);
                            }
                            Part::FunctionResponse { function_response, id } => {
                                println!("    [{}] FunctionResponse:", i);
                                println!("        name: {}", function_response.name);
                                println!("        response: {}", function_response.response);
                                println!("        id: {:?}", id);
                            }
                            _ => {
                                println!("    [{}] Other part type", i);
                            }
                        }
                    }
                }
            }
            Err(e) => {
                println!("Error: {}", e);
            }
        }
    }

    println!("\nTotal responses: {}", response_count);
    Ok(())
}
