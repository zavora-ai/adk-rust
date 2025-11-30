use adk_core::{Content, Llm, LlmRequest};
use adk_model::gemini::GeminiModel;
use futures::StreamExt;
use serde_json::json;

fn get_api_key() -> Option<String> {
    std::env::var("GEMINI_API_KEY").ok()
}

#[tokio::test]
#[ignore]
async fn test_google_search_zavora() {
    let api_key = match get_api_key() {
        Some(key) => key,
        None => {
            println!("Skipping test: GEMINI_API_KEY not set");
            return;
        }
    };

    let model = GeminiModel::new(api_key, "gemini-2.0-flash-exp").unwrap();

    let content =
        Content::new("user").with_text("Search for information about Zavora Technologies");
    let mut request = LlmRequest::new("gemini-2.0-flash-exp", vec![content]);

    // Add Google Search tool
    let google_search_tool = json!({
        "googleSearch": {}
    });
    request.tools.insert("google_search".to_string(), google_search_tool);

    let mut stream = model.generate_content(request, false).await.unwrap();
    let response = stream.next().await.unwrap().unwrap();

    assert!(response.content.is_some());
    let content = response.content.unwrap();
    let part = content.parts.first().unwrap();

    if let adk_core::Part::Text { text } = part {
        println!("Response: {}", text);
        assert!(!text.is_empty());
        // Should contain information from search
        assert!(text.len() > 50);
    }
}
