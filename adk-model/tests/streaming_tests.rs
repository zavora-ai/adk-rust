use adk_core::{Content, Llm, LlmRequest, Part};
use adk_model::gemini::{streaming::aggregate_stream, GeminiModel};

fn get_api_key() -> Option<String> {
    std::env::var("GEMINI_API_KEY").ok()
}

#[tokio::test]
#[ignore]
async fn test_stream_aggregation() {
    let api_key = match get_api_key() {
        Some(key) => key,
        None => {
            println!("Skipping test: GEMINI_API_KEY not set");
            return;
        }
    };

    let model = GeminiModel::new(api_key, "gemini-2.0-flash-exp").unwrap();
    let content = Content::new("user").with_text("Count from 1 to 5");
    let request = LlmRequest::new("gemini-2.0-flash-exp", vec![content]);

    let stream = model.generate_content(request, true).await.unwrap();
    let aggregated = aggregate_stream(stream).await.unwrap();

    assert!(aggregated.content.is_some());
    assert!(!aggregated.partial);
    assert!(aggregated.turn_complete);

    let content = aggregated.content.unwrap();
    let part = content.parts.first().unwrap();
    if let Part::Text { text } = part {
        assert!(!text.is_empty());
        println!("Aggregated: {}", text);
    }
}
