mod test_context;

use adk_agent::LlmAgentBuilder;
use adk_core::{
    Agent, Content, Llm, LlmRequest, LlmResponse, LlmResponseStream, Part, StreamingMode,
};
use async_trait::async_trait;
use futures::StreamExt;
use std::sync::{Arc, Mutex};
use test_context::TestContext;

struct Model {
    requests: Arc<Mutex<Vec<LlmRequest>>>,
    endless: bool,
    complete: bool,
}

#[async_trait]
impl Llm for Model {
    fn name(&self) -> &str {
        "continuation-fixture"
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        _: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        let mut requests = self.requests.lock().unwrap();
        requests.push(request);
        let paused = self.endless || requests.len() == 1;
        let mut content = Content {
            role: "model".into(),
            parts: vec![Part::ServerToolCall {
                server_tool_call: serde_json::json!({"type":"search", "id":"search-fixture"}),
            }],
        };
        if self.complete {
            content.parts.insert(0, Part::Text { text: "Partial text".into() });
        }
        Ok(Box::pin(futures::stream::iter([
            Ok(LlmResponse {
                content: Some(Content::new("model").with_text("Partial text")),
                partial: true,
                ..Default::default()
            }),
            Ok(LlmResponse {
                content: Some(content),
                turn_complete: !paused,
                provider_metadata: Some(
                    serde_json::json!({"continue_turn":paused, "content_complete":self.complete}),
                ),
                ..Default::default()
            }),
        ])))
    }
}

#[tokio::test]
async fn retains_text_and_server_parts_before_continuing() {
    for mode in [StreamingMode::None, StreamingMode::SSE, StreamingMode::Bidi] {
        for complete in [false, true] {
            let requests = Arc::new(Mutex::new(Vec::new()));
            let agent = LlmAgentBuilder::new("assistant")
                .model(Arc::new(Model { requests: requests.clone(), endless: false, complete }))
                .max_iterations(2)
                .build()
                .unwrap();
            let events = agent
                .run(Arc::new(TestContext::new("Search").with_streaming_mode(mode)))
                .await
                .unwrap()
                .collect::<Vec<_>>()
                .await
                .into_iter()
                .collect::<adk_core::Result<Vec<_>>>()
                .unwrap();
            let terminal: Vec<_> =
                events.iter().filter(|event| !event.llm_response.partial).collect();
            assert_eq!(terminal.len(), 2);
            assert!(!terminal[0].llm_response.turn_complete);
            assert!(terminal[1].llm_response.turn_complete);
            let content = terminal[0].llm_response.content.as_ref().unwrap();
            assert_eq!(content.parts.len(), 2);
            assert!(matches!(&content.parts[0], Part::Text { text } if text == "Partial text"));
            let requests = requests.lock().unwrap();
            assert_eq!(requests.len(), 2);
            assert_eq!(requests[1].contents.last().unwrap().parts, content.parts);
        }
    }
}

#[tokio::test]
async fn stops_repeated_pauses_at_the_agent_iteration_limit() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let agent = LlmAgentBuilder::new("assistant")
        .model(Arc::new(Model { requests: requests.clone(), endless: true, complete: true }))
        .max_iterations(2)
        .build()
        .unwrap();
    let events =
        agent.run(Arc::new(TestContext::new("Search"))).await.unwrap().collect::<Vec<_>>().await;
    assert!(events.last().unwrap().is_err());
    assert_eq!(requests.lock().unwrap().len(), 2);
}
