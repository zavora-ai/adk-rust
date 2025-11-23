use adk_core::{Content, Llm, LlmRequest};
use adk_model::gemini::GeminiModel;
use futures::StreamExt;

fn get_api_key() -> Option<String> {
    std::env::var("GEMINI_API_KEY").ok()
}

#[tokio::test]
#[ignore] // Run with: cargo test -- --ignored --test-threads=1
async fn test_gemini_generate_content() {
    let api_key = match get_api_key() {
        Some(key) => key,
        None => {
            println!("Skipping test: GEMINI_API_KEY not set");
            return;
        }
    };

    let model = GeminiModel::new(api_key, "gemini-2.0-flash-exp").unwrap();
    
    let content = Content::new("user").with_text("Say 'Hello' in one word");
    let request = LlmRequest::new("gemini-2.0-flash-exp", vec![content]);
    
    let mut stream = model.generate_content(request, false).await.unwrap();
    
    let response = stream.next().await.unwrap().unwrap();
    assert!(response.content.is_some());
    assert!(!response.partial);
    assert!(response.turn_complete);
    
    let content = response.content.unwrap();
    let part = content.parts.first().unwrap();
    if let adk_core::Part::Text { text } = part {
        assert!(!text.is_empty());
        println!("Response: {}", text);
    }
}

#[tokio::test]
#[ignore]
async fn test_gemini_streaming() {
    let api_key = match get_api_key() {
        Some(key) => key,
        None => {
            println!("Skipping test: GEMINI_API_KEY not set");
            return;
        }
    };

    let model = GeminiModel::new(api_key, "gemini-2.0-flash-exp").unwrap();
    
    let content = Content::new("user").with_text("Count from 1 to 3");
    let request = LlmRequest::new("gemini-2.0-flash-exp", vec![content]);
    
    let mut stream = model.generate_content(request, true).await.unwrap();
    
    let mut chunk_count = 0;
    while let Some(result) = stream.next().await {
        let response = result.unwrap();
        assert!(response.partial);
        assert!(!response.turn_complete);
        chunk_count += 1;
    }
    
    assert!(chunk_count > 0, "Should receive at least one chunk");
    println!("Received {} chunks", chunk_count);
}

#[tokio::test]
#[ignore]
async fn test_gemini_with_config() {
    let api_key = match get_api_key() {
        Some(key) => key,
        None => {
            println!("Skipping test: GEMINI_API_KEY not set");
            return;
        }
    };

    let model = GeminiModel::new(api_key, "gemini-2.0-flash-exp").unwrap();
    
    let content = Content::new("user").with_text("Say hello");
    let mut request = LlmRequest::new("gemini-2.0-flash-exp", vec![content]);
    request.config = Some(adk_core::GenerateContentConfig {
        temperature: Some(0.1),
        top_p: Some(0.95),
        top_k: Some(40),
        max_output_tokens: Some(50),
    });
    
    let mut stream = model.generate_content(request, false).await.unwrap();
    let response = stream.next().await.unwrap().unwrap();
    
    assert!(response.content.is_some());
    assert!(response.usage_metadata.is_some());
    
    let usage = response.usage_metadata.unwrap();
    assert!(usage.total_token_count > 0);
    println!("Token usage: {}", usage.total_token_count);
}

#[tokio::test]
#[ignore]
async fn test_gemini_conversation() {
    let api_key = match get_api_key() {
        Some(key) => key,
        None => {
            println!("Skipping test: GEMINI_API_KEY not set");
            return;
        }
    };

    let model = GeminiModel::new(api_key, "gemini-2.0-flash-exp").unwrap();
    
    let contents = vec![
        Content::new("user").with_text("My name is Alice"),
        Content::new("model").with_text("Hello Alice! Nice to meet you."),
        Content::new("user").with_text("What is my name?"),
    ];
    
    let request = LlmRequest::new("gemini-2.0-flash-exp", contents);
    let mut stream = model.generate_content(request, false).await.unwrap();
    let response = stream.next().await.unwrap().unwrap();
    
    let content = response.content.unwrap();
    let part = content.parts.first().unwrap();
    if let adk_core::Part::Text { text } = part {
        assert!(text.to_lowercase().contains("alice"));
        println!("Response: {}", text);
    }
}
