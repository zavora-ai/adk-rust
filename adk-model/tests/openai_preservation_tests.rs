//! Preservation property tests for the non-streaming path of `OpenAICompatible`.
//!
//! These tests capture the EXISTING baseline behavior of `stream=false` requests.
//! They MUST PASS on the current unfixed code, ensuring the fix does not introduce
//! regressions in the non-streaming path.
//!
//! **Property 3: Preservation — Non-Streaming Path Unchanged**
//! **Validates: Requirements 3.1, 3.2, 3.3, 3.4, 3.7**

#[cfg(feature = "openai")]
mod preservation {
    use adk_core::{Content, ErrorCategory, Llm, LlmRequest, Part};
    use adk_model::openai_compatible::{OpenAICompatible, OpenAICompatibleConfig};
    use adk_model::retry::RetryConfig;
    use async_openai::types::chat::ReasoningEffort;
    use futures::StreamExt;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Helper: create an `OpenAICompatible` client pointed at the mock server
    /// with retries disabled so tests don't hang on error status codes.
    fn make_client(base_url: &str) -> OpenAICompatible {
        let config = OpenAICompatibleConfig::new("test-key", "gpt-4o")
            .with_provider_name("test")
            .with_base_url(base_url);
        OpenAICompatible::new(config)
            .expect("client creation should succeed")
            .with_retry_config(RetryConfig::disabled())
    }

    /// Helper: create a client with organization header set.
    fn make_client_with_org(base_url: &str) -> OpenAICompatible {
        let config = OpenAICompatibleConfig::new("test-key", "gpt-4o")
            .with_provider_name("test")
            .with_base_url(base_url)
            .with_organization("org-test-123");
        OpenAICompatible::new(config)
            .expect("client creation should succeed")
            .with_retry_config(RetryConfig::disabled())
    }

    /// Helper: create a client with reasoning effort set.
    fn make_reasoning_client(base_url: &str) -> OpenAICompatible {
        let config = OpenAICompatibleConfig::new("test-key", "o3")
            .with_provider_name("test")
            .with_base_url(base_url)
            .with_reasoning_effort(ReasoningEffort::Medium);
        OpenAICompatible::new(config)
            .expect("client creation should succeed")
            .with_retry_config(RetryConfig::disabled())
    }

    /// Helper: build a minimal `LlmRequest`.
    fn make_request() -> LlmRequest {
        LlmRequest::new("gpt-4o", vec![Content::new("user").with_text("Hello")])
    }

    // ── Test 3a: Non-streaming text response ────────────────────────────
    //
    // **Validates: Requirements 3.1**
    //
    // `OpenAICompatible::generate_content(request, stream=false)` with a mock
    // server returning a standard JSON response produces a single `LlmResponse`
    // with `partial: false`, `turn_complete: true`, and text content via
    // `from_raw_openai_response()`.

