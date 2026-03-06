use adk_agent::LlmAgentBuilder;
use adk_core::{
    Agent, Artifacts, CallbackContext, Content, Part, ReadonlyContext, types::AdkIdentity,
};
use adk_model::gemini::GeminiModel;
use async_trait::async_trait;
use std::sync::{Arc, Mutex};

// Mock context for callback testing
struct MockCallbackContext {
    content: Content,
    identity: AdkIdentity,
    metadata: std::collections::HashMap<String, String>,
}

impl MockCallbackContext {
    fn new(id: &str) -> Self {
        let mut identity = AdkIdentity::default();
        identity.invocation_id = id.to_string());
        Self { content: Content::new("user"), identity, metadata: std::collections::HashMap::new() }
    }
}

#[async_trait]
impl ReadonlyContext for MockCallbackContext {
    fn identity(&self) -> &AdkIdentity {
        &self.identity
    }
    fn user_content(&self) -> &Content {
        &self.content
    }
    fn metadata(&self) -> &std::collections::HashMap<String, String> {
        &self.metadata
    }
}

#[async_trait]
impl CallbackContext for MockCallbackContext {
    fn artifacts(&self) -> Option<Arc<dyn Artifacts>> {
        None
    }
}

#[tokio::test]
async fn test_callback_execution() {
    let before_called = Arc::new(Mutex::new(false));
    let after_called = Arc::new(Mutex::new(false));

    let before_flag = before_called.clone();
    let after_flag = after_called.clone();

    // Create callbacks
    let before_callback = Box::new(move |_ctx: Arc<dyn CallbackContext>| {
        let flag = before_flag.clone();
        Box::pin(async move {
            *flag.lock().unwrap() = true;
            Ok(Some(Content {
                role: "system".to_string(),
                parts: vec![Part::text("Before callback executed".to_string())],
            }))
        })
            as std::pin::Pin<
                Box<dyn std::future::Future<Output = adk_core::Result<Option<Content>>> + Send>,
            >
    });

    let after_callback = Box::new(move |_ctx: Arc<dyn CallbackContext>| {
        let flag = after_flag.clone();
        Box::pin(async move {
            *flag.lock().unwrap() = true;
            Ok(Some(Content {
                role: "system".to_string(),
                parts: vec![Part::text("After callback executed".to_string())],
            }))
        })
            as std::pin::Pin<
                Box<dyn std::future::Future<Output = adk_core::Result<Option<Content>>> + Send>,
            >
    });

    // Execute callbacks
    let ctx = Arc::new(MockCallbackContext::new("test-inv"));

    let before_result = before_callback(ctx.clone()).await.unwrap();
    assert!(before_result.is_some());
    assert_eq!(
        before_result.unwrap().parts[0],
        Part::text("Before callback executed".to_string())
    );
    assert!(*before_called.lock().unwrap());

    let after_result = after_callback(ctx).await.unwrap();
    assert!(after_result.is_some());
    assert_eq!(
        after_result.unwrap().parts[0],
        Part::text("After callback executed".to_string())
    );
    assert!(*after_called.lock().unwrap());
}

#[test]
fn test_llm_agent_stores_callbacks() {
    let api_key = std::env::var("GEMINI_API_KEY").unwrap_or_else(|_| "test".to_string());
    let model = GeminiModel::new(&api_key, "gemini-2.5-flash").expect("Failed to create model");

    let agent = LlmAgentBuilder::new("test_agent")
        .model(Arc::new(model))
        .before_callback(Box::new(|_ctx| Box::pin(async move { Ok(None) })))
        .after_callback(Box::new(|_ctx| Box::pin(async move { Ok(None) })))
        .build()
        .expect("Failed to build agent");

    // Agent should be created successfully with callbacks
    assert_eq!(agent.name(), "test_agent");
}

#[tokio::test]
async fn test_callback_error_handling() {
    let error_callback = Box::new(|_ctx: Arc<dyn CallbackContext>| {
        Box::pin(async move { Err(adk_core::AdkError::Agent("Callback error".to_string())) })
            as std::pin::Pin<
                Box<dyn std::future::Future<Output = adk_core::Result<Option<Content>>> + Send>,
            >
    });

    let ctx = Arc::new(MockCallbackContext::new("test-inv"));
    let result = error_callback(ctx).await;

    assert!(result.is_err());
    match result {
        Err(adk_core::AdkError::Agent(msg)) => {
            assert_eq!(msg, "Callback error");
        }
        _ => panic!("Expected Agent error"),
    }
}

#[tokio::test]
async fn test_callback_returns_none() {
    let none_callback = Box::new(|_ctx: Arc<dyn CallbackContext>| {
        Box::pin(async move { Ok(None) })
            as std::pin::Pin<
                Box<dyn std::future::Future<Output = adk_core::Result<Option<Content>>> + Send>,
            >
    });

    let ctx = Arc::new(MockCallbackContext::new("test-inv"));
    let result = none_callback(ctx).await.unwrap();

    assert!(result.is_none());
}
