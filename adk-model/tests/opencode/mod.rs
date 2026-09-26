use adk_core::{Content, Llm, LlmRequest, Part};
use adk_model::opencode::{OpenCodeApi, OpenCodeClient, OpenCodeConfig, OpenCodeService};
use adk_model::retry::RetryConfig;
use futures::TryStreamExt;
use serde_json::{Value, json};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

mod tools;
mod validation;

fn config(model: &str) -> OpenCodeConfig {
    config_for(OpenCodeService::Go, model)
}

fn config_for(service: OpenCodeService, model: &str) -> OpenCodeConfig {
    OpenCodeConfig::new(service, "test-key", model)
        .with_user_agent("test-coding-agent/1.0")
        .with_session_id("conversation-42")
        .with_retry_config(RetryConfig::disabled())
}

const CASES: &[(OpenCodeService, &str, OpenCodeApi, &str)] = &[
    (OpenCodeService::Go, "deepseek-v4.1-flash", OpenCodeApi::ChatCompletions, "chat/completions"),
    (OpenCodeService::Go, "minimax-m3", OpenCodeApi::Messages, "messages"),
    (OpenCodeService::Go, "gpt-6-luna", OpenCodeApi::Responses, "responses"),
    (OpenCodeService::Zen, "minimax-m3", OpenCodeApi::ChatCompletions, "chat/completions"),
    (OpenCodeService::Zen, "qwen3.8-flash", OpenCodeApi::Messages, "messages"),
    (OpenCodeService::Zen, "gpt-6-luna", OpenCodeApi::Responses, "responses"),
    (
        OpenCodeService::Zen,
        "gemini-3.8-flash",
        OpenCodeApi::GenerateContent,
        "models/gemini-3.8-flash:generateContent",
    ),
];

fn gemini() -> Value {
    json!({"candidates":[{"index":0,"content":{"role":"model","parts":[{"text":"done"}]},"finishReason":"STOP"}],
        "usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":2,"totalTokenCount":12}})
}

fn reply(api: OpenCodeApi, model: &str) -> Value {
    match api {
        OpenCodeApi::ChatCompletions => chat(model),
        OpenCodeApi::Responses => response(model),
        OpenCodeApi::Messages => message(model),
        OpenCodeApi::GenerateContent => gemini(),
    }
}

fn message(model: &str) -> Value {
    json!({"id":"msg_test", "type":"message", "role":"assistant", "model":model,
        "content":[{"type":"text","text":"done"}], "stop_reason":"end_turn", "stop_sequence":null,
        "usage":{"input_tokens":10,"output_tokens":2}})
}

fn response(model: &str) -> Value {
    json!({"id":"resp_test", "object":"response", "created_at":1, "model":model,
        "status":"completed", "output":[{"type":"message", "id":"msg_test", "role":"assistant",
            "status":"completed", "content":[{"type":"output_text","text":"done","annotations":[]}]}],
        "usage":{"input_tokens":10,"output_tokens":2,"total_tokens":12,
            "input_tokens_details":{"cached_tokens":0},"output_tokens_details":{"reasoning_tokens":0}}})
}

fn chat(model: &str) -> Value {
    json!({"id":"chat_test", "object":"chat.completion", "created":1, "model":model,
        "choices":[{"index":0,"message":{"role":"assistant","content":"done"},"finish_reason":"stop"}],
        "usage":{"prompt_tokens":10,"completion_tokens":2,"total_tokens":12}})
}

fn sse(events: &[Value]) -> String {
    events
        .iter()
        .map(|value| match value["type"].as_str() {
            Some(kind) => format!("event: {kind}\ndata: {value}\n\n"),
            None => format!("data: {value}\n\n"),
        })
        .collect()
}

