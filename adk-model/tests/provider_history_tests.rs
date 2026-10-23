#![cfg(all(feature = "openai", feature = "gemini"))]

use adk_core::history::{for_provider, with_provider_parts};
use adk_core::{Content, Llm, LlmRequest, Part, ProviderOrigin};
use adk_model::{
    GeminiModel,
    openai::{OpenAIResponsesClient, OpenAIResponsesConfig},
    retry::RetryConfig,
};
use futures::StreamExt;
use serde_json::{Value, json};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

fn origin(provider: &str, model: &str) -> ProviderOrigin {
    ProviderOrigin {
        provider: provider.into(),
        model: model.into(),
        scope: "account-and-endpoint-owner".into(),
    }
}

async fn consume(model: &dyn Llm, contents: Vec<Content>) -> adk_core::LlmResponse {
    let mut stream =
        model.generate_content(LlmRequest::new(model.name(), contents), false).await.unwrap();
    let mut final_response = None;
    while let Some(response) = stream.next().await {
        final_response = Some(response.unwrap());
    }
    final_response.unwrap()
}

#[tokio::test]
async fn explicit_ownership_preserves_stateless_replay() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id":"resp_1","object":"response","created_at":1,"model":"reasoning-model","status":"completed",
            "output":[{"type":"reasoning","id":"rs_1","summary":[],"encrypted_content":"opaque +/ bytes"},
                {"type":"message","id":"msg_1","role":"assistant","status":"completed","content":[{"type":"output_text","text":"Answer","annotations":[]}]}]
        }))).mount(&server).await;
    let model = OpenAIResponsesClient::new(
        OpenAIResponsesConfig::new("key", "reasoning-model").with_base_url(server.uri()),
    )
    .unwrap()
    .with_retry_config(RetryConfig::disabled())
    .with_request_adapter(std::sync::Arc::new(|body, _| {
        body["store"] = json!(false);
        Ok(())
    }));
    let owner = origin("responses", model.name());
    let mut response = consume(&model, vec![Content::new("user").with_text("First")]).await;
    response.provider_metadata.as_mut().unwrap()["provider_origin"] = json!(owner);
    let original = serde_json::to_vec(&response).unwrap();
    let replay = with_provider_parts(
        response.content.as_ref().unwrap(),
        response.provider_metadata.as_ref(),
    );
    let mut contents = vec![
        Content::new("user").with_text("First"),
        replay,
        Content::new("user").with_text("Next"),
    ];
    for_provider(&mut contents, &owner);
    consume(&model, contents).await;
    let requests = server.received_requests().await.unwrap();
    let body: Value = serde_json::from_slice(&requests[1].body).unwrap();
    assert_eq!(body["store"], false);
    assert!(body.get("previous_response_id").is_none_or(Value::is_null));
    assert!(
        body["input"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["encrypted_content"] == "opaque +/ bytes")
    );
    assert!(!body.to_string().contains("__adk_provider_replay"));
    assert!(!body.to_string().contains("provider_origin"));
    assert_eq!(serde_json::to_vec(&response).unwrap(), original);
}

#[tokio::test]
async fn gemini_skips_unknown_opaque_data() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(200).set_body_json(json!({
        "candidates":[{"content":{"role":"model","parts":[{"text":"Answer"}]},"finishReason":"STOP"}]
    }))).mount(&server).await;
    let model = GeminiModel::new_with_base_url(
        "key",
        "gemini-3.7-flash",
        format!("{}/v1beta/", server.uri()),
    )
    .unwrap()
    .with_retry_config(RetryConfig::disabled());
    let source = Content {
        role: "model".into(),
        parts: vec![
            Part::ServerToolCall {
                server_tool_call: json!({"type":"reasoning","encrypted_content":"foreign-opaque"}),
            },
            Part::ServerToolResponse {
                server_tool_response: json!({"type":"unknown-provider-result","secret":"foreign-result"}),
            },
            Part::Text { text: "Visible answer".into() },
        ],
    };
    consume(
        &model,
        vec![
            Content::new("user").with_text("First"),
            source.clone(),
            Content::new("user").with_text("Next"),
        ],
    )
    .await;
    let bound = with_provider_parts(
        &source,
        Some(&json!({"provider_origin":origin("responses", "reasoning-model")})),
    );
    consume(
        &model,
        vec![
            Content::new("user").with_text("First"),
            bound,
            Content::new("user").with_text("Next"),
        ],
    )
    .await;
    for request in server.received_requests().await.unwrap() {
        let body = String::from_utf8(request.body).unwrap();
        assert!(body.contains("Visible answer"));
        for forbidden in [
            "foreign-opaque",
            "foreign-result",
            "__adk_provider_replay",
            "toolCall",
            "toolResponse",
        ] {
            assert!(!body.contains(forbidden), "{forbidden}: {body}");
        }
    }
}

