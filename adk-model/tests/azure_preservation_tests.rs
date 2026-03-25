//! Preservation property tests for `AzureOpenAIClient` with non-reasoning models.
//!
//! These tests capture the EXISTING baseline behavior of `AzureOpenAIClient` for
//! responses that do NOT contain `reasoning_content`. They MUST PASS on the current
//! unfixed code, ensuring the fix does not introduce regressions in Azure
//! non-reasoning model behavior.
//!
//! **Property 4: Preservation — Azure Non-Reasoning Models Unchanged**
//! **Validates: Requirements 3.5**

#[cfg(feature = "openai")]
mod azure_preservation {
    use adk_core::{Content, Llm, LlmRequest, Part};
    use adk_model::openai::{AzureConfig, AzureOpenAIClient};
    use adk_model::retry::RetryConfig;
    use futures::StreamExt;
    use wiremock::matchers::{method, path_regex};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Helper: build a minimal `LlmRequest`.
    fn make_request() -> LlmRequest {
        LlmRequest::new("gpt-4o", vec![Content::new("user").with_text("Hello")])
    }

    /// Helper: create an `AzureOpenAIClient` pointed at the mock server with
    /// retries disabled.
    fn make_azure_client(server_uri: &str) -> AzureOpenAIClient {
        let config = AzureConfig::new("test-azure-key", server_uri, "2024-12-01-preview", "gpt-4o");
        AzureOpenAIClient::new(config)
            .expect("AzureOpenAIClient creation should succeed")
            .with_retry_config(RetryConfig::disabled())
    }

    // ── Test 4a: Azure non-reasoning text response ──────────────────────
    //
    // **Validates: Requirements 3.5**
    //
    // `AzureOpenAIClient::generate_content()` with a mock response containing
    // only `content` (no `reasoning_content`) produces `Part::Text` correctly
    // via `from_openai_response()`.

    #[tokio::test]
    async fn azure_non_reasoning_text_response() {
        let server = MockServer::start().await;

        let response_body = serde_json::json!({
            "id": "chatcmpl-azure-text-1",
            "object": "chat.completion",
            "created": 1700000000,
            "model": "gpt-4o",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "Hello! How can I help you today?"
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 8,
                "completion_tokens": 9,
                "total_tokens": 17
            }
        });

        // Azure URL pattern: {api_base}/openai/deployments/{deployment_id}/chat/completions
        Mock::given(method("POST"))
            .and(path_regex(r"/openai/deployments/.+/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&response_body))
            .expect(1)
            .mount(&server)
            .await;

        let client = make_azure_client(&server.uri());
        let request = make_request();

        let mut stream =
            client.generate_content(request, false).await.expect("generate_content should succeed");

        let mut responses = Vec::new();
        while let Some(item) = stream.next().await {
            responses.push(item.expect("stream item should be Ok"));
        }

        // Non-streaming yields exactly one response
        assert_eq!(responses.len(), 1, "should yield exactly 1 response");

        let resp = &responses[0];
        assert!(!resp.partial, "response should have partial=false");
        assert!(resp.turn_complete, "response should have turn_complete=true");

        // Verify text content
        let content = resp.content.as_ref().expect("response should have content");
        assert_eq!(content.role, "model");
        assert_eq!(content.parts.len(), 1, "should have exactly 1 part (text only)");
        assert!(
            matches!(&content.parts[0], Part::Text { text } if text == "Hello! How can I help you today?"),
            "expected Part::Text with correct content"
        );

        // Verify no Thinking parts (non-reasoning model)
        let thinking_count =
            content.parts.iter().filter(|p| matches!(p, Part::Thinking { .. })).count();
        assert_eq!(thinking_count, 0, "non-reasoning model should produce no Part::Thinking");

        // Verify usage metadata
        let usage = resp.usage_metadata.as_ref().expect("response should have usage");
        assert_eq!(usage.prompt_token_count, 8);
        assert_eq!(usage.candidates_token_count, 9);
        assert_eq!(usage.total_token_count, 17);
    }

    // ── Test 4b: Azure non-reasoning tool calls ─────────────────────────
    //
    // **Validates: Requirements 3.5**
    //
    // `AzureOpenAIClient::generate_content()` with tool calls produces
    // `Part::FunctionCall` parts correctly.

    #[tokio::test]
    async fn azure_non_reasoning_tool_calls() {
        let server = MockServer::start().await;

        let response_body = serde_json::json!({
            "id": "chatcmpl-azure-tools-1",
            "object": "chat.completion",
            "created": 1700000000,
            "model": "gpt-4o",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [
                        {
                            "id": "call_azure_001",
                            "type": "function",
                            "function": {
                                "name": "get_weather",
                                "arguments": "{\"city\":\"Seattle\",\"units\":\"fahrenheit\"}"
                            }
                        },
                        {
                            "id": "call_azure_002",
                            "type": "function",
                            "function": {
                                "name": "get_stock_price",
                                "arguments": "{\"symbol\":\"MSFT\"}"
                            }
                        }
                    ]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {
                "prompt_tokens": 15,
                "completion_tokens": 25,
                "total_tokens": 40
            }
        });

        Mock::given(method("POST"))
            .and(path_regex(r"/openai/deployments/.+/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&response_body))
            .expect(1)
            .mount(&server)
            .await;

        let client = make_azure_client(&server.uri());

        let mut request = make_request();
        request.tools.insert(
            "get_weather".to_string(),
            serde_json::json!({
                "description": "Get weather for a city",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "city": { "type": "string" },
                        "units": { "type": "string" }
                    }
                }
            }),
        );
        request.tools.insert(
            "get_stock_price".to_string(),
            serde_json::json!({
                "description": "Get stock price",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "symbol": { "type": "string" }
                    }
                }
            }),
        );

        let mut stream =
            client.generate_content(request, false).await.expect("generate_content should succeed");

        let mut responses = Vec::new();
        while let Some(item) = stream.next().await {
            responses.push(item.expect("stream item should be Ok"));
        }

        assert_eq!(responses.len(), 1, "should yield exactly 1 response");

        let resp = &responses[0];
        assert!(!resp.partial, "response should have partial=false");
        assert!(resp.turn_complete, "response should have turn_complete=true");

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
        assert_eq!(func_calls[0].1["city"], "Seattle");
        assert_eq!(func_calls[0].1["units"], "fahrenheit");
        assert_eq!(func_calls[0].2.as_deref(), Some("call_azure_001"));

        // Verify second tool call
        assert_eq!(func_calls[1].0, "get_stock_price");
        assert_eq!(func_calls[1].1["symbol"], "MSFT");
        assert_eq!(func_calls[1].2.as_deref(), Some("call_azure_002"));

        // Verify no Thinking parts
        let thinking_count =
            content.parts.iter().filter(|p| matches!(p, Part::Thinking { .. })).count();
        assert_eq!(thinking_count, 0, "non-reasoning model should produce no Part::Thinking");

        // Verify usage metadata
        let usage = resp.usage_metadata.as_ref().expect("response should have usage");
        assert_eq!(usage.prompt_token_count, 15);
        assert_eq!(usage.candidates_token_count, 25);
        assert_eq!(usage.total_token_count, 40);
    }
}
