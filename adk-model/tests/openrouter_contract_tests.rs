#![cfg(feature = "openrouter")]
#![allow(clippy::result_large_err)]

use adk_core::AdkError;
use adk_model::openrouter::{
    OpenRouterApiMode, OpenRouterChatContentPart, OpenRouterChatMessage,
    OpenRouterChatMessageContent, OpenRouterChatRequest, OpenRouterClient, OpenRouterConfig,
    OpenRouterFunctionDescription, OpenRouterResponseInput, OpenRouterResponseInputContent,
    OpenRouterResponseInputContentPart, OpenRouterResponseInputItem, OpenRouterResponseTool,
    OpenRouterResponsesRequest, OpenRouterResponsesStreamItem, OpenRouterTool,
    OpenRouterToolChoice,
};
use futures::StreamExt;
use serde_json::json;
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::PathBuf;

const DEFAULT_MODEL: &str = "openai/gpt-4.1-mini";
const DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/v1";
const DEFAULT_SITE_URL: &str = "https://github.com/zavora-ai/adk-rust";
const DEFAULT_APP_NAME: &str = "ADK-Rust OpenRouter Contract Tests";
const DEFAULT_WEB_SEARCH_TOOL: &str = "web_search_preview";
const DEFAULT_IMAGE_URL: &str =
    "https://upload.wikimedia.org/wikipedia/commons/3/3f/JPEG_example_flower.jpg";
const MAX_OUTPUT_TOKENS: i32 = 128;

#[derive(Debug, Clone)]
struct TestConfig {
    model: String,
    web_search_tool: String,
    image_url: String,
}

fn load_test_config() -> Option<TestConfig> {
    if env_value("OPENROUTER_API_KEY").is_none() {
        println!("Skipping OpenRouter live contract tests: missing OPENROUTER_API_KEY");
        return None;
    }

    Some(TestConfig {
        model: env_value("OPENROUTER_MODEL").unwrap_or_else(|| DEFAULT_MODEL.to_string()),
        web_search_tool: env_value("OPENROUTER_WEB_SEARCH_TOOL")
            .unwrap_or_else(|| DEFAULT_WEB_SEARCH_TOOL.to_string()),
        image_url: env_value("OPENROUTER_IMAGE_URL")
            .unwrap_or_else(|| DEFAULT_IMAGE_URL.to_string()),
    })
}

fn env_flag(key: &str) -> bool {
    env_value(key).is_some_and(|value| {
        matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")
    })
}

fn env_value(key: &str) -> Option<String> {
    env::var(key)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| dotenv_values().get(key).cloned())
}

fn dotenv_values() -> BTreeMap<String, String> {
    find_dotenv_path()
        .and_then(|path| fs::read_to_string(path).ok())
        .map(|contents| parse_env_file(&contents))
        .unwrap_or_default()
}

fn find_dotenv_path() -> Option<PathBuf> {
    let mut dir = env::current_dir().ok()?;

    loop {
        let candidate = dir.join(".env");
        if candidate.is_file() {
            return Some(candidate);
        }
        if !dir.pop() {
            return None;
        }
    }
}

fn parse_env_file(contents: &str) -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();

    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            continue;
        };

        values.insert(key.trim().to_string(), normalize_env_value(value.trim()));
    }

    values
}

fn normalize_env_value(value: &str) -> String {
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        let quoted_with_double = bytes[0] == b'"' && bytes[value.len() - 1] == b'"';
        let quoted_with_single = bytes[0] == b'\'' && bytes[value.len() - 1] == b'\'';

        if quoted_with_double || quoted_with_single {
            return value[1..value.len() - 1].to_string();
        }
    }

    value.to_string()
}

fn build_client(api_mode: OpenRouterApiMode, model: &str) -> Result<OpenRouterClient, AdkError> {
    let api_key = env_value("OPENROUTER_API_KEY")
        .ok_or_else(|| AdkError::model("missing required env var: OPENROUTER_API_KEY"))?;

    OpenRouterClient::new(
        OpenRouterConfig::new(api_key, model)
            .with_base_url(
                env_value("OPENROUTER_BASE_URL").unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
            )
            .with_http_referer(
                env_value("OPENROUTER_SITE_URL").unwrap_or_else(|| DEFAULT_SITE_URL.to_string()),
            )
            .with_title(
                env_value("OPENROUTER_APP_NAME").unwrap_or_else(|| DEFAULT_APP_NAME.to_string()),
            )
            .with_default_api_mode(api_mode),
    )
}

