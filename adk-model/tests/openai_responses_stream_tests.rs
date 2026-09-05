#![cfg(feature = "openai")]

use adk_core::{Content, Llm, LlmRequest, LlmResponse, Part};
use adk_model::openai::{OpenAIResponsesClient, OpenAIResponsesConfig};
use futures::TryStreamExt;
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn completion(output: Value) -> Value {
    json!({
        "type": "response.completed", "sequence_number": 10,
        "response": {
            "id": "resp_test", "object": "response", "created_at": 0,
            "model": "test-model", "status": "completed", "output": output,
            "usage": {
                "input_tokens": 100, "input_tokens_details": {"cached_tokens": 42},
                "output_tokens": 20, "output_tokens_details": {"reasoning_tokens": 5},
                "total_tokens": 120
            }
        }
    })
}

async fn responses(events: &[Value], open_responses_mode: bool, prefix: &str) -> Vec<LlmResponse> {
    let server = MockServer::start().await;
    let mut body = prefix.to_string();
    for event in events {
        body.push_str(&format!("data: {event}\n\n"));
    }
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
        .expect(1)
        .mount(&server)
        .await;
    let client = OpenAIResponsesClient::new(
        OpenAIResponsesConfig::new("test-key", "test-model")
            .with_base_url(server.uri())
            .with_open_responses_mode(open_responses_mode),
    )
    .unwrap();
    let request = LlmRequest::new("test-model", vec![Content::new("user").with_text("read files")]);
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        client.generate_content(request, true).await.unwrap().try_collect::<Vec<_>>().await
    })
    .await
    .expect("stream must terminate")
    .expect("SSE events must deserialize");
    let requests = server.received_requests().await.unwrap();
    assert_eq!(serde_json::from_slice::<Value>(&requests[0].body).unwrap()["stream"], true);
    result
}

#[tokio::test]
async fn streamed_invalid_arguments_preserve_errors_and_usage_in_both_modes() {
    for compatible in [false, true] {
        let results = responses(
            &[completion(json!([{
                "type": "function_call", "id": "fc_bad", "call_id": "call_bad",
                "name": "read_file", "arguments": "{invalid", "status": "completed"
            }]))],
            compatible,
            "",
        )
        .await;
        assert_eq!(results.len(), 1);
        let result = &results[0];
        assert_eq!(
            result.error_code.as_deref(),
            Some("model.openai_responses.invalid_tool_arguments")
        );
        assert!(result.error_message.as_deref().is_some_and(|message| !message.is_empty()));
        assert!(result.turn_complete);
        assert!(!result.partial);
        assert_eq!(result.usage_metadata.as_ref().unwrap().cache_read_input_token_count, Some(42));
        assert_eq!(
            result.provider_metadata.as_ref().unwrap()["openai"]["response_id"],
            "resp_test"
        );
        assert!(result.content.as_ref().is_none_or(|content| content.parts.is_empty()));
    }
}

#[tokio::test]
async fn compatible_stream_restores_only_the_matching_tools_arguments() {
    let results = responses(
        &[
            json!({
                "type": "response.function_call_arguments.delta", "sequence_number": 1,
                "output_index": 1, "item_id": "fc_b", "delta": r#"{"path":"second.txt"}"#
            }),
            completion(json!([
                {"type": "function_call", "id": "fc_a", "call_id": "call_a",
                    "name": "read_file", "arguments": r#"{"path":"first.txt"}"#},
                {"type": "function_call", "id": "fc_b", "call_id": "call_b", "name": "read_file"}
            ])),
        ],
        true,
        "data:\n\ndata: \r\n\r\n: keepalive\n\n",
    )
    .await;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].error_code, None);
    assert_eq!(
        results[0].content.as_ref().unwrap().parts,
        vec![
            Part::FunctionCall {
                id: Some("call_a".into()),
                name: "read_file".into(),
                args: json!({"path": "first.txt"}),
                thought_signature: None
            },
            Part::FunctionCall {
                id: Some("call_b".into()),
                name: "read_file".into(),
                args: json!({"path": "second.txt"}),
                thought_signature: None
            },
        ]
    );
}
