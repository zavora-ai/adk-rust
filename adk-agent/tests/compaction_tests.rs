use adk_agent::LlmAgent;
use adk_core::model::{Llm, LlmRequest, LlmResponse, LlmResponseStream};
use adk_core::types::{Content, InvocationId, Role};
use adk_core::{BaseEventsSummarizer, Event, EventsCompactionConfig, Result};
use adk_session::{GetRequest, InMemorySessionService, Session, SessionService};
use async_trait::async_trait;
use std::sync::Arc;

struct MockSummarizerLlm {
    summary_text: String,
}

#[async_trait]
impl Llm for MockSummarizerLlm {
    fn name(&self) -> &str {
        "mock-summarizer"
    }
    async fn generate_content(&self, _req: LlmRequest, _stream: bool) -> Result<LlmResponseStream> {
        let response = LlmResponse {
            content: Some(Content::new(Role::Model).with_text(self.summary_text.clone())),
            ..Default::default()
        };
        Ok(Box::pin(async_stream::stream! { yield Ok(response); }))
    }
}

struct SimpleSummarizer {
    llm: Arc<dyn Llm>,
}

#[async_trait]
impl BaseEventsSummarizer for SimpleSummarizer {
    async fn summarize_events(&self, _events: &[Event]) -> Result<Option<Event>> {
        let req = LlmRequest::new("mock", vec![]);
        let mut stream = self.llm.generate_content(req, false).await?;
        let resp = stream.next().await.unwrap()?;
        
        let mut event = Event::new(InvocationId::new("summary").unwrap());
        event.author = "summarizer".to_string();
        event.llm_response = resp;
        Ok(Some(event))
    }
}

#[tokio::test]
async fn test_compaction_logic() {
    let llm = Arc::new(MockSummarizerLlm { summary_text: "summary".to_string() });
    let summarizer = Arc::new(SimpleSummarizer { llm });
    let session_service = Arc::new(InMemorySessionService::new());

    let agent = LlmAgent::new("test", Arc::new(MockSummarizerLlm { summary_text: "hi".to_string() }), vec![]);
    
    // This test is minimal just to verify the types and structure compile
    assert_eq!(agent.name(), "test");
}
