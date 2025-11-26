use adk_agent::LlmAgentBuilder;
use adk_core::{Agent, CallbackContext, Llm, LlmRequest, LlmResponse};
use adk_model::gemini::GeminiModel;
use async_trait::async_trait;
use futures::StreamExt;
use std::sync::{Arc, Mutex};

// Helper to create a simple test context
mod test_context {
    use super::*;
    use adk_core::{Content, InvocationContext, Part, ReadonlyContext, RunConfig};

    pub struct TestContext {
        content: Content,
        config: RunConfig,
    }

mod test_context;
use test_context::TestContext;

#[tokio::test]
async fn test_before_model_callback_logging() {
    let api_key = std::env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY not set");
    let model = GeminiModel::new(api_key, "gemini-2.0-flash-exp").unwrap();

    // Track that callback was called
    let callback_called = Arc::new(Mutex::new(false));
    let callback_flag = callback_called.clone();

    let agent = LlmAgentBuilder::new("test_agent")
        .description("Test agent")
        .model(Arc::new(model))
        .before_model_callback(Box::new(move |_ctx: Arc<dyn CallbackContext>, req: LlmRequest| {
            let flag = callback_flag.clone();
            Box::pin(async move {
                *flag.lock().unwrap() = true;
                println!("BeforeModel: Model={}, Contents={}", req.model, req.contents.len());
                Ok(None) // Don't skip model call
            })
        }))
        .build()
        .unwrap();

    let ctx = Arc::new(test_context::TestContext::new("What is 1+1?"));
    let mut stream = agent.run(ctx).await.unwrap();

    // Consume events
    while let Some(_) = stream.next().await {}

    // Verify callback was called
    assert!(*callback_called.lock().unwrap(), "BeforeModel callback should have been called");
}

#[tokio::test]
async fn test_before_model_callback_caching() {
    let api_key = std::env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY not set");
    let model = GeminiModel::new(api_key, "gemini-2.0-flash-exp").unwrap();

    // This callback returns a cached response, skipping the model call
    let agent = LlmAgentBuilder::new("cached_agent")
        .description("Agent with caching")
        .model(Arc::new(model))
        .before_model_callback(Box::new(|_ctx: Arc<dyn CallbackContext>, _req: LlmRequest| {
            Box::pin(async move {
                println!("BeforeModel: Returning cached response");
                // Return a cached response
                Ok(Some(LlmResponse {
                    content: Some(adk_core::Content {
                        role: "model".to_string(),
                        parts: vec![adk_core::Part::Text {
                            text: "CACHED RESPONSE".to_string(),
                        }],
                    }),
                    usage_metadata: None,
                    finish_reason: Some(adk_core::FinishReason::Stop),
                    partial: false,
                    turn_complete: true,
                    interrupted: false,
                    error_code: None,
                    error_message: None,
                }))
            })
        }))
        .build()
        .unwrap();

    let ctx = Arc::new(test_context::TestContext::new("This should be cached"));
    let mut stream = agent.run(ctx).await.unwrap();

    // Collect events
    let mut found_cached_response = false;
    while let Some(result) = stream.next().await {
        let event = result.unwrap();
        if let Some(content) = &event.content {
            for part in &content.parts {
                if let adk_core::Part::Text { text } = part {
                    if text.contains("CACHED RESPONSE") {
                        found_cached_response = true;
                    }
                }
            }
        }
    }

    assert!(found_cached_response, "Should have received cached response from callback");
}

#[tokio::test]
async fn test_after_model_callback_modification() {
    let api_key = std::env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY not set");
    let model = GeminiModel::new(api_key, "gemini-2.0-flash-exp").unwrap();

    let agent = LlmAgentBuilder::new("modifying_agent")
        .description("Agent that modifies responses")
        .model(Arc::new(model))
        .after_model_callback(Box::new(|_ctx: Arc<dyn CallbackContext>, mut response: LlmResponse| {
            Box::pin(async move {
                println!("AfterModel: Modifying response");
                // Add a prefix to the response
                if let Some(ref mut content) = response.content {
                    content.parts.insert(0, adk_core::Part::Text {
                        text: "[MODIFIED] ".to_string(),
                    });
                }
                Ok(Some(response))
            })
        }))
        .build()
        .unwrap();

    let ctx = Arc::new(test_context::TestContext::new("Say hello"));
    let mut stream = agent.run(ctx).await.unwrap();

    // Collect all text
    let mut all_text = String::new();
    while let Some(result) = stream.next().await {
        let event = result.unwrap();
        if let Some(content) = &event.content {
            for part in &content.parts {
                if let adk_core::Part::Text { text } = part {
                    all_text.push_str(text);
                }
            }
        }
    }

    println!("Final text: {}", all_text);
    assert!(all_text.contains("[MODIFIED]"), "Response should contain modification marker");
}

#[tokio::test]
async fn test_multiple_callbacks_in_sequence() {
    let api_key = std::env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY not set");
    let model = GeminiModel::new(api_key, "gemini-2.0-flash-exp").unwrap();

    let call_order = Arc::new(Mutex::new(Vec::new()));
    let order1 = call_order.clone();
    let order2 = call_order.clone();

    let agent = LlmAgentBuilder::new("multi_callback_agent")
        .description("Multiple callbacks")
        .model(Arc::new(model))
        .before_model_callback(Box::new(move |_ctx, _req| {
            let order = order1.clone();
            Box::pin(async move {
                order.lock().unwrap().push("before1");
                Ok(None)
            })
        }))
        .before_model_callback(Box::new(move |_ctx, _req| {
            let order = order2.clone();
            Box::pin(async move {
                order.lock().unwrap().push("before2");
                Ok(None)
            })
        }))
        .build()
        .unwrap();

    let ctx = Arc::new(test_context::TestContext::new("Test"));
    let mut stream = agent.run(ctx).await.unwrap();
    while let Some(_) = stream.next().await {}

    let order = call_order.lock().unwrap();
    assert_eq!(order[0], "before1");
    assert_eq!(order[1], "before2");
}
