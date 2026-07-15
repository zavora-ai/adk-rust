use adk_realtime::error::RealtimeError;
use adk_realtime::events::ServerEvent;
use serde_json::{Value, json};
use tokio::sync::Mutex;

#[cfg(feature = "openai")]
mod openai_tests {
    use super::*;
    use adk_realtime::openai::protocol::OpenAITransportLink;
    use async_trait::async_trait;

    struct MockTransport {
        events: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl OpenAITransportLink for MockTransport {
        fn session_id(&self) -> &str {
            "mock"
        }
        fn is_connected(&self) -> bool {
            true
        }
        async fn send_raw(&self, _v: &Value) -> adk_realtime::error::Result<()> {
            Ok(())
        }
        async fn close(&self) -> adk_realtime::error::Result<()> {
            Ok(())
        }
        async fn receive_raw(&self) -> Option<adk_realtime::error::Result<ServerEvent>> {
            let mut events = self.events.lock().await;
            if events.is_empty() {
                return None;
            }
            let text = events.remove(0);

            // This mirrors the logic in OpenAIRealtimeSession::receive_raw
            match serde_json::from_str::<ServerEvent>(&text) {
                Ok(mut event) => {
                    if let ServerEvent::FunctionCallDone { arguments, name, .. } = &mut event {
                        if let Value::String(s) = arguments {
                            match serde_json::from_str::<Value>(s) {
                                Ok(parsed) => {
                                    *arguments = parsed;
                                }
                                Err(e) => {
                                    return Some(Err(RealtimeError::protocol(format!(
                                        "malformed function arguments for {}: {}",
                                        name, e
                                    ))));
                                }
                            }
                        }
                    }
                    Some(Ok(event))
                }
                Err(_) => Some(Ok(ServerEvent::Unknown)),
            }
        }
    }

    #[tokio::test]
    async fn test_openai_argument_normalization() {
        let transport = MockTransport {
            events: Mutex::new(vec![
                json!({
                    "type": "response.function_call_arguments.done",
                    "event_id": "evt_1",
                    "response_id": "resp_1",
                    "item_id": "item_1",
                    "output_index": 0,
                    "call_id": "call_1",
                    "name": "test_tool",
                    "arguments": "{\"key\": \"value\"}"
                })
                .to_string(),
            ]),
        };

        let event = transport.receive_raw().await.unwrap().unwrap();
        if let ServerEvent::FunctionCallDone { arguments, .. } = event {
            assert_eq!(arguments, json!({"key": "value"}));
        } else {
            panic!("Expected FunctionCallDone");
        }
    }

