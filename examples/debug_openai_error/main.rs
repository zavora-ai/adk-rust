//! Debug OpenAI errors to find root cause of 400 Bad Request

use adk_core::{Content, Llm, LlmRequest, Part};
use adk_model::openai::{OpenAIClient, OpenAIConfig};
use futures::StreamExt;
use std::collections::HashMap;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY must be set");
    let model = OpenAIClient::new(OpenAIConfig::new(api_key, "gpt-5-mini"))?;

    println!("=== Test A: Empty assistant message (no text, no tool calls) ===");
    test_request(
        &model,
        vec![
            Content {
                role: "user".to_string(),
                parts: vec![Part::Text { text: "Hello".to_string() }],
            },
            Content {
                role: "model".to_string(),
                parts: vec![], // Empty parts - this creates an empty assistant message!
            },
            Content {
                role: "user".to_string(),
                parts: vec![Part::Text { text: "Say more".to_string() }],
            },
        ],
        HashMap::new(),
    )
    .await;

    println!("\n=== Test B: Assistant message with empty text ===");
    test_request(
        &model,
        vec![
            Content {
                role: "user".to_string(),
                parts: vec![Part::Text { text: "Hello".to_string() }],
            },
            Content {
                role: "model".to_string(),
                parts: vec![Part::Text { text: "".to_string() }], // Empty text
            },
            Content {
                role: "user".to_string(),
                parts: vec![Part::Text { text: "Say more".to_string() }],
            },
        ],
        HashMap::new(),
    )
    .await;

    println!("\n=== Test C: Multiple assistant messages in a row ===");
    test_request(
        &model,
        vec![
            Content {
                role: "user".to_string(),
                parts: vec![Part::Text { text: "Hello".to_string() }],
            },
            Content {
                role: "model".to_string(),
                parts: vec![Part::Text { text: "Hi".to_string() }],
            },
            Content {
                role: "model".to_string(),
                parts: vec![Part::Text { text: "How are you?".to_string() }],
            },
            Content {
                role: "user".to_string(),
                parts: vec![Part::Text { text: "Fine".to_string() }],
            },
        ],
        HashMap::new(),
    )
    .await;

    println!("\n=== Test D: Multiple user messages in a row ===");
    test_request(
        &model,
        vec![
            Content {
                role: "user".to_string(),
                parts: vec![Part::Text { text: "Hello".to_string() }],
            },
            Content {
                role: "user".to_string(),
                parts: vec![Part::Text { text: "Are you there?".to_string() }],
            },
        ],
        HashMap::new(),
    )
    .await;

    println!("\n=== Test E: User message with empty text ===");
    test_request(
        &model,
        vec![Content {
            role: "user".to_string(),
            parts: vec![Part::Text { text: "".to_string() }],
        }],
        HashMap::new(),
    )
    .await;

    println!("\n=== Test F: Simulating parallel agent merging (multiple responses) ===");
    // In parallel agents, multiple sub-agent responses might get accumulated
    test_request(
        &model,
        vec![
            Content {
                role: "user".to_string(),
                parts: vec![Part::Text { text: "Analyze this".to_string() }],
            },
            Content {
                role: "model".to_string(),
                parts: vec![
                    Part::Text { text: "Technical view: ...".to_string() },
                    Part::Text { text: "Business view: ...".to_string() },
                    Part::Text { text: "User view: ...".to_string() },
                ],
            },
            Content {
                role: "user".to_string(),
                parts: vec![Part::Text { text: "Thanks".to_string() }],
            },
        ],
        HashMap::new(),
    )
    .await;

    Ok(())
}

async fn test_request(
    model: &OpenAIClient,
    contents: Vec<Content>,
    tools: HashMap<String, serde_json::Value>,
) {
    let request = LlmRequest { model: "gpt-5-mini".to_string(), contents, tools, config: None };

    println!("Sending request...");
    match model.generate_content(request, true).await {
        Ok(mut stream) => {
            let mut count = 0;
            while let Some(result) = stream.next().await {
                count += 1;
                match result {
                    Ok(response) => {
                        if let Some(content) = &response.content {
                            for part in &content.parts {
                                if let Part::Text { text } = part {
                                    print!("{}", text);
                                }
                            }
                        }
                    }
                    Err(e) => {
                        println!("\n*** ERROR in chunk {}: {} ***", count, e);
                    }
                }
            }
            println!("\nTotal chunks: {}", count);
        }
        Err(e) => {
            println!("*** REQUEST FAILED: {} ***", e);
        }
    }
}
