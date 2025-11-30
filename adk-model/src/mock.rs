use adk_core::{Llm, LlmRequest, LlmResponse, LlmResponseStream, Result};
use async_trait::async_trait;

pub struct MockLlm {
    name: String,
    responses: Vec<LlmResponse>,
}

impl MockLlm {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into(), responses: vec![] }
    }

    pub fn with_response(mut self, response: LlmResponse) -> Self {
        self.responses.push(response);
        self
    }
}

#[async_trait]
impl Llm for MockLlm {
    fn name(&self) -> &str {
        &self.name
    }

    async fn generate_content(&self, _req: LlmRequest, _stream: bool) -> Result<LlmResponseStream> {
        let responses = self.responses.clone();
        let stream = async_stream::stream! {
            for response in responses {
                yield Ok(response);
            }
        };
        Ok(Box::pin(stream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::Content;

    #[test]
    fn test_mock_llm() {
        let mock =
            MockLlm::new("test-llm").with_response(LlmResponse::new(Content::new("assistant")));
        assert_eq!(mock.name(), "test-llm");
        assert_eq!(mock.responses.len(), 1);
    }

    #[tokio::test]
    async fn test_mock_llm_generate() {
        use futures::StreamExt;

        let mock = MockLlm::new("test")
            .with_response(LlmResponse::new(Content::new("assistant").with_text("Hello")));

        let req = LlmRequest::new("test", vec![]);
        let mut stream = mock.generate_content(req, false).await.unwrap();

        let response = stream.next().await.unwrap().unwrap();
        assert!(response.content.is_some());
    }
}