    #[tokio::test]
    async fn test_openai_malformed_argument_normalization() {
        let transport = MockTransport {
            events: Mutex::new(vec![
                json!({
                    "type": "response.function_call_arguments.done",
                    "event_id": "evt_1",
                    "response_id": "resp_1",
                    "item_id": "item_1",
                    "output_index": 0,
                    "call_id": "call_1",
                    "name": "test_tool",
                    "arguments": "{\"key\": \"value\"" // Malformed JSON
                })
                .to_string(),
            ]),
        };

        let result = transport.receive_raw().await.unwrap();
        match result {
            Err(RealtimeError::Protocol(msg)) => {
                assert!(msg.contains("malformed function arguments"));
            }
            _ => panic!("Expected Protocol error, got {:?}", result),
        }
    }
}

#[cfg(feature = "openai")]
#[tokio::test]
async fn test_transfer_to_agent_validation() {
    use adk_core::{
        Agent, CallbackContext, Content, EventStream, InvocationContext, ReadonlyContext, RunConfig,
    };
    use adk_realtime::session::BoxedSession;
    use adk_realtime::{RealtimeAgent, RealtimeConfig, audio::AudioFormat};
    use futures::StreamExt;
    use std::sync::Arc;

    #[derive(Debug)]
    struct MockModel;
    #[async_trait::async_trait]
    impl adk_realtime::model::RealtimeModel for MockModel {
        fn provider(&self) -> &str {
            "mock"
        }
        fn model_id(&self) -> &str {
            "mock"
        }
        fn supported_input_formats(&self) -> Vec<AudioFormat> {
            vec![]
        }
        fn supported_output_formats(&self) -> Vec<AudioFormat> {
            vec![]
        }
        fn available_voices(&self) -> Vec<&str> {
            vec![]
        }
        async fn connect(
            &self,
            _config: RealtimeConfig,
        ) -> adk_realtime::error::Result<BoxedSession> {
            Ok(Box::new(MockSession))
        }
    }

    struct MockSession;
    #[async_trait::async_trait]
    impl adk_realtime::session::RealtimeSession for MockSession {
        fn session_id(&self) -> &str {
            "mock"
        }
        fn is_connected(&self) -> bool {
            true
        }
        async fn send_audio(
            &self,
            _a: &adk_realtime::audio::AudioChunk,
        ) -> adk_realtime::error::Result<()> {
            Ok(())
        }
        async fn send_audio_base64(&self, _a: &str) -> adk_realtime::error::Result<()> {
            Ok(())
        }
        async fn send_text(&self, _t: &str) -> adk_realtime::error::Result<()> {
            Ok(())
        }
        async fn send_tool_response(
            &self,
            _r: adk_realtime::events::ToolResponse,
        ) -> adk_realtime::error::Result<()> {
            Ok(())
        }
        async fn commit_audio(&self) -> adk_realtime::error::Result<()> {
            Ok(())
        }
        async fn clear_audio(&self) -> adk_realtime::error::Result<()> {
            Ok(())
        }
        async fn create_response(&self) -> adk_realtime::error::Result<()> {
            Ok(())
        }
        async fn interrupt(&self) -> adk_realtime::error::Result<()> {
            Ok(())
        }
        async fn send_event(
            &self,
            _e: adk_realtime::events::ClientEvent,
        ) -> adk_realtime::error::Result<()> {
            Ok(())
        }
        async fn next_event(&self) -> Option<adk_realtime::error::Result<ServerEvent>> {
            Some(Ok(ServerEvent::FunctionCallDone {
                event_id: "evt_1".into(),
                response_id: "resp_1".into(),
                item_id: "item_1".into(),
                output_index: 0,
                call_id: "call_1".into(),
                name: "transfer_to_agent".into(),
                arguments: json!({"agent_name": ""}), // Empty agent_name
            }))
        }
        fn events(
            &self,
        ) -> std::pin::Pin<
            Box<dyn futures::Stream<Item = adk_realtime::error::Result<ServerEvent>> + Send + '_>,
        > {
            Box::pin(futures::stream::empty())
        }
        async fn close(&self) -> adk_realtime::error::Result<()> {
            Ok(())
        }
        async fn mutate_context(
            &self,
            _c: RealtimeConfig,
        ) -> adk_realtime::error::Result<adk_realtime::session::ContextMutationOutcome> {
            Ok(adk_realtime::session::ContextMutationOutcome::Applied)
        }
    }

    struct MockInvocationContext {
        content: Content,
        agent: Arc<dyn Agent>,
    }
    #[async_trait::async_trait]
    impl ReadonlyContext for MockInvocationContext {
        fn invocation_id(&self) -> &str {
            "inv_1"
        }
        fn agent_name(&self) -> &str {
            "test"
        }
        fn user_id(&self) -> &str {
            "user_1"
        }
        fn app_name(&self) -> &str {
            "app"
        }
        fn session_id(&self) -> &str {
            "sess_1"
        }
        fn branch(&self) -> &str {
            ""
        }
        fn user_content(&self) -> &Content {
            &self.content
        }
    }
    #[async_trait::async_trait]
    impl CallbackContext for MockInvocationContext {
        fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
            None
        }
    }
    #[async_trait::async_trait]
    impl InvocationContext for MockInvocationContext {
        fn memory(&self) -> Option<Arc<dyn adk_core::Memory>> {
            None
        }
        fn agent(&self) -> Arc<dyn Agent> {
            self.agent.clone()
        }
        fn session(&self) -> &dyn adk_core::Session {
            todo!()
        }
        fn run_config(&self) -> &RunConfig {
            todo!()
        }
        fn end_invocation(&self) {}
        fn ended(&self) -> bool {
            false
        }
    }

    let sub_agent = RealtimeAgent::builder("sub").model(Arc::new(MockModel)).build().unwrap();
    let agent = RealtimeAgent::builder("test")
        .model(Arc::new(MockModel))
        .sub_agent(Arc::new(sub_agent))
        .build()
        .unwrap();

    let ctx = MockInvocationContext {
        content: Content { role: "user".into(), parts: vec![] },
        agent: Arc::new(RealtimeAgent::builder("test").model(Arc::new(MockModel)).build().unwrap()),
    };
    let mut stream = agent.run(Arc::new(ctx)).await.unwrap();

    // First event is session started
    let _ = stream.next().await.unwrap().unwrap();

    // Second event should be the error because of empty agent_name
    let result = stream.next().await.unwrap();
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("transfer_to_agent called with missing or empty 'agent_name'")
    );
}
