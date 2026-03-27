//! Bug condition exploration tests for `OpenAIClient` SSE streaming + reasoning content.
//!
//! These tests encode the EXPECTED (correct) behavior for streaming via `OpenAIClient`.
//! They are designed to FAIL on the unfixed code, confirming the bug exists for
//! `OpenAIClient` specifically (which delegates to `OpenAICompatible`).
//!
//! **Property 1 (extended): Bug Condition — OpenAIClient SSE Streaming Produces Incremental Responses**
//! **Validates: Requirements 1.6, 2.9**

#[cfg(feature = "openai")]
mod openai_client_streaming_exploration {
    use adk_core::{Content, Llm, LlmRequest, Part};
    use adk_model::openai::{OpenAIClient, OpenAIConfig};
    use adk_model::retry::RetryConfig;
    use futures::StreamExt;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Helper: create an `OpenAIClient` pointed at the mock server.
    fn make_client(base_url: &str) -> OpenAIClient {
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
    fn make_request() -> LlmRequest {
        LlmRequest::new("o3", vec![Content::new("user").with_text("Hello")])
    }

    // ── Test: OpenAIClient streaming reasoning yields Part::Thinking chunks ──
    //
    // **Validates: Requirements 1.6, 2.9**
    //
    // When `OpenAIClient::generate_content(request, stream=true)` is called with
    // a reasoning model, the SSE stream with `delta.reasoning_content` chunks
    // MUST yield multiple `LlmResponse` items with `Part::Thinking`.
    //
    // On unfixed code, `OpenAIClient` delegates to `OpenAICompatible` which
    // ignores the `stream` parameter — no incremental Part::Thinking chunks
    // are yielded → test FAILS.

    #[tokio::test]
    async fn openai_client_streaming_reasoning_yields_thinking_parts() {
        let server = MockServer::start().await;

        let sse_body = [
            "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"reasoning_content\":\"Let me think\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\" step by step.\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"The answer is 42.\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":10,\"total_tokens\":15}}\n\n",
            "data: [DONE]\n\n",
        ]
        .join("");

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(&sse_body)
                    .insert_header("content-type", "text/event-stream"),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = make_client(&server.uri());
        let request = make_request();

        let stream_result = client.generate_content(request, true).await;
        assert!(stream_result.is_ok(), "generate_content should not error");

        let mut stream = stream_result.unwrap();
        let mut responses = Vec::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(resp) => responses.push(resp),
                Err(e) => panic!("stream yielded error: {e}"),
            }
        }

        // BUG CONDITION: on unfixed code, OpenAIClient delegates to OpenAICompatible
        // which ignores the stream flag. The SSE body is parsed as a single JSON blob
        // (which fails or produces one response), so no incremental Part::Thinking chunks.
        assert!(
            responses.len() >= 2,
            "expected at least 2 LlmResponse items from SSE stream via OpenAIClient, got {}. \
             Counterexample: OpenAIClient with stream=true produces single blob response \
             instead of incremental chunks — bug confirmed.",
            responses.len()
        );

        // Collect all Part::Thinking parts from all responses
        let thinking_parts: Vec<&str> = responses
            .iter()
            .filter_map(|r| r.content.as_ref())
            .flat_map(|c| &c.parts)
            .filter_map(|p| match p {
                Part::Thinking { thinking, .. } => Some(thinking.as_str()),
                _ => None,
            })
            .collect();

        assert!(
            !thinking_parts.is_empty(),
            "expected Part::Thinking responses from delta.reasoning_content chunks via OpenAIClient. \
             Counterexample: no Part::Thinking yielded — OpenAIClient inherits streaming bug — bug confirmed."
        );

        // Verify the thinking content matches what was sent
        let combined_thinking: String = thinking_parts.join("");
        assert!(
            combined_thinking.contains("Let me think"),
            "thinking content should contain the reasoning text"
        );

        // Check intermediate responses have partial=true, turn_complete=false
        for resp in &responses[..responses.len() - 1] {
            assert!(resp.partial, "intermediate response should have partial=true");
            assert!(!resp.turn_complete, "intermediate response should have turn_complete=false");
        }

        // Check final response has partial=false, turn_complete=true
        let final_resp = responses.last().unwrap();
        assert!(!final_resp.partial, "final response should have partial=false");
        assert!(final_resp.turn_complete, "final response should have turn_complete=true");
    }
}
