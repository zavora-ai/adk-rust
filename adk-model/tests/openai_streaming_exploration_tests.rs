//! Bug condition exploration tests for OpenAI SSE streaming + reasoning content.
//!
//! These tests encode the EXPECTED (correct) behavior for streaming. They are
//! designed to FAIL on the unfixed code, confirming the bug exists.
//!
//! **Validates: Requirements 1.1, 1.2, 1.3, 1.4, 2.1, 2.2, 2.3, 2.4, 2.5, 2.6**

#[cfg(feature = "openai")]
mod streaming_exploration {
    use adk_core::{Content, Llm, LlmRequest, Part};
    use adk_model::openai_compatible::{OpenAICompatible, OpenAICompatibleConfig};
    use futures::StreamExt;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Helper: create an `OpenAICompatible` client pointed at the mock server.
    fn make_client(base_url: &str) -> OpenAICompatible {
        let config = OpenAICompatibleConfig::new("test-key", "gpt-4o")
            .with_provider_name("test")
            .with_base_url(base_url);
        OpenAICompatible::new(config).expect("client creation should succeed")
    }

    /// Helper: build a minimal `LlmRequest`.
    fn make_request() -> LlmRequest {
        LlmRequest::new("gpt-4o", vec![Content::new("user").with_text("Hello")])
    }

    // ── Test 1: stream flag in request body ─────────────────────────────
    //
    // **Validates: Requirements 1.1, 2.1**
    //
    // When `stream=true` is passed to `generate_content()`, the HTTP request
    // body MUST contain `"stream": true`. On unfixed code the `_stream`
    // parameter is ignored, so this field will be absent → test FAILS.