fn chat_message_text(message: &OpenRouterChatMessage) -> Option<String> {
    match message.content.as_ref()? {
        OpenRouterChatMessageContent::Text(text) => Some(text.clone()),
        OpenRouterChatMessageContent::Parts(parts) => {
            let text = parts
                .iter()
                .filter(|part| part.kind == "text")
                .filter_map(|part| part.text.as_deref())
                .collect::<Vec<_>>()
                .join("");
            (!text.is_empty()).then_some(text)
        }
    }
}

fn response_output_text(response: &adk_model::openrouter::OpenRouterResponse) -> Option<String> {
    if let Some(text) = response.output_text.as_deref().filter(|text| !text.trim().is_empty()) {
        return Some(text.to_string());
    }

    let text = response
        .output
        .iter()
        .filter_map(|item| item.content.as_ref())
        .filter_map(serde_json::Value::as_array)
        .flat_map(|parts| parts.iter())
        .filter(|part| part.get("type").and_then(serde_json::Value::as_str) == Some("output_text"))
        .filter_map(|part| part.get("text").and_then(serde_json::Value::as_str))
        .collect::<Vec<_>>()
        .join("");

    (!text.is_empty()).then_some(text)
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live contract test; requires OpenRouter credentials"]
async fn chat_non_streaming_contract() {
    let Some(config) = load_test_config() else {
        return;
    };

    let client = build_client(OpenRouterApiMode::ChatCompletions, &config.model)
        .expect("client should build");
    let response = client
        .send_chat(OpenRouterChatRequest {
            model: config.model,
            messages: vec![OpenRouterChatMessage {
                role: "user".to_string(),
                content: Some(OpenRouterChatMessageContent::Text(
                    "Reply with one short sentence about OpenRouter.".to_string(),
                )),
                ..Default::default()
            }],
            max_completion_tokens: Some(MAX_OUTPUT_TOKENS),
            ..Default::default()
        })
        .await
        .expect("chat request should succeed");

    let message = response
        .choices
        .first()
        .and_then(|choice| choice.message.as_ref())
        .expect("chat response should include a message");
    let text = chat_message_text(message).expect("chat response should include text");

    assert!(!text.trim().is_empty(), "chat response text must be non-empty");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live contract test; requires OpenRouter credentials"]
async fn chat_streaming_contract() {
    let Some(config) = load_test_config() else {
        return;
    };

    let client = build_client(OpenRouterApiMode::ChatCompletions, &config.model)
        .expect("client should build");
    let mut stream = client
        .send_chat_stream(OpenRouterChatRequest {
            model: config.model,
            messages: vec![OpenRouterChatMessage {
                role: "user".to_string(),
                content: Some(OpenRouterChatMessageContent::Text(
                    "Count from 1 to 5, one number per line.".to_string(),
                )),
                ..Default::default()
            }],
            max_completion_tokens: Some(MAX_OUTPUT_TOKENS),
            ..Default::default()
        })
        .await
        .expect("chat stream should start");

    let mut chunk_count = 0usize;
    let mut full_text = String::new();

    while let Some(item) = stream.next().await {
        match item.expect("chat stream item should parse") {
            adk_model::openrouter::OpenRouterChatStreamItem::Chunk(chunk) => {
                chunk_count += 1;
                if let Some(delta) = chunk.choices.first().and_then(|choice| choice.delta.as_ref())
                {
                    if let Some(text) = chat_message_text(delta) {
                        full_text.push_str(&text);
                    }
                }
            }
            adk_model::openrouter::OpenRouterChatStreamItem::Done => break,
            adk_model::openrouter::OpenRouterChatStreamItem::Error(error) => {
                panic!("chat stream emitted error: {}", error.message);
            }
        }
    }

    assert!(chunk_count > 0, "chat stream should emit at least one chunk");
    assert!(!full_text.trim().is_empty(), "chat stream should emit text");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live contract test; requires OpenRouter credentials"]
async fn responses_non_streaming_contract() {
    let Some(config) = load_test_config() else {
        return;
    };

    let client =
        build_client(OpenRouterApiMode::Responses, &config.model).expect("client should build");
    let response = client
        .create_response(OpenRouterResponsesRequest {
            model: Some(config.model),
            input: Some(OpenRouterResponseInput::Text(
                "Explain what OpenRouter routing does in one sentence.".to_string(),
            )),
            max_output_tokens: Some(MAX_OUTPUT_TOKENS),
            ..Default::default()
        })
        .await
        .expect("responses request should succeed");

    let text = response_output_text(&response).expect("responses request should produce text");
    assert!(!text.trim().is_empty(), "responses text must be non-empty");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live contract test; requires OpenRouter credentials"]
async fn responses_streaming_contract() {
    let Some(config) = load_test_config() else {
        return;
    };

    let client =
        build_client(OpenRouterApiMode::Responses, &config.model).expect("client should build");
    let mut stream = client
        .create_response_stream(OpenRouterResponsesRequest {
            model: Some(config.model),
            input: Some(OpenRouterResponseInput::Text(
                "List three short benefits of provider routing.".to_string(),
            )),
            max_output_tokens: Some(MAX_OUTPUT_TOKENS),
            ..Default::default()
        })
        .await
        .expect("responses stream should start");

    let mut saw_completed = false;
    let mut full_text = String::new();

    while let Some(item) = stream.next().await {
        match item.expect("responses stream item should parse") {
            OpenRouterResponsesStreamItem::Event(event) => match event.kind.as_str() {
                "response.output_text.delta" => {
                    if let Some(delta) = event.delta.as_deref() {
                        full_text.push_str(delta);
                    }
                }
                "response.completed" => saw_completed = true,
                _ => {}
            },
            OpenRouterResponsesStreamItem::Done => break,
            OpenRouterResponsesStreamItem::Error(error) => {
                panic!("responses stream emitted error: {}", error.message);
            }
        }
    }

    assert!(!full_text.trim().is_empty(), "responses stream should emit text");
    assert!(saw_completed, "responses stream should emit response.completed");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live contract test; requires OpenRouter credentials"]
async fn tool_calling_contract() {
    let Some(config) = load_test_config() else {
        return;
    };

    let client = build_client(OpenRouterApiMode::ChatCompletions, &config.model)
        .expect("client should build");
    let response = client
        .send_chat(OpenRouterChatRequest {
            model: config.model,
            messages: vec![OpenRouterChatMessage {
                role: "user".to_string(),
                content: Some(OpenRouterChatMessageContent::Text(
                    "Call get_weather for Nairobi.".to_string(),
                )),
                ..Default::default()
            }],
            tools: Some(vec![OpenRouterTool {
                kind: "function".to_string(),
                function: Some(OpenRouterFunctionDescription {
                    name: "get_weather".to_string(),
                    description: Some("Look up the weather for a city.".to_string()),
                    parameters: Some(json!({
                        "type": "object",
                        "properties": {
                            "city": { "type": "string" }
                        },
                        "required": ["city"]
                    })),
                    ..Default::default()
                }),
                ..Default::default()
            }]),
            tool_choice: Some(OpenRouterToolChoice::Mode("required".to_string())),
            max_completion_tokens: Some(MAX_OUTPUT_TOKENS),
            ..Default::default()
        })
        .await
        .expect("tool-calling request should succeed");

    let tool_call = response
        .choices
        .first()
        .and_then(|choice| choice.message.as_ref())
        .and_then(|message| message.tool_calls.as_ref())
        .and_then(|tool_calls| tool_calls.first())
        .expect("response should contain a tool call");
    let function = tool_call.function.as_ref().expect("tool call should contain a function");
    let name = function.name.as_deref().expect("tool call should include a function name");
    let arguments =
        function.arguments.as_deref().expect("tool call should include serialized arguments");
    let args: serde_json::Value =
        serde_json::from_str(arguments).expect("tool call arguments should be valid JSON");

    assert_eq!(name, "get_weather");
    assert_eq!(args["city"].as_str(), Some("Nairobi"));
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live contract test; requires OpenRouter credentials and OPENROUTER_ENABLE_WEB_SEARCH=1"]
async fn built_in_web_search_contract() {
    let Some(config) = load_test_config() else {
        return;
    };
    if !env_flag("OPENROUTER_ENABLE_WEB_SEARCH") {
        println!("Skipping OpenRouter web-search contract: set OPENROUTER_ENABLE_WEB_SEARCH=1");
        return;
    }

    let client =
        build_client(OpenRouterApiMode::Responses, &config.model).expect("client should build");
    let response = client
        .create_response(OpenRouterResponsesRequest {
            model: Some(config.model),
            input: Some(OpenRouterResponseInput::Text(
                "Find one recent fact about Rust releases and cite the source in one sentence."
                    .to_string(),
            )),
            tools: Some(vec![OpenRouterResponseTool {
                kind: config.web_search_tool,
                ..Default::default()
            }]),
            max_output_tokens: Some(MAX_OUTPUT_TOKENS),
            ..Default::default()
        })
        .await
        .expect("web-search response should succeed");

    let text = response_output_text(&response).expect("web-search response should produce text");
    assert!(!text.trim().is_empty(), "web-search response text must be non-empty");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live contract test; requires OpenRouter credentials"]
async fn multimodal_image_contract() {
    let Some(config) = load_test_config() else {
        return;
    };

    let client = build_client(OpenRouterApiMode::ChatCompletions, &config.model)
        .expect("client should build");
    let response = client
        .send_chat(OpenRouterChatRequest {
            model: config.model,
            messages: vec![OpenRouterChatMessage {
                role: "user".to_string(),
                content: Some(OpenRouterChatMessageContent::Parts(vec![
                    OpenRouterChatContentPart {
                        kind: "text".to_string(),
                        text: Some("Describe this image in one sentence.".to_string()),
                        ..Default::default()
                    },
                    OpenRouterChatContentPart {
                        kind: "image_url".to_string(),
                        image_url: Some(json!({
                            "url": config.image_url,
                            "detail": "auto"
                        })),
                        ..Default::default()
                    },
                ])),
                ..Default::default()
            }],
            max_completion_tokens: Some(MAX_OUTPUT_TOKENS),
            ..Default::default()
        })
        .await
        .expect("multimodal chat request should succeed");

    let message = response
        .choices
        .first()
        .and_then(|choice| choice.message.as_ref())
        .expect("multimodal response should include a message");
    let text = chat_message_text(message).expect("multimodal response should include text");

    assert!(!text.trim().is_empty(), "multimodal response text must be non-empty");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live contract test; requires OpenRouter credentials"]
async fn discovery_contract() {
    let Some(config) = load_test_config() else {
        return;
    };

    let client = build_client(OpenRouterApiMode::ChatCompletions, &config.model)
        .expect("client should build");
    let models = client.list_models().await.expect("list_models should succeed");
    let providers = client.list_providers().await.expect("list_providers should succeed");

    assert!(!models.is_empty(), "discovery should return at least one model");
    assert!(!providers.is_empty(), "discovery should return at least one provider");
    assert!(
        models.iter().any(|model| model.id == config.model),
        "configured model should appear in discovery results"
    );

    if let Some((author, slug)) = config.model.split_once('/') {
        let endpoints = client
            .get_model_endpoints(author, slug)
            .await
            .expect("get_model_endpoints should succeed");
        assert!(!endpoints.endpoints.is_empty(), "model endpoints should not be empty");
    }

    match client.get_credits().await {
        Ok(credits) => {
            assert!(credits.total_credits >= 0.0);
            assert!(credits.total_usage >= 0.0);
        }
        Err(err) => {
            assert!(
                err.is_unauthorized() || err.http_status_code() == 403,
                "unexpected credits error: {err}"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live contract test; requires OpenRouter credentials"]
async fn responses_multimodal_image_contract() {
    let Some(config) = load_test_config() else {
        return;
    };

    let client =
        build_client(OpenRouterApiMode::Responses, &config.model).expect("client should build");
    let response = client
        .create_response(OpenRouterResponsesRequest {
            model: Some(config.model),
            input: Some(OpenRouterResponseInput::Items(vec![OpenRouterResponseInputItem {
                kind: "message".to_string(),
                role: Some("user".to_string()),
                content: Some(OpenRouterResponseInputContent::Parts(vec![
                    OpenRouterResponseInputContentPart {
                        kind: "input_text".to_string(),
                        text: Some("Describe this image in one sentence.".to_string()),
                        ..Default::default()
                    },
                    OpenRouterResponseInputContentPart {
                        kind: "input_image".to_string(),
                        image_url: Some(config.image_url),
                        detail: Some("auto".to_string()),
                        ..Default::default()
                    },
                ])),
                ..Default::default()
            }])),
            max_output_tokens: Some(MAX_OUTPUT_TOKENS),
            ..Default::default()
        })
        .await
        .expect("responses multimodal request should succeed");

    let text =
        response_output_text(&response).expect("responses multimodal request should produce text");
    assert!(!text.trim().is_empty(), "responses multimodal text must be non-empty");
}