    #[tokio::test]
    async fn non_streaming_text_response() {
        let server = MockServer::start().await;

        let body = serde_json::json!({
            "id": "chatcmpl-1",
            "object": "chat.completion",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "Hello there!" },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 5, "completion_tokens": 3, "total_tokens": 8 }
        });

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .expect(1)
            .mount(&server)
            .await;

        let client = make_client(&server.uri());
        let request = make_request();

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
            matches!(&content.parts[0], Part::Text { text } if text == "Hello there!"),
            "expected Part::Text with 'Hello there!'"
        );

        // Verify usage metadata
        let usage = resp.usage_metadata.as_ref().expect("response should have usage");
        assert_eq!(usage.prompt_token_count, 5);
        assert_eq!(usage.candidates_token_count, 3);
        assert_eq!(usage.total_token_count, 8);
    }

    // ── Test 3b: Non-streaming tool calls ───────────────────────────────
    //
    // **Validates: Requirements 3.2**
    //
    // `OpenAICompatible::generate_content(request, stream=false)` with tools
    // returns `Part::FunctionCall` parts correctly.

    #[tokio::test]
    async fn non_streaming_tool_calls() {
        let server = MockServer::start().await;

        let body = serde_json::json!({
            "id": "chatcmpl-2",
            "object": "chat.completion",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [
                        {
                            "id": "call_abc123",
                            "type": "function",
                            "function": {
                                "name": "get_weather",
                                "arguments": "{\"city\":\"Paris\"}"
                            }
                        },
                        {
                            "id": "call_def456",
                            "type": "function",
                            "function": {
                                "name": "get_time",
                                "arguments": "{\"timezone\":\"UTC\"}"
                            }
                        }
                    ]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 20, "total_tokens": 30 }
        });

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .expect(1)
            .mount(&server)
            .await;

        let client = make_client(&server.uri());
        let mut request = make_request();
        request.tools.insert(
            "get_weather".to_string(),
            serde_json::json!({
                "description": "Get weather",
                "parameters": { "type": "object", "properties": { "city": { "type": "string" } } }
            }),
        );
        request.tools.insert(
            "get_time".to_string(),
            serde_json::json!({
                "description": "Get time",
                "parameters": { "type": "object", "properties": { "timezone": { "type": "string" } } }
            }),
        );

        let mut stream =
            client.generate_content(request, false).await.expect("generate_content should succeed");

        let mut responses = Vec::new();
        while let Some(item) = stream.next().await {
            responses.push(item.expect("stream item should be Ok"));
        }

        assert_eq!(responses.len(), 1, "non-streaming should yield exactly 1 response");

        let resp = &responses[0];
        assert!(!resp.partial);
        assert!(resp.turn_complete);

        let content = resp.content.as_ref().expect("response should have content");

        // Collect function calls
        let func_calls: Vec<_> = content
            .parts
            .iter()
            .filter_map(|p| match p {
                Part::FunctionCall { name, args, id, .. } => Some((name, args, id)),
                _ => None,
            })
            .collect();

        assert_eq!(func_calls.len(), 2, "should have 2 function calls");

        // Verify first tool call
        assert_eq!(func_calls[0].0, "get_weather");
        assert_eq!(func_calls[0].1["city"], "Paris");
        assert_eq!(func_calls[0].2.as_deref(), Some("call_abc123"));

        // Verify second tool call
        assert_eq!(func_calls[1].0, "get_time");
        assert_eq!(func_calls[1].1["timezone"], "UTC");
        assert_eq!(func_calls[1].2.as_deref(), Some("call_def456"));
    }

    // ── Test 3c: Non-streaming reasoning model ──────────────────────────
    //
    // **Validates: Requirements 3.1, 3.2**
    //
    // `OpenAICompatible::generate_content(request, stream=false)` with a
    // reasoning model returns `Part::Thinking` + `Part::Text` correctly
    // (this already works via `from_raw_openai_response`).

    #[tokio::test]
    async fn non_streaming_reasoning_model() {
        let server = MockServer::start().await;

        let body = serde_json::json!({
            "id": "chatcmpl-3",
            "object": "chat.completion",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "reasoning_content": "Let me think step by step...",
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
        let request = make_request();

        let mut stream =
            client.generate_content(request, false).await.expect("generate_content should succeed");

        let mut responses = Vec::new();
        while let Some(item) = stream.next().await {
            responses.push(item.expect("stream item should be Ok"));
        }

        assert_eq!(responses.len(), 1, "non-streaming should yield exactly 1 response");

        let resp = &responses[0];
        assert!(!resp.partial);
        assert!(resp.turn_complete);

        let content = resp.content.as_ref().expect("response should have content");
        assert_eq!(content.parts.len(), 2, "should have Thinking + Text parts");

        // First part: Thinking
        assert!(
            matches!(&content.parts[0], Part::Thinking { thinking, .. } if thinking == "Let me think step by step..."),
            "first part should be Part::Thinking"
        );

        // Second part: Text
        assert!(
            matches!(&content.parts[1], Part::Text { text } if text == "The answer is 42."),
            "second part should be Part::Text"
        );

        // Verify thinking token count
        let usage = resp.usage_metadata.as_ref().expect("should have usage");
        assert_eq!(usage.thinking_token_count, Some(40));
    }

    // ── Test 3d: HTTP error status codes ────────────────────────────────
    //
    // **Validates: Requirements 3.3**
    //
    // HTTP error status codes (401, 403, 404, 429, 503, 500) produce correct
    // `AdkError` with matching `ErrorCategory`.

    #[tokio::test]
    async fn http_error_401_unauthorized() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(401).set_body_string("Unauthorized"))
            .expect(1)
            .mount(&server)
            .await;

        let client = make_client(&server.uri());
        let request = make_request();

        let mut stream = client
            .generate_content(request, false)
            .await
            .expect("generate_content returns a stream");

        let err =
            stream.next().await.expect("should yield an item").expect_err("should be an error");
        assert_eq!(err.category, ErrorCategory::Unauthorized, "401 should map to Unauthorized");
    }

    #[tokio::test]
    async fn http_error_403_forbidden() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(403).set_body_string("Forbidden"))
            .expect(1)
            .mount(&server)
            .await;

        let client = make_client(&server.uri());
        let request = make_request();

        let mut stream = client
            .generate_content(request, false)
            .await
            .expect("generate_content returns a stream");

        let err =
            stream.next().await.expect("should yield an item").expect_err("should be an error");
        assert_eq!(err.category, ErrorCategory::Forbidden, "403 should map to Forbidden");
    }

    #[tokio::test]
    async fn http_error_404_not_found() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(404).set_body_string("Not Found"))
            .expect(1)
            .mount(&server)
            .await;

        let client = make_client(&server.uri());
        let request = make_request();

        let mut stream = client
            .generate_content(request, false)
            .await
            .expect("generate_content returns a stream");

        let err =
            stream.next().await.expect("should yield an item").expect_err("should be an error");
        assert_eq!(err.category, ErrorCategory::NotFound, "404 should map to NotFound");
    }

    #[tokio::test]
    async fn http_error_429_rate_limited() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(429).set_body_string("Rate limited"))
            .expect(1)
            .mount(&server)
            .await;

        let client = make_client(&server.uri());
        let request = make_request();

        let mut stream = client
            .generate_content(request, false)
            .await
            .expect("generate_content returns a stream");

        let err =
            stream.next().await.expect("should yield an item").expect_err("should be an error");
        assert_eq!(err.category, ErrorCategory::RateLimited, "429 should map to RateLimited");
    }

    #[tokio::test]
    async fn http_error_503_unavailable() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(503).set_body_string("Service Unavailable"))
            .expect(1)
            .mount(&server)
            .await;

        let client = make_client(&server.uri());
        let request = make_request();

        let mut stream = client
            .generate_content(request, false)
            .await
            .expect("generate_content returns a stream");

        let err =
            stream.next().await.expect("should yield an item").expect_err("should be an error");
        assert_eq!(err.category, ErrorCategory::Unavailable, "503 should map to Unavailable");
    }

    #[tokio::test]
    async fn http_error_500_internal() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(500).set_body_string("Internal Server Error"))
            .expect(1)
            .mount(&server)
            .await;

        let client = make_client(&server.uri());
        let request = make_request();

        let mut stream = client
            .generate_content(request, false)
            .await
            .expect("generate_content returns a stream");

        let err =
            stream.next().await.expect("should yield an item").expect_err("should be an error");
        assert_eq!(err.category, ErrorCategory::Internal, "500 should map to Internal");
    }

    // ── Test 3e: Request parameters preserved ───────────────────────────
    //
    // **Validates: Requirements 3.7**
    //
    // Request body contains correct parameters (messages, tools,
    // reasoning_effort, temperature, top_p, max_completion_tokens,
    // response_format, organization header) regardless of stream flag.

    #[tokio::test]
    async fn request_parameters_preserved_with_config() {
        let server = MockServer::start().await;

        let body = serde_json::json!({
            "id": "chatcmpl-4",
            "object": "chat.completion",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "ok" },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 }
        });

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .expect(1)
            .mount(&server)
            .await;

        let client = make_reasoning_client(&server.uri());

        let mut request = LlmRequest::new("o3", vec![Content::new("user").with_text("test")]);
        request.config = Some(adk_core::GenerateContentConfig {
            temperature: Some(0.5),
            top_p: Some(0.9),
            max_output_tokens: Some(1024),
            ..Default::default()
        });
        request.tools.insert(
            "my_tool".to_string(),
            serde_json::json!({
                "description": "A test tool",
                "parameters": { "type": "object", "properties": { "x": { "type": "string" } } }
            }),
        );

        // Call with stream=false
        let mut stream =
            client.generate_content(request, false).await.expect("generate_content should succeed");

        // Consume the stream
        while (stream.next().await).is_some() {}

        // Inspect the captured request
        let received = server.received_requests().await.unwrap();
        assert_eq!(received.len(), 1);

        let req_body: serde_json::Value =
            serde_json::from_slice(&received[0].body).expect("request body should be valid JSON");

        // Verify model
        assert_eq!(req_body["model"], "o3", "model should be o3");

        // Verify messages present
        let messages = req_body["messages"].as_array().expect("messages should be an array");
        assert!(!messages.is_empty(), "messages should not be empty");

        // Verify tools present
        let tools = req_body["tools"].as_array().expect("tools should be an array");
        assert_eq!(tools.len(), 1, "should have 1 tool");
        let tool_fn = &tools[0]["function"];
        assert_eq!(tool_fn["name"], "my_tool");

        // Verify reasoning_effort
        assert_eq!(req_body["reasoning_effort"], "medium", "reasoning_effort should be medium");

        // Verify temperature
        assert_eq!(req_body["temperature"], 0.5, "temperature should be 0.5");

        // Verify top_p (f32 precision: 0.9_f64 → 0.9_f32 → ~0.8999999761581421)
        let top_p_val = req_body["top_p"].as_f64().expect("top_p should be a number");
        assert!(
            (top_p_val - 0.9).abs() < 0.001,
            "top_p should be approximately 0.9, got {top_p_val}"
        );

        // Verify max_completion_tokens
        assert_eq!(req_body["max_completion_tokens"], 1024, "max_completion_tokens should be 1024");
    }

    #[tokio::test]
    async fn request_parameters_organization_header() {
        let server = MockServer::start().await;

        let body = serde_json::json!({
            "id": "chatcmpl-5",
            "object": "chat.completion",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "ok" },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 }
        });

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .expect(1)
            .mount(&server)
            .await;

        let client = make_client_with_org(&server.uri());
        let request = make_request();

        let mut stream =
            client.generate_content(request, false).await.expect("generate_content should succeed");

        // Consume the stream
        while (stream.next().await).is_some() {}

        // Inspect the captured request headers
        let received = server.received_requests().await.unwrap();
        assert_eq!(received.len(), 1);

        let org_header = received[0]
            .headers
            .get("OpenAI-Organization")
            .expect("OpenAI-Organization header should be present");
        assert_eq!(
            org_header.to_str().unwrap(),
            "org-test-123",
            "organization header should match"
        );
    }

    #[tokio::test]
    async fn request_parameters_response_format() {
        let server = MockServer::start().await;

        let body = serde_json::json!({
            "id": "chatcmpl-6",
            "object": "chat.completion",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "{\"name\":\"test\"}" },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 }
        });

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .expect(1)
            .mount(&server)
            .await;

        let client = make_client(&server.uri());

        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" }
            },
            "required": ["name"]
        });

        let request = LlmRequest::new("gpt-4o", vec![Content::new("user").with_text("test")])
            .with_response_schema(schema);

        let mut stream =
            client.generate_content(request, false).await.expect("generate_content should succeed");

        // Consume the stream
        while (stream.next().await).is_some() {}

        // Inspect the captured request
        let received = server.received_requests().await.unwrap();
        assert_eq!(received.len(), 1);

        let req_body: serde_json::Value =
            serde_json::from_slice(&received[0].body).expect("request body should be valid JSON");

        // Verify response_format is present and is json_schema type
        let response_format = &req_body["response_format"];
        assert_eq!(
            response_format["type"], "json_schema",
            "response_format type should be json_schema"
        );
        assert!(response_format["json_schema"].is_object(), "json_schema should be present");
        assert!(response_format["json_schema"]["strict"] == true, "strict should be true");
    }
}
