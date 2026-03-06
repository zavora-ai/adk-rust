use adk_core::{Artifacts, CallbackContext, Content, Part, ReadonlyContext, Role, types::{AdkIdentity, InvocationId}};
use adk_runner::Callbacks;
use async_trait::async_trait;
use std::sync::{Arc, Mutex};

// Mock context for testing
struct MockCallbackContext {
    invocation_id: InvocationId,
    content: Content,
    identity: AdkIdentity,
    metadata: std::collections::HashMap<String, String>,
}

impl MockCallbackContext {
    fn new(id: &str) -> Self {
        Self {
            invocation_id: InvocationId::new(id).unwrap(),
            content: Content::new(Role::User),
            identity: AdkIdentity::default(),
            metadata: std::collections::HashMap::new(),
        }
    }
}

#[async_trait]
impl ReadonlyContext for MockCallbackContext {
    fn invocation_id(&self) -> &InvocationId {
        &self.invocation_id
    }
    fn user_content(&self) -> &Content {
        &self.content
    }
    fn identity(&self) -> &AdkIdentity {
        &self.identity
    }
    fn metadata(&self) -> &std::collections::HashMap<String, String> {
        &self.metadata
    }
}

#[async_trait]
impl Artifacts for MockCallbackContext {
    async fn list(&self) -> adk_core::Result<Vec<String>> {
        Ok(vec![])
    }
    async fn load(&self, _name: &str) -> adk_core::Result<Part> {
        Ok(Part::text("".to_string()))
    }
    async fn save(&self, _name: &str, _part: &Part) -> adk_core::Result<i64> {
        Ok(1)
    }
}

#[async_trait]
impl CallbackContext for MockCallbackContext {
    fn artifacts(&self) -> Option<Arc<dyn Artifacts>> {
        None
    }
}

#[tokio::test]
async fn test_callbacks_execution() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let log_clone = log.clone();

    let callbacks = Callbacks::new()
        .on_event(move |event| {
            let mut l = log_clone.lock().unwrap();
            l.push(event.id.clone());
            Ok(())
        });

    let mut event = adk_core::Event::new(InvocationId::new("inv-1").unwrap());
    event.llm_response.content = Some(Content::new(Role::Model).with_text("hello"));
    
    callbacks.handle_event(&event).unwrap();

    let l = log.lock().unwrap();
    assert_eq!(l.len(), 1);
    assert_eq!(l[0], event.id);
}

#[tokio::test]
async fn test_on_content_callback() {
    let captured = Arc::new(Mutex::new(String::new()));
    let captured_clone = captured.clone();

    let callbacks = Callbacks::new()
        .on_content(move |content| {
            let mut c = captured_clone.lock().unwrap();
            *c = content.text();
            Ok(())
        });

    let mut event = adk_core::Event::new(InvocationId::new("inv-1").unwrap());
    event.llm_response.content = Some(Content::new(Role::Model).with_text("callback data"));
    
    callbacks.handle_event(&event).unwrap();

    let c = captured.lock().unwrap();
    assert_eq!(*c, "callback data");
}
