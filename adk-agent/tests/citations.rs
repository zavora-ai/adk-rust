mod test_context;

use adk_agent::LlmAgentBuilder;
use adk_core::{
    Agent, BeforeModelResult, CitationMetadata, CitationSource, Content, Llm, LlmRequest,
    LlmResponse, LlmResponseStream, StreamingMode,
};
use async_trait::async_trait;
use futures::StreamExt;
use std::sync::Arc;
use test_context::TestContext;

struct Model;

fn response() -> LlmResponse {
    LlmResponse {
        content: Some(Content::new("model").with_text("Result")),
        citation_metadata: Some(CitationMetadata {
            citation_sources: vec![CitationSource {
                uri: Some("https://example.com/source".into()),
                title: Some("Source".into()),
                start_index: Some(0),
                end_index: Some(6),
                license: None,
                publication_date: None,
            }],
        }),
        turn_complete: true,
        ..Default::default()
    }
}

#[async_trait]
impl Llm for Model {
    fn name(&self) -> &str {
        "citation-fixture"
    }

    async fn generate_content(
        &self,
        _: LlmRequest,
        _: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        Ok(Box::pin(futures::stream::iter([Ok(response())])))
    }
}

#[tokio::test]
async fn preserves_sources_in_streamed_and_aggregated_events() {
    for mode in [StreamingMode::None, StreamingMode::SSE, StreamingMode::Bidi] {
        let agent = LlmAgentBuilder::new("assistant").model(Arc::new(Model)).build().unwrap();
        let context = Arc::new(TestContext::new("Find a source").with_streaming_mode(mode));
        let events = agent.run(context).await.unwrap().collect::<Vec<_>>().await;
        let event = events.into_iter().next().unwrap().unwrap();
        assert_eq!(event.llm_response.citation_metadata, response().citation_metadata);
        assert_eq!(event.llm_response.content.unwrap().parts, response().content.unwrap().parts);
    }
}

#[tokio::test]
async fn preserves_sources_when_a_callback_supplies_the_response() {
    let agent = LlmAgentBuilder::new("assistant")
        .model(Arc::new(Model))
        .before_model_callback(Box::new(|_, _| {
            Box::pin(async { Ok(BeforeModelResult::Skip(response())) })
        }))
        .build()
        .unwrap();
    let events = agent
        .run(Arc::new(TestContext::new("Find a source")))
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;
    let event = events.into_iter().next().unwrap().unwrap();
    assert_eq!(event.llm_response.citation_metadata, response().citation_metadata);
}
