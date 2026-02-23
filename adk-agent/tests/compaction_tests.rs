//! Tests for the LlmEventSummarizer context compaction.

use adk_agent::LlmEventSummarizer;
use adk_core::{
    BaseEventsSummarizer, Content, Event, Llm, LlmRequest, LlmResponse, LlmResponseStream, Part,
    Result,
};
use async_trait::async_trait;
use std::sync::Arc;

/// A mock LLM that echoes back a fixed summary.
struct MockSummarizerLlm {
    summary_text: String,
}

#[async_trait]
impl Llm for MockSummarizerLlm {
    fn name(&self) -> &str {
        "mock-summarizer"
    }

    async fn generate_content(&self, _req: LlmRequest, _stream: bool) -> Result<LlmResponseStream> {
        let content = Content::new("model").with_text(&self.summary_text);
        let response = LlmResponse::new(content);
        Ok(Box::pin(futures::stream::once(async { Ok(response) })))
    }
}

fn make_event(author: &str, text: &str) -> Event {
    let mut event = Event::new("inv-test");
    event.author = author.to_string();
    event.set_content(Content {
        role: if author == "user" { "user" } else { "model" }.to_string(),
        parts: vec![Part::Text { text: text.to_string() }],
    });
    event
}

#[tokio::test]
async fn test_summarize_empty_events_returns_none() {
    let llm = Arc::new(MockSummarizerLlm { summary_text: "summary".into() });
    let summarizer = LlmEventSummarizer::new(llm);

    let result = summarizer.summarize_events(&[]).await.unwrap();
    assert!(result.is_none());
}

#[tokio::test]
async fn test_summarize_produces_compaction_event() {
    let llm = Arc::new(MockSummarizerLlm {
        summary_text: "User asked about weather. Agent provided NYC forecast.".into(),
    });
    let summarizer = LlmEventSummarizer::new(llm);

    let events = vec![
        make_event("user", "What's the weather in NYC?"),
        make_event("assistant", "The weather in NYC is 72°F and sunny."),
        make_event("user", "Thanks!"),
        make_event("assistant", "You're welcome!"),
    ];

    let result = summarizer.summarize_events(&events).await.unwrap();
    assert!(result.is_some());

    let compaction_event = result.unwrap();
    assert_eq!(compaction_event.author, "system");

    // Must have compaction metadata
    let compaction = compaction_event.actions.compaction.as_ref().unwrap();
    assert_eq!(compaction.start_timestamp, events[0].timestamp);
    assert_eq!(compaction.end_timestamp, events[3].timestamp);

    // Compacted content should be the LLM summary
    assert_eq!(compaction.compacted_content.role, "model");
    let text = match &compaction.compacted_content.parts[0] {
        Part::Text { text } => text.clone(),
        _ => panic!("Expected text part"),
    };
    assert!(text.contains("weather"));
}

#[tokio::test]
async fn test_summarize_with_custom_prompt_template() {
    let llm = Arc::new(MockSummarizerLlm { summary_text: "Custom summary output".into() });
    let summarizer = LlmEventSummarizer::new(llm)
        .with_prompt_template("Summarize briefly: {conversation_history}");

    let events = vec![make_event("user", "Hello"), make_event("assistant", "Hi there")];

    let result = summarizer.summarize_events(&events).await.unwrap();
    assert!(result.is_some());

    let compaction = result.unwrap().actions.compaction.unwrap();
    let text = match &compaction.compacted_content.parts[0] {
        Part::Text { text } => text.clone(),
        _ => panic!("Expected text part"),
    };
    assert_eq!(text, "Custom summary output");
}

#[tokio::test]
async fn test_summarize_skips_non_text_parts() {
    let llm = Arc::new(MockSummarizerLlm { summary_text: "Summary of tool interaction".into() });
    let summarizer = LlmEventSummarizer::new(llm);

    // Event with function call (no text) — should be skipped in prompt formatting
    let mut fc_event = Event::new("inv-test");
    fc_event.author = "assistant".to_string();
    fc_event.set_content(Content {
        role: "model".to_string(),
        parts: vec![Part::FunctionCall {
            name: "get_weather".to_string(),
            args: serde_json::json!({}),
            id: Some("call_1".to_string()),
            thought_signature: None,
        }],
    });

    let events =
        vec![make_event("user", "Check weather"), fc_event, make_event("assistant", "It's sunny")];

    let result = summarizer.summarize_events(&events).await.unwrap();
    assert!(result.is_some());
}
