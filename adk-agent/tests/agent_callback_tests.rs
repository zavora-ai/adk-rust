use adk_agent::LlmAgentBuilder;
use adk_core::{Agent, CallbackContext, Content, Part};
use async_trait::async_trait;
use futures::StreamExt;
use std::sync::{Arc, Mutex};

mod test_context;
use test_context::TestContext;

struct MockLlm;

#[async_trait]
impl adk_core::Llm for MockLlm {
    fn name(&self) -> &str {
        "mock-llm"
    }

    async fn generate_content(
        &self,
        _request: adk_core::LlmRequest,
        _stream: bool,
    ) -> adk_core::Result<adk_core::LlmResponseStream> {
        let s = async_stream::stream! {
            yield Ok(adk_core::LlmResponse {
                content: Some(adk_core::Content {
                    role: "model".to_string(),
                    parts: vec![adk_core::Part::Text { text: "mock response".to_string() }],
                }),
                usage_metadata: None,
                finish_reason: None,
                citation_metadata: None,
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

#[tokio::test]
async fn test_before_agent_callback() {
    let model = MockLlm;

    let callback_called = Arc::new(Mutex::new(false));
    let flag = callback_called.clone();

    let agent = LlmAgentBuilder::new("test_agent")
        .description("Test agent")
        .model(Arc::new(model))
        .before_callback(Box::new(move |_ctx: Arc<dyn CallbackContext>| {
            let f = flag.clone();
            Box::pin(async move {
                *f.lock().unwrap() = true;
                println!("BeforeAgent callback executed");
                Ok(None) // Don't skip agent
            })
        }))
        .build()
        .unwrap();

    let ctx = Arc::new(TestContext::new("test"));
    let mut stream = agent.run(ctx).await.unwrap();
    while (stream.next().await).is_some() {}

    assert!(*callback_called.lock().unwrap(), "BeforeAgent callback should execute");
}

#[tokio::test]
async fn test_before_agent_callback_skip_execution() {
    let model = MockLlm;

    let agent = LlmAgentBuilder::new("skip_agent")
        .description("Agent that gets skipped")
        .model(Arc::new(model))
        .before_callback(Box::new(|_ctx: Arc<dyn CallbackContext>| {
            Box::pin(async move {
                println!("BeforeAgent: Skipping agent execution");
                Ok(Some(Content {
                    role: "model".to_string(),
                    parts: vec![Part::Text { text: "AGENT SKIPPED BY CALLBACK".to_string() }],
                }))
            })
        }))
        .build()
        .unwrap();

    let ctx = Arc::new(TestContext::new("This should not reach the model"));
    let mut stream = agent.run(ctx).await.unwrap();

    let mut found_skip_message = false;
    while let Some(result) = stream.next().await {
        if let Ok(event) = result {
            if let Some(content) = &event.llm_response.content {
                for part in &content.parts {
                    if let Part::Text { text } = part {
                        if text.contains("AGENT SKIPPED BY CALLBACK") {
                            found_skip_message = true;
                        }
                    }
                }
            }
        }
    }

    assert!(found_skip_message, "Should receive callback content instead of model response");
}

#[tokio::test]
async fn test_after_agent_callback() {
    let model = MockLlm;

    let callback_called = Arc::new(Mutex::new(false));
    let flag = callback_called.clone();

    let agent = LlmAgentBuilder::new("test_agent")
        .description("Test agent")
        .model(Arc::new(model))
        .after_callback(Box::new(move |_ctx: Arc<dyn CallbackContext>| {
            let f = flag.clone();
            Box::pin(async move {
                *f.lock().unwrap() = true;
                println!("AfterAgent callback executed");
                Ok(Some(Content {
                    role: "model".to_string(),
                    parts: vec![Part::Text { text: "AFTER AGENT CALLBACK".to_string() }],
                }))
            })
        }))
        .build()
        .unwrap();

    let ctx = Arc::new(TestContext::new("Say hello"));
    let mut stream = agent.run(ctx).await.unwrap();

    let mut found_after_message = false;
    while let Some(result) = stream.next().await {
        if let Ok(event) = result {
            if let Some(content) = &event.llm_response.content {
                for part in &content.parts {
                    if let Part::Text { text } = part {
                        if text.contains("AFTER AGENT CALLBACK") {
                            found_after_message = true;
                        }
                    }
                }
            }
        }
    }

    assert!(*callback_called.lock().unwrap(), "AfterAgent callback should execute");
    assert!(found_after_message, "Should receive after agent callback content");
}
