//! HTTP-level request tests for provider-specific reasoning history replay.

#[cfg(feature = "openai")]
mod reasoning_history_requests {
    use adk_core::{Content, FunctionResponseData, Llm, LlmRequest, Part};
    use adk_model::openai::{AzureConfig, AzureOpenAIClient, OpenAIClient, OpenAIConfig};
    use adk_model::retry::RetryConfig;
    use adk_model::{OpenAICompatible, OpenAICompatibleConfig, ReasoningReplayField};
    use futures::StreamExt;
    use serde_json::{Value, json};
    use wiremock::matchers::{body_json, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const MODEL: &str = "test-model";

    fn text_history() -> LlmRequest {
        LlmRequest::new(
            MODEL,
            vec![
                Content::new("user").with_text("Question"),
                Content::new("assistant")
                    .with_thinking("Private reasoning")
                    .with_text("Visible answer"),
                Content::new("user").with_text("Follow-up"),
            ],
        )
    }

    fn tool_call_history() -> LlmRequest {
        let mut assistant = Content::new("assistant").with_thinking("I should call the tool");
        assistant.parts.push(Part::FunctionCall {
            name: "lookup".to_string(),
            args: json!({"query": "value"}),
            id: Some("call_1".to_string()),
            thought_signature: None,
        });

        let tool = Content {
            role: "tool".to_string(),
            parts: vec![Part::FunctionResponse {
                function_response: FunctionResponseData::new("lookup", json!({"result": "value"})),
                id: Some("call_1".to_string()),
                annotations: None,
            }],
        };

        LlmRequest::new(
            MODEL,
            vec![Content::new("user").with_text("Look this up"), assistant, tool],
        )
    }

    fn success_response() -> Value {
        json!({
            "id": "chatcmpl-test",
            "object": "chat.completion",
            "created": 1_700_000_000,
            "model": MODEL,
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "Done"},
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 1,
                "completion_tokens": 1,
                "total_tokens": 2
            }
        })
    }

    async fn mount_strict_endpoint(server: &MockServer, endpoint: &str, expected_body: Value) {
        Mock::given(method("POST"))
            .and(path(endpoint))
            .and(body_json(expected_body))
            .respond_with(ResponseTemplate::new(200).set_body_json(success_response()))
            .with_priority(1)
            .expect(1)
            .mount(server)
            .await;

        Mock::given(method("POST"))
            .and(path(endpoint))
            .respond_with(
                ResponseTemplate::new(400).set_body_string("unsupported reasoning history field"),
            )
            .with_priority(10)
            .mount(server)
            .await;
    }

    async fn run_non_streaming(client: &impl Llm, request: LlmRequest) -> Result<(), String> {
        let mut stream =
            client.generate_content(request, false).await.map_err(|error| error.to_string())?;
        let mut response_count = 0;
        while let Some(response) = stream.next().await {
            response.map_err(|error| error.to_string())?;
            response_count += 1;
        }
        if response_count != 1 {
            return Err(format!("expected one response, received {response_count}"));
        }
        Ok(())
    }

    async fn assert_strict_endpoint_accepts(
        server: &MockServer,
        client: &impl Llm,
        request: LlmRequest,
    ) {
        let result = run_non_streaming(client, request).await;
        let requests = server.received_requests().await.unwrap_or_default();
        let body = requests
            .last()
            .and_then(|request| request.body_json::<Value>().ok())
            .unwrap_or(Value::Null);
        assert!(result.is_ok(), "strict endpoint rejected request: {result:?}\nbody: {body:#}");
    }

    #[tokio::test]
    async fn standard_openai_omits_reasoning_replay_fields() {
        let server = MockServer::start().await;
        let expected = json!({
            "model": MODEL,
            "messages": [
                {"role": "user", "content": "Question"},
                {"role": "assistant", "content": "Visible answer"},
                {"role": "user", "content": "Follow-up"}
            ]
        });
        mount_strict_endpoint(&server, "/chat/completions", expected).await;

        let config = OpenAIConfig::compatible("test-key", server.uri(), MODEL);
        let client = OpenAIClient::new(config)
            .expect("client should build")
            .with_retry_config(RetryConfig::disabled());

        assert_strict_endpoint_accepts(&server, &client, text_history()).await;
    }

    #[tokio::test]
    async fn azure_openai_omits_reasoning_replay_fields() {
        let server = MockServer::start().await;
        let endpoint = format!("/openai/deployments/{MODEL}/chat/completions");
        let expected = json!({
            "model": MODEL,
            "messages": [
                {"role": "user", "content": "Question"},
                {"role": "assistant", "content": "Visible answer"},
                {"role": "user", "content": "Follow-up"}
            ]
        });

        Mock::given(method("POST"))
            .and(path(endpoint.as_str()))
            .and(query_param("api-version", "2024-12-01-preview"))
            .and(body_json(expected))
            .respond_with(ResponseTemplate::new(200).set_body_json(success_response()))
            .with_priority(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(endpoint.as_str()))
            .respond_with(
                ResponseTemplate::new(400).set_body_string("unsupported reasoning history field"),
            )
            .with_priority(10)
            .mount(&server)
            .await;

        let config = AzureConfig::new("test-key", server.uri(), "2024-12-01-preview", MODEL);
        let client = AzureOpenAIClient::new(config)
            .expect("client should build")
            .with_retry_config(RetryConfig::disabled());

        assert_strict_endpoint_accepts(&server, &client, text_history()).await;
    }

    #[tokio::test]
    async fn reasoning_content_backend_receives_only_its_configured_field() {
        let server = MockServer::start().await;
        let expected = json!({
            "model": MODEL,
            "messages": [
                {"role": "user", "content": "Question"},
                {
                    "role": "assistant",
                    "content": "Visible answer",
                    "reasoning_content": "Private reasoning"
                },
                {"role": "user", "content": "Follow-up"}
            ]
        });
        mount_strict_endpoint(&server, "/chat/completions", expected).await;

        let config = OpenAICompatibleConfig::new("test-key", MODEL)
            .with_provider_name("reasoning-content-backend")
            .with_base_url(server.uri());
        let client = OpenAICompatible::new(config)
            .expect("client should build")
            .with_reasoning_replay(true)
            .with_retry_config(RetryConfig::disabled());

        assert_strict_endpoint_accepts(&server, &client, text_history()).await;
    }

    #[tokio::test]
    async fn configured_backends_replay_tool_call_continuation_with_only_supported_field() {
        for field in [ReasoningReplayField::ReasoningContent, ReasoningReplayField::Reasoning] {
            let server = MockServer::start().await;
            let mut expected = json!({
                "model": MODEL,
                "messages": [
                    {"role": "user", "content": "Look this up"},
                    {
                        "role": "assistant",
                        "reasoning": "I should call the tool",
                        "tool_calls": [{
                            "id": "call_1",
                            "type": "function",
                            "function": {
                                "name": "lookup",
                                "arguments": "{\"query\":\"value\"}"
                            }
                        }]
                    },
                    {
                        "role": "tool",
                        "tool_call_id": "call_1",
                        "content": "{\"result\":\"value\"}"
                    }
                ]
            });
            if field == ReasoningReplayField::ReasoningContent {
                let assistant = expected["messages"][1].as_object_mut().unwrap();
                let reasoning = assistant.remove("reasoning").unwrap();
                assistant.insert("reasoning_content".into(), reasoning);
            }
            mount_strict_endpoint(&server, "/chat/completions", expected).await;

            let config = OpenAICompatibleConfig::new("test-key", MODEL)
                .with_provider_name("reasoning-backend")
                .with_base_url(server.uri());
            let client = OpenAICompatible::new(config)
                .expect("client should build")
                .with_reasoning_replay(true)
                .with_reasoning_replay_field(field)
                .with_retry_config(RetryConfig::disabled());

            assert_strict_endpoint_accepts(&server, &client, tool_call_history()).await;
        }
    }

    #[tokio::test]
    async fn compatible_default_and_explicit_disable_omit_reasoning_fields() {
        for initial_field in [
            None,
            Some(ReasoningReplayField::ReasoningContent),
            Some(ReasoningReplayField::Reasoning),
        ] {
            let server = MockServer::start().await;
            let expected = json!({
                "model": MODEL,
                "messages": [
                    {"role": "user", "content": "Question"},
                    {"role": "assistant", "content": "Visible answer"},
                    {"role": "user", "content": "Follow-up"}
                ]
            });
            mount_strict_endpoint(&server, "/chat/completions", expected).await;
            let config = OpenAICompatibleConfig::new("test-key", MODEL).with_base_url(server.uri());
            let mut client =
                OpenAICompatible::new(config).unwrap().with_retry_config(RetryConfig::disabled());
            if let Some(field) = initial_field {
                client = client.with_reasoning_replay_field(field).with_reasoning_replay(false);
            }
            assert_strict_endpoint_accepts(&server, &client, text_history()).await;
        }
    }
}
