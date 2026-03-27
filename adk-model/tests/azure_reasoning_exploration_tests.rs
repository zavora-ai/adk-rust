//! Bug condition exploration test for Azure OpenAI reasoning content.
//!
//! This test encodes the EXPECTED (correct) behavior: when `AzureOpenAIClient`
//! receives a response containing `reasoning_content`, the `LlmResponse` should
//! include a `Part::Thinking` part.
//!
//! On UNFIXED code this test MUST FAIL because `AzureOpenAIClient` uses
//! `async-openai`'s typed `CreateChatCompletionResponse` which has no
//! `reasoning_content` field — the reasoning output is silently discarded by
//! `from_openai_response()`.
//!
//! **Validates: Requirements 1.5, 2.8**

#[cfg(feature = "openai")]
mod azure_reasoning_exploration {
    use adk_core::{Content, Llm, LlmRequest, Part};
    use adk_model::openai::{AzureConfig, AzureOpenAIClient};
    use futures::StreamExt;
    use wiremock::matchers::{method, path_regex};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Helper: build a minimal `LlmRequest`.
    fn make_request() -> LlmRequest {
        LlmRequest::new("o3", vec![Content::new("user").with_text("What is 2+2?")])
    }

    // ── Test: Azure reasoning_content extraction ────────────────────────
    //
    // **Validates: Requirements 1.5, 2.8**
    //
    // When `AzureOpenAIClient::generate_content()` receives a response from
    // a reasoning model that includes `reasoning_content`, the `LlmResponse`
    // MUST include a `Part::Thinking` part preserving the reasoning text.
    //
    // On unfixed code, `from_openai_response()` parses the typed
    // `CreateChatCompletionResponse` which has no `reasoning_content` field,
    // so reasoning output is silently discarded → test FAILS.

    #[tokio::test]
    async fn azure_reasoning_content_yields_thinking_part() {
        let server = MockServer::start().await;

        // The Azure OpenAI API response includes `reasoning_content` for
        // reasoning models like o3, o4-mini, gpt-5-mini.
        // `async-openai`'s typed `CreateChatCompletionResponse` does NOT have
        // this field, so it gets dropped during deserialization.
        let response_body = serde_json::json!({
            "id": "chatcmpl-azure-1",
            "object": "chat.completion",
            "created": 1700000000,
            "model": "o3",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "reasoning_content": "Let me think... 2+2 equals 4 because addition is commutative.",
                    "content": "The answer is 4."
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 50,
                "total_tokens": 60,
                "completion_tokens_details": {
                    "reasoning_tokens": 40
                }
            }
        });

        // Azure URL pattern: {api_base}/openai/deployments/{deployment_id}/chat/completions
        // The mock server needs to match this path pattern.
        Mock::given(method("POST"))
            .and(path_regex(r"/openai/deployments/.+/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&response_body))
            .expect(1)
            .mount(&server)
            .await;

        // Create an AzureOpenAIClient pointed at the mock server.
        let config = AzureConfig::new(
            "test-azure-key",
            server.uri(),         // api_base → mock server
            "2024-12-01-preview", // api_version
            "o3",                 // deployment_id
        );
        let client =
            AzureOpenAIClient::new(config).expect("AzureOpenAIClient creation should succeed");

        let request = make_request();

        // Call generate_content (stream=false — Azure always uses non-streaming internally)
        let stream_result = client.generate_content(request, false).await;
        assert!(
            stream_result.is_ok(),
            "generate_content should not error: {:?}",
            stream_result.err()
        );

        let mut stream = stream_result.unwrap();
        let mut responses = Vec::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(resp) => responses.push(resp),
                Err(e) => panic!("stream yielded error: {e}"),
            }
        }

        assert!(!responses.is_empty(), "expected at least one LlmResponse from Azure client");

        // Collect all parts from all responses
        let all_parts: Vec<&Part> =
            responses.iter().filter_map(|r| r.content.as_ref()).flat_map(|c| &c.parts).collect();

        // Check for Part::Thinking
        let thinking_parts: Vec<&str> = all_parts
            .iter()
            .filter_map(|p| match p {
                Part::Thinking { thinking, .. } => Some(thinking.as_str()),
                _ => None,
            })
            .collect();

        // BUG CONDITION: on unfixed code, `from_openai_response()` parses the
        // typed `CreateChatCompletionResponse` which has no `reasoning_content`
        // field. The reasoning output is silently discarded.
        //
        // Counterexample: response with `reasoning_content: "Let me think..."`
        // produces no `Part::Thinking` — reasoning content silently dropped.
        assert!(
            !thinking_parts.is_empty(),
            "expected Part::Thinking in response when reasoning_content is present. \
             Counterexample: response with reasoning_content: \"Let me think... 2+2 equals 4 \
             because addition is commutative.\" produces no Part::Thinking — \
             reasoning content silently discarded by from_openai_response() — bug confirmed."
        );

        // Verify the thinking content matches what was sent
        let combined_thinking = thinking_parts.join("");
        assert!(
            combined_thinking.contains("Let me think"),
            "Part::Thinking should contain the reasoning text from the response"
        );

        // Also verify that the text content is still present
        let text_parts: Vec<&str> = all_parts
            .iter()
            .filter_map(|p| match p {
                Part::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();

        assert!(!text_parts.is_empty(), "expected Part::Text in response for the visible content");
        assert!(
            text_parts.iter().any(|t| t.contains("The answer is 4")),
            "Part::Text should contain the visible response text"
        );
    }
}