    #[tokio::test]
    async fn stream_flag_present_in_request_body() {
        let server = MockServer::start().await;

        // Return a valid non-streaming response so the call doesn't error out.
        // The important thing is capturing what was SENT.
        let body = serde_json::json!({
            "id": "chatcmpl-1",
            "object": "chat.completion",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "Hi" },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 5, "completion_tokens": 1, "total_tokens": 6 }
        });

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .expect(1)
            .mount(&server)
            .await;

        let client = make_client(&server.uri());
        let request = make_request();

        // Call with stream=true
        let stream_result = client.generate_content(request, true).await;
        assert!(stream_result.is_ok(), "generate_content should not error");

        // Consume the stream
        let mut stream = stream_result.unwrap();
        while (stream.next().await).is_some() {}

        // Inspect the captured request
        let received = server.received_requests().await.unwrap();
        assert_eq!(received.len(), 1, "expected exactly one request");

        let req_body: serde_json::Value =
            serde_json::from_slice(&received[0].body).expect("request body should be valid JSON");

        // BUG CONDITION: on unfixed code, "stream" field is absent or false
        assert_eq!(
            req_body.get("stream"),
            Some(&serde_json::json!(true)),
            "request body must contain \"stream\": true when stream=true is passed. \
             Counterexample: request body has no stream field — bug confirmed."
        );
    }

    // ── Test 2: SSE response yields multiple LlmResponse items ──────────
    //
    // **Validates: Requirements 1.2, 1.3, 2.2, 2.3, 2.5**
    //
    // When the server returns an SSE stream with multiple `data:` lines
    // containing `delta.content` chunks, the client MUST yield multiple
    // `LlmResponse` items. On unfixed code, the entire response is parsed
    // as a single JSON blob → only one response yielded → test FAILS.

    #[tokio::test]
    async fn sse_stream_yields_multiple_responses() {
        let server = MockServer::start().await;

        // Build an SSE response with multiple chunks
        let sse_body = [
            "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"Hello\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\" world\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":2,\"total_tokens\":7}}\n\n",
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

        // BUG CONDITION: on unfixed code, only 1 response is yielded (the
        // entire SSE body is parsed as a single JSON blob, which fails or
        // produces one response).
        assert!(
            responses.len() >= 2,
            "expected at least 2 LlmResponse items from SSE stream, got {}. \
             Counterexample: single blob response instead of incremental chunks — bug confirmed.",
            responses.len()
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

    // ── Test 3: reasoning_content chunks yield Part::Thinking ────────────
    //
    // **Validates: Requirements 1.2, 2.2**
    //
    // When the SSE stream contains `delta.reasoning_content` chunks, the
    // client MUST yield `LlmResponse` items with `Part::Thinking`. On
    // unfixed code there is no SSE processing → no Part::Thinking from
    // streaming → test FAILS.

    #[tokio::test]
    async fn reasoning_content_chunks_yield_thinking_parts() {
        let server = MockServer::start().await;

        let sse_body = [
            "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"reasoning_content\":\"Let me think...\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\" about this.\"},\"finish_reason\":null}]}\n\n",
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

        // BUG CONDITION: on unfixed code, no SSE processing occurs, so no
        // Part::Thinking is yielded from streaming delta chunks.
        assert!(
            !thinking_parts.is_empty(),
            "expected Part::Thinking responses from delta.reasoning_content chunks. \
             Counterexample: no Part::Thinking yielded — reasoning content not streamed — bug confirmed."
        );

        // Verify the thinking content matches what was sent
        let combined_thinking: String = thinking_parts.join("");
        assert!(
            combined_thinking.contains("Let me think"),
            "thinking content should contain the reasoning text"
        );
    }

    // ── Test 4: tool_calls chunks accumulated and yielded on finish ──────
    //
    // **Validates: Requirements 1.4, 2.4, 2.5**
    //
    // When the SSE stream contains `delta.tool_calls` chunks, the client
    // MUST accumulate them by index and yield `Part::FunctionCall` on
    // `finish_reason`. On unfixed code there is no SSE processing → test FAILS.

    #[tokio::test]
    async fn tool_call_chunks_accumulated_and_yielded_on_finish() {
        let server = MockServer::start().await;

        let sse_body = [
            // First chunk: tool call index 0, id + function name
            "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"tool_calls\":[{\"index\":0,\"id\":\"call_abc\",\"type\":\"function\",\"function\":{\"name\":\"get_weather\",\"arguments\":\"\"}}]},\"finish_reason\":null}]}\n\n",
            // Second chunk: tool call index 0, argument fragment
            "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"city\\\"\"}}]},\"finish_reason\":null}]}\n\n",
            // Third chunk: tool call index 0, more argument fragment
            "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\": \\\"Paris\\\"}\"}}]},\"finish_reason\":null}]}\n\n",
            // Finish chunk
            "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":15,\"total_tokens\":25}}\n\n",
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

        // Collect all Part::FunctionCall parts
        let function_calls: Vec<_> = responses
            .iter()
            .filter_map(|r| r.content.as_ref())
            .flat_map(|c| &c.parts)
            .filter_map(|p| match p {
                Part::FunctionCall { name, args, id, .. } => {
                    Some((name.clone(), args.clone(), id.clone()))
                }
                _ => None,
            })
            .collect();

        // BUG CONDITION: on unfixed code, no SSE processing occurs, so no
        // tool calls are accumulated from streaming chunks.
        assert!(
            !function_calls.is_empty(),
            "expected Part::FunctionCall from accumulated delta.tool_calls chunks. \
             Counterexample: no Part::FunctionCall yielded — tool calls not streamed — bug confirmed."
        );

        // Verify the accumulated tool call
        let (name, args, id) = &function_calls[0];
        assert_eq!(name, "get_weather", "tool call name should be get_weather");
        assert_eq!(args["city"], "Paris", "tool call args should contain city=Paris");
        assert_eq!(id.as_deref(), Some("call_abc"), "tool call id should be call_abc");

        // The response containing the tool call should be the final one
        let final_resp = responses.last().unwrap();
        assert!(!final_resp.partial, "final response with tool calls should have partial=false");
        assert!(
            final_resp.turn_complete,
            "final response with tool calls should have turn_complete=true"
        );
    }
}
