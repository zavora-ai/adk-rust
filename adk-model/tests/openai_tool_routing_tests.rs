//! `OpenAIClient` sends tool-carrying requests for GPT-5.6 and later models through
//! the Responses API, because Chat Completions rejects function tools for them
//! while reasoning is enabled.

#![cfg(feature = "openai")]

use adk_core::{Content, Llm, LlmRequest, Part};
use adk_model::openai::{OpenAIClient, OpenAIConfig, OpenAIReasoningEffort};
use adk_model::retry::RetryConfig;
use futures::TryStreamExt;
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn tool_request(model: &str) -> LlmRequest {
    let mut request = LlmRequest::new(
        model,
        vec![Content::new("user").with_text("What is the weather in Paris?")],
    );
    request.tools.insert(
        "get_weather".to_string(),
        json!({
            "description": "Get the weather for a city",
            "parameters": {
                "type": "object",
                "properties": {"city": {"type": "string"}},
                "required": ["city"]
            }
        }),
    );
    request
}

fn responses_body(model: &str) -> Value {
    json!({
        "id": "resp_routed", "object": "response", "created_at": 0, "model": model,
        "status": "completed",
        "output": [{
            "type": "function_call", "id": "fc_1", "call_id": "call_1",
            "name": "get_weather", "arguments": "{\"city\":\"Paris\"}", "status": "completed"
        }],
        "usage": {
            "input_tokens": 10, "input_tokens_details": {"cached_tokens": 0},
            "output_tokens": 5, "output_tokens_details": {"reasoning_tokens": 0},
            "total_tokens": 15
        }
    })
}

fn chat_body() -> Value {
    json!({
        "id": "chatcmpl-1", "object": "chat.completion", "created": 0, "model": "fixture",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "Hello"},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 5, "completion_tokens": 1, "total_tokens": 6}
    })
}

/// Serves both endpoints and returns the path of every request the client sent.
async fn served_paths(
    client: OpenAIClient,
    request: LlmRequest,
    server: &MockServer,
) -> (Vec<String>, Vec<Part>) {
    let responses = client
        .generate_content(request, false)
        .await
        .expect("request should be sent")
        .try_collect::<Vec<_>>()
        .await
        .expect("response should parse");
    let parts = responses.into_iter().filter_map(|response| response.content).flat_map(|c| c.parts);
    let paths = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|request| request.url.path().to_string())
        .collect();
    (paths, parts.collect())
}

async fn mock_server(model: &str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(responses_body(model)))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_body()))
        .mount(&server)
        .await;
    server
}

fn client(server: &MockServer, model: &str) -> OpenAIClient {
    let mut config = OpenAIConfig::new("test-key", model);
    config.base_url = Some(server.uri());
    OpenAIClient::new(config).unwrap().with_retry_config(RetryConfig::disabled())
}

#[tokio::test]
async fn gpt_5_6_tool_requests_use_the_responses_api() {
    let model = "gpt-5.6-terra";
    let server = mock_server(model).await;

    let (paths, parts) = served_paths(client(&server, model), tool_request(model), &server).await;

    assert_eq!(paths, vec!["/responses".to_string()]);
    assert_eq!(
        parts,
        vec![Part::FunctionCall {
            name: "get_weather".into(),
            args: json!({"city": "Paris"}),
            id: Some("call_1".into()),
            thought_signature: None,
        }]
    );
    let sent: Value =
        serde_json::from_slice(&server.received_requests().await.unwrap()[0].body).unwrap();
    assert_eq!(
        (&sent["model"], &sent["store"], &sent["tools"][0]["name"]),
        (&json!(model), &json!(false), &json!("get_weather"))
    );
}

#[tokio::test]
async fn streaming_tool_requests_use_the_responses_api() {
    let model = "gpt-6-astra";
    let server = MockServer::start().await;
    let completed = json!({"type": "response.completed", "sequence_number": 1,
        "response": responses_body(model)});
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(format!("data: {completed}\n\ndata: [DONE]\n\n")),
        )
        .expect(1)
        .mount(&server)
        .await;

    let responses = client(&server, model)
        .generate_content(tool_request(model), true)
        .await
        .unwrap()
        .try_collect::<Vec<_>>()
        .await
        .unwrap();

    let calls: Vec<_> = responses
        .iter()
        .filter_map(|response| response.content.as_ref())
        .flat_map(|content| &content.parts)
        .filter(|part| matches!(part, Part::FunctionCall { .. }))
        .collect();
    assert_eq!(calls.len(), 1, "{responses:?}");
}

#[tokio::test]
async fn requests_chat_completions_accepts_stay_on_chat_completions() {
    let model = "gpt-5.6-terra";
    let server = mock_server(model).await;
    let no_tools = LlmRequest::new(model, vec![Content::new("user").with_text("Hello")]);
    let (paths, _) = served_paths(client(&server, model), no_tools, &server).await;
    assert_eq!(paths, vec!["/chat/completions".to_string()], "request without tools");

    let server = mock_server("gpt-5.5").await;
    let (paths, _) =
        served_paths(client(&server, "gpt-5.5"), tool_request("gpt-5.5"), &server).await;
    assert_eq!(paths, vec!["/chat/completions".to_string()], "model before GPT-5.6");

    let server = mock_server(model).await;
    let mut config = OpenAIConfig::new("test-key", model);
    config.base_url = Some(server.uri());
    let reasoning_off =
        OpenAIClient::new_with_reasoning_effort(config, OpenAIReasoningEffort::None)
            .unwrap()
            .with_retry_config(RetryConfig::disabled());
    let (paths, _) = served_paths(reasoning_off, tool_request(model), &server).await;
    assert_eq!(paths, vec!["/chat/completions".to_string()], "reasoning effort none");

    let server = mock_server(model).await;
    let mut request = tool_request(model);
    let mut config = adk_core::GenerateContentConfig::default();
    config.extensions.insert("openai".to_string(), json!({"reasoning_effort": "none"}));
    request.config = Some(config);
    let (paths, _) = served_paths(client(&server, model), request, &server).await;
    assert_eq!(paths, vec!["/chat/completions".to_string()], "reasoning disabled by extension");
}

/// Live check against OpenAI. Requires `OPENAI_API_KEY`.
#[tokio::test]
#[ignore = "calls the OpenAI API; requires OPENAI_API_KEY"]
async fn live_gpt_5_6_terra_calls_a_function_tool() {
    let Ok(api_key) = std::env::var("OPENAI_API_KEY") else {
        return;
    };
    let model = adk_model::catalog::OPENAI_DEFAULT;
    let client = OpenAIClient::new(OpenAIConfig::new(api_key, model)).unwrap();
    let mut request = tool_request(model);
    request.contents =
        vec![Content::new("user").with_text("Call get_weather for Paris. Do not answer directly.")];

    for stream in [false, true] {
        let responses = client
            .generate_content(request.clone(), stream)
            .await
            .expect("OpenAI accepted the tool request")
            .try_collect::<Vec<_>>()
            .await
            .expect("OpenAI response parsed");
        let called = responses
            .iter()
            .filter_map(|response| response.content.as_ref())
            .flat_map(|content| &content.parts)
            .any(|part| matches!(part, Part::FunctionCall { name, .. } if name == "get_weather"));
        assert!(called, "stream={stream}: {responses:?}");
    }
}
