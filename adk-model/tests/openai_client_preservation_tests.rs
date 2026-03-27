//! Preservation property tests for the non-streaming path of `OpenAIClient`.
//!
//! These tests capture the EXISTING baseline behavior of `OpenAIClient` with
//! `stream=false` requests. They MUST PASS on the current unfixed code, ensuring
//! the fix does not introduce regressions in the `OpenAIClient` non-streaming path.
//!
//! **Property 3 (extended): Preservation — OpenAIClient Non-Streaming Path Unchanged**
//! **Validates: Requirements 3.8**

#[cfg(feature = "openai")]
mod openai_client_preservation {
    use adk_core::{Content, Llm, LlmRequest, Part};
    use adk_model::openai::{OpenAIClient, OpenAIConfig};
    use adk_model::retry::RetryConfig;
    use futures::StreamExt;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Helper: create an `OpenAIClient` pointed at the mock server with retries disabled.
    fn make_client(base_url: &str) -> OpenAIClient {
        let config = OpenAIConfig {
            api_key: "test-key".to_string(),
            model: "gpt-4o".to_string(),
            base_url: Some(base_url.to_string()),
            organization_id: None,
            project_id: None,
            reasoning_effort: None,
        };
        OpenAIClient::new(config)
            .expect("client creation should succeed")
            .with_retry_config(RetryConfig::disabled())
    }

    /// Helper: create an `OpenAIClient` configured as a reasoning model.
    fn make_reasoning_client(base_url: &str) -> OpenAIClient {
        let config = OpenAIConfig {
            api_key: "test-key".to_string(),
            model: "o3".to_string(),
            base_url: Some(base_url.to_string()),
            organization_id: None,
            project_id: None,
            reasoning_effort: None,
        };
        OpenAIClient::new(config)
            .expect("client creation should succeed")
            .with_retry_config(RetryConfig::disabled())
    }

    /// Helper: build a minimal `LlmRequest`.
    fn make_request(model: &str) -> LlmRequest {
        LlmRequest::new(model, vec![Content::new("user").with_text("Hello")])
    }

    // ── Test: Non-streaming text response via OpenAIClient ──────────────
    //
    // **Validates: Requirements 3.8**
    //
    // `OpenAIClient::generate_content(request, stream=false)` with a mock
    // server returning a standard JSON response produces a single `LlmResponse`
    // with `partial: false`, `turn_complete: true`, and correct text content.

    #[tokio::test]
    async fn non_streaming_text_response() {
        let server = MockServer::start().await;

        let body = serde_json::json!({
            "id": "chatcmpl-1",
            "object": "chat.completion",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "Hello from OpenAIClient!" },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 5, "completion_tokens": 4, "total_tokens": 9 }
        });

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .expect(1)
            .mount(&server)
            .await;

        let client = make_client(&server.uri());
        let request = make_request("gpt-4o");

        let mut stream =
            client.generate_content(request, false).await.expect("generate_content should succeed");

        let mut responses = Vec::new();
        while let Some(item) = stream.next().await {
            responses.push(item.expect("stream item should be Ok"));
        }

        // Non-streaming yields exactly one response
        assert_eq!(responses.len(), 1, "non-streaming should yield exactly 1 response");

        let resp = &responses[0];
        assert!(!resp.partial, "non-streaming response should have partial=false");
        assert!(resp.turn_complete, "non-streaming response should have turn_complete=true");

        // Verify text content
        let content = resp.content.as_ref().expect("response should have content");
        assert_eq!(content.role, "model");
        assert_eq!(content.parts.len(), 1);
        assert!(
            matches!(&content.parts[0], Part::Text { text } if text == "Hello from OpenAIClient!"),
            "expected Part::Text with 'Hello from OpenAIClient!'"
        );

        // Verify usage metadata
        let usage = resp.usage_metadata.as_ref().expect("response should have usage");
        assert_eq!(usage.prompt_token_count, 5);
        assert_eq!(usage.candidates_token_count, 4);
        assert_eq!(usage.total_token_count, 9);
    }

    // ── Test: Non-streaming reasoning model via OpenAIClient ────────────
    //
    // **Validates: Requirements 3.8**
    //
    // `OpenAIClient::generate_content(request, stream=false)` with a reasoning
    // model returns `Part::Thinking` + `Part::Text` correctly (delegated to
    // `OpenAICompatible` which uses `from_raw_openai_response`).

    #[tokio::test]
    async fn non_streaming_reasoning_model() {
        let server = MockServer::start().await;

        let body = serde_json::json!({
            "id": "chatcmpl-2",
            "object": "chat.completion",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "reasoning_content": "Let me reason through this carefully...",
                    "content": "The answer is 42."
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 50,
                "total_tokens": 60,
                "completion_tokens_details": { "reasoning_tokens": 40 }
            }
        });

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .expect(1)
            .mount(&server)
            .await;

        let client = make_reasoning_client(&server.uri());
        let request = make_request("o3");

        let mut stream =
            client.generate_content(request, false).await.expect("generate_content should succeed");

        let mut responses = Vec::new();
        while let Some(item) = stream.next().await {
            responses.push(item.expect("stream item should be Ok"));
        }

        assert_eq!(responses.len(), 1, "non-streaming should yield exactly 1 response");

        let resp = &responses[0];
        assert!(!resp.partial, "non-streaming response should have partial=false");
        assert!(resp.turn_complete, "non-streaming response should have turn_complete=true");

        let content = resp.content.as_ref().expect("response should have content");
        assert_eq!(content.parts.len(), 2, "should have Thinking + Text parts");

        // First part: Thinking
        assert!(
            matches!(
                &content.parts[0],
                Part::Thinking { thinking, .. } if thinking == "Let me reason through this carefully..."
            ),
            "first part should be Part::Thinking with reasoning content"
        );

        // Second part: Text
        assert!(
            matches!(
                &content.parts[1],
                Part::Text { text } if text == "The answer is 42."
            ),
            "second part should be Part::Text with answer"
        );

        // Verify thinking token count
        let usage = resp.usage_metadata.as_ref().expect("should have usage");
        assert_eq!(usage.thinking_token_count, Some(40));
    }
}
