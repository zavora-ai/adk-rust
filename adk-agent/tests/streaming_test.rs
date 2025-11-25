use adk_agent::LlmAgentBuilder;
use adk_core::{Agent, Content, Event, Llm, LlmRequest, LlmResponse, LlmResponseStream, Part, Result};
use async_stream::stream;
use async_trait::async_trait;
use futures::StreamExt;
use std::sync::Arc;

/// Mock model that simulates streaming responses with multiple chunks
struct MockStreamingModel {
    chunks: Vec<&'static str>,
}

#[async_trait]
impl Llm for MockStreamingModel {
    fn name(&self) -> &str {
        "mock-streaming"
    }

    async fn generate_content(&self, _req: LlmRequest, stream: bool) -> Result<LlmResponseStream> {
        let chunks = self.chunks.clone();
        
        if stream {
            // Simulate streaming: yield multiple partial chunks
            let s = stream! {
                for (i, chunk_text) in chunks.iter().enumerate() {
                    let is_last = i == chunks.len() - 1;
                    
                    yield Ok(LlmResponse {
                        content: Some(Content {
                            role: "model".to_string(),
                            parts: vec![Part::Text { text: chunk_text.to_string() }],
                        }),
                        usage_metadata: None,
                        finish_reason: if is_last { Some(adk_core::FinishReason::Stop) } else { None },
                        partial: !is_last,
                        turn_complete: is_last,
                        interrupted: false,
                        error_code: None,
                        error_message: None,
                    });
                }
            };
            Ok(Box::pin(s))
        } else {
            // Non-streaming: return all content in one chunk (old behavior)
            let full_text = chunks.join("");
            let s = stream! {
                yield Ok(LlmResponse {
                    content: Some(Content {
                        role: "model".to_string(),
                        parts: vec![Part::Text { text: full_text }],
                    }),
                    usage_metadata: None,
                    finish_reason: Some(adk_core::FinishReason::Stop),
                    partial: false,
                    turn_complete: true,
                    interrupted: false,
                    error_code: None,
                    error_message: None,
                });
            };
            Ok(Box::pin(s))
        }
    }
}

#[tokio::test]
async fn test_streaming_yields_multiple_events() {
    // Create a mock model that returns 3 chunks
    let model = Arc::new(MockStreamingModel {
        chunks: vec!["Hello ", "world", "!"],
    });

    let agent = LlmAgentBuilder::new("streaming_agent")
        .description("Test agent for streaming")
        .model(model)
        .build()
        .expect("Failed to build agent");

    // Create a simple test context using the runner
    use adk_runner::Runner;
    use adk_session::InMemorySessionService;
    
    let session_service = Arc::new(InMemorySessionService::new());
    let runner = Runner::new(Arc::new(agent), session_service.clone(), None, None);
    
    let user_content = Content {
        role: "user".to_string(),
        parts: vec![Part::Text { text: "test".to_string() }],
    };
    
    // Run the agent
    let mut event_stream = runner.run("test-user", "test-session", user_content).await.unwrap();

    // Collect all events
    let mut events = Vec::new();
    while let Some(result) = event_stream.next().await {
        match result {
            Ok(event) => {
                println!("Event: author={}, has_content={}", event.author, event.content.is_some());
                events.push(event);
            }
            Err(e) => panic!("Error in event stream: {:?}", e),
        }
    }

    // Verify we got multiple events (one per chunk)
    println!("Total events received: {}", events.len());
    assert!(
        events.len() >= 3,
        "Expected at least 3 events (3 chunks), got {}",
        events.len()
    );

    // Verify early events have content (not waiting for full response)
    let first_event = events.iter().find(|e| e.content.is_some());
    assert!(first_event.is_some(), "Should have at least one event with content");
}
