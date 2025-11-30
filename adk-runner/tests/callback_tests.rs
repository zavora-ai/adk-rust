use adk_core::{Artifacts, CallbackContext, Content, Part, ReadonlyContext};
use adk_runner::Callbacks;
use async_trait::async_trait;
use std::sync::{Arc, Mutex};

// Mock context for testing
struct MockCallbackContext {
    invocation_id: String,
    content: Content,
}

impl MockCallbackContext {
    fn new(id: &str) -> Self {
        Self { invocation_id: id.to_string(), content: Content::new("user") }
    }
}

#[async_trait]
impl ReadonlyContext for MockCallbackContext {
    fn invocation_id(&self) -> &str {
        &self.invocation_id
    }
    fn agent_name(&self) -> &str {
        "test-agent"
    }
    fn user_id(&self) -> &str {
        "test-user"
    }
    fn app_name(&self) -> &str {
        "test-app"
    }
    fn session_id(&self) -> &str {
        "test-session"
    }
    fn branch(&self) -> &str {
        ""
    }
    fn user_content(&self) -> &Content {
        &self.content
    }
}

#[async_trait]
impl CallbackContext for MockCallbackContext {
    fn artifacts(&self) -> Option<Arc<dyn Artifacts>> {
        None
    }
}

#[tokio::test]
async fn test_callbacks_creation() {
    let callbacks = Callbacks::new();
    assert_eq!(callbacks.before_model.len(), 0);
    assert_eq!(callbacks.after_model.len(), 0);
    assert_eq!(callbacks.before_tool.len(), 0);
    assert_eq!(callbacks.after_tool.len(), 0);
}

#[tokio::test]
async fn test_add_before_model_callback() {
    let mut callbacks = Callbacks::new();

    callbacks.add_before_model(Box::new(|_ctx| Box::pin(async move { Ok(None) })));

    assert_eq!(callbacks.before_model.len(), 1);
}

#[tokio::test]
async fn test_execute_before_model_callbacks() {
    let mut callbacks = Callbacks::new();
    let call_count = Arc::new(Mutex::new(0));

    let count1 = call_count.clone();
    callbacks.add_before_model(Box::new(move |_ctx| {
        let count = count1.clone();
        Box::pin(async move {
            *count.lock().unwrap() += 1;
            Ok(Some(Content {
                role: "system".to_string(),
                parts: vec![Part::Text { text: "Before model 1".to_string() }],
            }))
        })
    }));

    let count2 = call_count.clone();
    callbacks.add_before_model(Box::new(move |_ctx| {
        let count = count2.clone();
        Box::pin(async move {
            *count.lock().unwrap() += 1;
            Ok(Some(Content {
                role: "system".to_string(),
                parts: vec![Part::Text { text: "Before model 2".to_string() }],
            }))
        })
    }));

    let ctx = Arc::new(MockCallbackContext::new("test-inv"));
    let results = callbacks.execute_before_model(ctx).await.unwrap();

    assert_eq!(results.len(), 2);
    assert_eq!(*call_count.lock().unwrap(), 2);
}

#[tokio::test]
async fn test_execute_after_model_callbacks() {
    let mut callbacks = Callbacks::new();

    callbacks.add_after_model(Box::new(|_ctx| {
        Box::pin(async move {
            Ok(Some(Content {
                role: "assistant".to_string(),
                parts: vec![Part::Text { text: "After model".to_string() }],
            }))
        })
    }));

    let ctx = Arc::new(MockCallbackContext::new("test-inv"));
    let results = callbacks.execute_after_model(ctx).await.unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].role, "assistant");
}

#[tokio::test]
async fn test_execute_before_tool_callbacks() {
    let mut callbacks = Callbacks::new();

    callbacks.add_before_tool(Box::new(|_ctx| {
        Box::pin(async move {
            Ok(Some(Content {
                role: "system".to_string(),
                parts: vec![Part::Text { text: "Before tool".to_string() }],
            }))
        })
    }));

    let ctx = Arc::new(MockCallbackContext::new("test-inv"));
    let results = callbacks.execute_before_tool(ctx).await.unwrap();

    assert_eq!(results.len(), 1);
}

#[tokio::test]
async fn test_execute_after_tool_callbacks() {
    let mut callbacks = Callbacks::new();

    callbacks.add_after_tool(Box::new(|_ctx| {
        Box::pin(async move {
            Ok(Some(Content {
                role: "function".to_string(),
                parts: vec![Part::Text { text: "After tool".to_string() }],
            }))
        })
    }));

    let ctx = Arc::new(MockCallbackContext::new("test-inv"));
    let results = callbacks.execute_after_tool(ctx).await.unwrap();

    assert_eq!(results.len(), 1);
}

#[tokio::test]
async fn test_callback_returns_none() {
    let mut callbacks = Callbacks::new();

    callbacks.add_before_model(Box::new(|_ctx| Box::pin(async move { Ok(None) })));

    let ctx = Arc::new(MockCallbackContext::new("test-inv"));
    let results = callbacks.execute_before_model(ctx).await.unwrap();

    assert_eq!(results.len(), 0);
}

#[tokio::test]
async fn test_callback_error_handling() {
    let mut callbacks = Callbacks::new();

    callbacks.add_before_model(Box::new(|_ctx| {
        Box::pin(async move { Err(adk_core::AdkError::Agent("Test error".to_string())) })
    }));

    let ctx = Arc::new(MockCallbackContext::new("test-inv"));
    let result = callbacks.execute_before_model(ctx).await;

    assert!(result.is_err());
}

#[tokio::test]
async fn test_multiple_callback_types() {
    let mut callbacks = Callbacks::new();

    callbacks.add_before_model(Box::new(|_ctx| {
        Box::pin(async move { Ok(Some(Content::new("system"))) })
    }));

    callbacks.add_after_model(Box::new(|_ctx| {
        Box::pin(async move { Ok(Some(Content::new("assistant"))) })
    }));

    callbacks.add_before_tool(Box::new(|_ctx| {
        Box::pin(async move { Ok(Some(Content::new("system"))) })
    }));

    callbacks.add_after_tool(Box::new(|_ctx| {
        Box::pin(async move { Ok(Some(Content::new("function"))) })
    }));

    assert_eq!(callbacks.before_model.len(), 1);
    assert_eq!(callbacks.after_model.len(), 1);
    assert_eq!(callbacks.before_tool.len(), 1);
    assert_eq!(callbacks.after_tool.len(), 1);
}