#[tokio::test]
async fn routes_unary_requests_and_preserves_headers_across_turns() {
    for &(service, model, api, endpoint) in CASES {
        let server = MockServer::start().await;
        let auth = match api {
            OpenCodeApi::Messages => ("x-api-key", "test-key"),
            OpenCodeApi::GenerateContent => ("x-goog-api-key", "test-key"),
            _ => ("authorization", "Bearer test-key"),
        };
        let prefix = if service == OpenCodeService::Go { "/zen/go/v1" } else { "/zen/v1" };
        Mock::given(method("POST"))
            .and(path(format!("{prefix}/{endpoint}")))
            .and(header("user-agent", "test-coding-agent/1.0"))
            .and(header("x-opencode-session", "conversation-42"))
            .and(header(auth.0, auth.1))
            .respond_with(ResponseTemplate::new(200).set_body_json(reply(api, model)))
            .expect(2)
            .mount(&server)
            .await;
        let client = OpenCodeClient::new(
            config_for(service, model).with_base_url(format!("{}{prefix}/", server.uri())),
        )
        .unwrap();
        assert_eq!((client.api(), client.name()), (api, model));
        for text in ["inspect files", "summarize changes"] {
            let request = LlmRequest::new(model, vec![Content::new("user").with_text(text)]);
            let replies = client
                .generate_content(request, false)
                .await
                .unwrap()
                .try_collect::<Vec<_>>()
                .await
                .unwrap();
            assert!(
                replies.iter().any(|reply| reply.content.as_ref().is_some_and(|content| {
                    content
                        .parts
                        .iter()
                        .any(|part| matches!(part, Part::Text{text} if text == "done"))
                })),
                "{model}: {replies:?}"
            );
            assert!(replies.iter().any(|reply| {
                reply.usage_metadata.as_ref().is_some_and(|usage| usage.prompt_token_count == 10)
            }));
        }
        for request in server.received_requests().await.unwrap() {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            if api != OpenCodeApi::GenerateContent {
                assert_eq!(body["model"], model);
            }
            assert!(body.get("user_agent").is_none());
            assert!(body.get("session_id").is_none());
            if api == OpenCodeApi::Messages {
                assert_eq!(request.headers["anthropic-version"], "2023-06-01");
            }
        }
    }
}

#[tokio::test]
async fn routes_streams_with_conversation_headers_and_usage() {
    let chat_events = sse(&[
        json!({"id":"chat_test","object":"chat.completion.chunk","created":1,"model":"deepseek-v4.1-flash",
            "choices":[{"index":0,"delta":{"role":"assistant","content":"done"},"finish_reason":null}]}),
        json!({"id":"chat_test","object":"chat.completion.chunk","created":1,"model":"deepseek-v4.1-flash",
            "choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":2,"total_tokens":12}}),
    ]) + "data: [DONE]\n\n";
    let messages = sse(&[
        json!({"type":"message_start","message":{"id":"msg_test","type":"message","role":"assistant","model":"minimax-m3","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"output_tokens":0}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"done"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":2}}),
        json!({"type":"message_stop"}),
    ]);
    let responses = sse(&[
        json!({"type":"response.output_text.delta","sequence_number":0,"item_id":"msg_test","output_index":0,"content_index":0,"delta":"done","logprobs":[]}),
        json!({"type":"response.completed","sequence_number":1,"response":response("gpt-6-luna")}),
    ]);
    for &(service, model, api, endpoint) in CASES {
        let (endpoint, body) = match api {
            OpenCodeApi::ChatCompletions => (endpoint.to_owned(), chat_events.clone()),
            OpenCodeApi::Messages => (endpoint.to_owned(), messages.clone()),
            OpenCodeApi::Responses => (endpoint.to_owned(), responses.clone()),
            OpenCodeApi::GenerateContent => {
                (endpoint.replace(":generateContent", ":streamGenerateContent"), sse(&[gemini()]))
            }
        };
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(format!("/v1/{endpoint}")))
            .and(header("user-agent", "test-coding-agent/1.0"))
            .and(header("x-opencode-session", "conversation-42"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
            .expect(1)
            .mount(&server)
            .await;
        let client = OpenCodeClient::new(
            config_for(service, model).with_base_url(format!("{}/v1", server.uri())),
        )
        .unwrap();
        let replies = client
            .generate_content(
                LlmRequest::new(model, vec![Content::new("user").with_text("inspect files")]),
                true,
            )
            .await
            .unwrap()
            .try_collect::<Vec<_>>()
            .await
            .unwrap();
        assert!(
            replies.iter().any(|reply| reply.content.as_ref().is_some_and(|content| {
                content.parts.iter().any(|part| matches!(part, Part::Text{text} if text == "done"))
            })),
            "{model}: {replies:?}"
        );
        assert!(
            replies.iter().any(|reply| reply
                .usage_metadata
                .as_ref()
                .is_some_and(|usage| usage.prompt_token_count == 10)),
            "{model}: {replies:?}"
        );
        let requests = server.received_requests().await.unwrap();
        if api == OpenCodeApi::GenerateContent {
            assert_eq!(requests[0].url.query(), Some("alt=sse"));
        } else {
            assert_eq!(serde_json::from_slice::<Value>(&requests[0].body).unwrap()["stream"], true);
        }
    }
}