#[tokio::test]
async fn signatures_require_matching_ownership() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(200).set_body_json(json!({
        "candidates":[{"content":{"role":"model","parts":[{"text":"Answer"}]},"finishReason":"STOP"}]
    }))).mount(&server).await;
    let model = GeminiModel::new_with_base_url(
        "key",
        "gemini-3.7-flash",
        format!("{}/v1beta/", server.uri()),
    )
    .unwrap();
    let owner = origin("gemini", model.name());
    let source = Content {
        role: "model".into(),
        parts: vec![
            Part::Thinking {
                thinking: "Private thought".into(),
                signature: Some("thinking-signature".into()),
            },
            Part::FunctionCall {
                name: "lookup".into(),
                args: json!({"value":42}),
                id: Some("call_1".into()),
                thought_signature: Some("call-signature".into()),
            },
            Part::Text { text: "Visible answer".into() },
        ],
    };
    for ownership in [Some(owner.clone()), Some(origin("responses", "other-model")), None] {
        let metadata = ownership.as_ref().map(|origin| json!({"provider_origin":origin}));
        let bound = with_provider_parts(&source, metadata.as_ref());
        let mut contents = vec![
            Content::new("user").with_text("First"),
            bound,
            Content::new("user").with_text("Next"),
        ];
        if ownership.as_ref() == Some(&owner) {
            for_provider(&mut contents, &owner);
        }
        consume(&model, contents).await;
    }
    for (index, request) in server.received_requests().await.unwrap().into_iter().enumerate() {
        let body = String::from_utf8(request.body).unwrap();
        assert!(body.contains("lookup") && body.contains("Visible answer"));
        // A foreign origin loses its signatures; owned and originless parts replay.
        assert_eq!(body.contains("call-signature"), index != 1, "{body}");
        assert_eq!(body.contains("thinking-signature"), index != 1, "{body}");
        assert_eq!(body.contains("Private thought"), index != 1, "{body}");
        assert!(!body.contains("__adk_provider_replay"));
    }
}

/// A plain ADK agent records no ownership and never calls `for_provider`. Its
/// same-invocation continuation still replays the model's signed parts.
#[cfg(feature = "anthropic")]
#[tokio::test]
async fn plain_continuation_replays_signed_parts() {
    use adk_core::FunctionResponseData;
    use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
    let source = Content {
        role: "model".into(),
        parts: vec![
            Part::Thinking {
                thinking: "Private thought".into(),
                signature: Some("thinking-signature".into()),
            },
            Part::FunctionCall {
                name: "lookup".into(),
                args: json!({"value":42}),
                id: Some("call_1".into()),
                thought_signature: Some("call-signature".into()),
            },
        ],
    };
    let contents = vec![
        Content::new("user").with_text("First"),
        with_provider_parts(&source, Some(&json!({"continue_turn":false}))),
        Content {
            role: "function".into(),
            parts: vec![Part::FunctionResponse {
                function_response: FunctionResponseData::new("lookup", json!({"value":42})),
                id: Some("call_1".into()),
                annotations: None,
            }],
        },
    ];

    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id":"msg_1","type":"message","role":"assistant","model":"claude-opus-5",
            "content":[{"type":"text","text":"Answer"}],"stop_reason":"end_turn","stop_sequence":null,
            "usage":{"input_tokens":1,"output_tokens":1}
        }))).mount(&server).await;
    let anthropic = AnthropicClient::new(
        AnthropicConfig::new("key", "claude-opus-5").with_base_url(server.uri()),
    )
    .unwrap()
    .with_retry_config(RetryConfig::disabled());
    consume(&anthropic, contents.clone()).await;
    let request = &server.received_requests().await.unwrap()[0];
    let body: Value = serde_json::from_slice(&request.body).unwrap();
    let assistant = &body["messages"][1]["content"];
    assert_eq!(assistant[0]["type"], "thinking", "{body}");
    assert_eq!(assistant[0]["signature"], "thinking-signature");
    assert_eq!(assistant[1]["type"], "tool_use");

    let server = MockServer::start().await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(200).set_body_json(json!({
        "candidates":[{"content":{"role":"model","parts":[{"text":"Answer"}]},"finishReason":"STOP"}]
    }))).mount(&server).await;
    let gemini = GeminiModel::new_with_base_url(
        "key",
        "gemini-3.7-flash",
        format!("{}/v1beta/", server.uri()),
    )
    .unwrap()
    .with_retry_config(RetryConfig::disabled());
    consume(&gemini, contents).await;
    let body =
        String::from_utf8(server.received_requests().await.unwrap()[0].body.clone()).unwrap();
    assert!(body.contains("call-signature"), "{body}");
    assert!(body.contains("thinking-signature"), "{body}");
    assert!(!body.contains("__adk_provider_replay"));
}
