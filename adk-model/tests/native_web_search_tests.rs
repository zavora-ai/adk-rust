//! Which provider-executed web search each client's transport serves, read from the client
//! (spec 3.1 §7.20), and DeepSeek's Responses `web_search` on the wire.

#[allow(unused_imports)]
use adk_core::{Llm, NativeWebSearch, NativeWebSearchTool};

#[cfg(feature = "gemini")]
#[test]
fn gemini_serves_google_search_without_an_allowlist() {
    let model = adk_model::GeminiModel::new("fixture-key", "gemini-3.5-flash-lite").unwrap();
    assert_eq!(
        model.native_web_search(),
        Some(NativeWebSearch::new("gemini", NativeWebSearchTool::GoogleSearch, false))
    );
}

#[cfg(feature = "gemini-interactions")]
#[test]
fn gemini_on_the_interactions_transport_serves_no_search_beside_function_tools() {
    let model = adk_model::GeminiModel::new("fixture-key", "gemini-3.7-flash")
        .unwrap()
        .use_interactions_api(true)
        .unwrap();
    assert_eq!(model.native_web_search(), None);
}

#[cfg(feature = "anthropic")]
#[test]
fn anthropic_serves_web_search_on_its_api_and_on_deepseeks_endpoint_only() {
    use adk_model::anthropic::AnthropicConfig;
    let client = |base: Option<&str>| {
        let mut config = AnthropicConfig::new("fixture-key", "claude-sonnet-5-5");
        if let Some(base) = base {
            config = config.with_base_url(base);
        }
        adk_model::AnthropicClient::new(config).unwrap()
    };
    let anthropic =
        Some(NativeWebSearch::new("anthropic", NativeWebSearchTool::AnthropicWebSearch, true));
    assert_eq!(client(None).native_web_search(), anthropic);
    assert_eq!(client(Some("https://api.anthropic.com")).native_web_search(), anthropic);
    assert_eq!(
        client(Some("https://api.deepseek.com/anthropic")).native_web_search(),
        Some(NativeWebSearch::new("deepseek", NativeWebSearchTool::AnthropicWebSearch, false)),
        "DeepSeek's Anthropic endpoint documents no allowlist"
    );
    assert_eq!(
        client(Some("https://gateway.example.com/anthropic")).native_web_search(),
        None,
        "a compatible endpoint is not assumed to run Anthropic's server tools"
    );
}

#[cfg(feature = "openai")]
#[test]
fn responses_serve_web_search_on_openai_and_deepseek_v4_pro_only() {
    use adk_model::openai::{OpenAIResponsesClient, OpenAIResponsesConfig};
    let client = |base: Option<&str>, model: &str| {
        let mut config = OpenAIResponsesConfig::new("fixture-key", model);
        if let Some(base) = base {
            config = config.with_base_url(base);
        }
        OpenAIResponsesClient::new(config).unwrap()
    };
    assert_eq!(
        client(None, "gpt-5.6-terra").native_web_search(),
        Some(NativeWebSearch::new("openai", NativeWebSearchTool::OpenAiWebSearch, true))
    );
    assert_eq!(
        client(Some("https://api.deepseek.com"), "deepseek-v4-pro").native_web_search(),
        Some(NativeWebSearch::new("deepseek", NativeWebSearchTool::DeepSeekWebSearch, false))
    );
    assert_eq!(
        client(Some("https://api.deepseek.com"), "deepseek-flash").native_web_search(),
        None,
        "deepseek-flash ignores web_search"
    );
    assert_eq!(client(Some("http://localhost:1234/v1"), "local").native_web_search(), None);
    // Chat Completions has no web_search tool.
    let chat = adk_model::OpenAIClient::new(adk_model::OpenAIConfig::new("fixture-key", "gpt-5.6"))
        .unwrap();
    assert_eq!(chat.native_web_search(), None);
}

#[cfg(feature = "deepseek")]
#[test]
fn deepseek_chat_completions_serve_no_web_search() {
    let client = adk_model::DeepSeekClient::new(adk_model::DeepSeekConfig::new(
        "fixture-key",
        "deepseek-v4-pro",
    ))
    .unwrap();
    assert_eq!(client.native_web_search(), None, "Chat Completions accept function tools only");
}

#[cfg(feature = "ollama")]
#[test]
fn ollama_serves_no_web_search() {
    let model = adk_model::OllamaModel::new(adk_model::OllamaConfig::new("gpt-oss:20b")).unwrap();
    assert_eq!(model.native_web_search(), None);
}

/// DeepSeek's `web_search` declared to a Responses client reaches the wire as
/// `{"type": "web_search"}`, and the `web_search_call` it answers with comes back as a
/// server tool call part.
#[cfg(feature = "openai")]
#[tokio::test]
async fn deepseek_web_search_reaches_the_responses_wire_and_its_call_comes_back() {
    use adk_core::{Content, LlmRequest, Part};
    use adk_model::openai::{OpenAIResponsesClient, OpenAIResponsesConfig};
    use futures::TryStreamExt;
    use serde_json::{Value, json};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "resp_ds",
            "status": "completed",
            "model": "deepseek-v4-pro",
            "output": [
                {
                    "type": "web_search_call",
                    "id": "ws_1",
                    "status": "completed",
                    "action": { "type": "search", "query": "latest stable rust release" }
                },
                {
                    "type": "message", "id": "msg_1", "role": "assistant", "status": "completed",
                    "content": [{ "type": "output_text", "text": "Rust 1.95.0.", "annotations": [] }]
                }
            ]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let client = OpenAIResponsesClient::new(
        OpenAIResponsesConfig::new("fixture-key", "deepseek-v4-pro").with_base_url(server.uri()),
    )
    .unwrap()
    .with_retry_config(adk_model::RetryConfig::disabled());
    let mut request = LlmRequest::new(
        "deepseek-v4-pro",
        vec![Content::new("user").with_text("What is the latest stable Rust release?")],
    );
    // The declaration `adk_tool::DeepSeekWebSearchTool` produces.
    request.tools.insert(
        "deepseek_web_search".to_string(),
        json!({
            "name": "deepseek_web_search",
            "description": "Searches the web for current information (DeepSeek server-side).",
            "x-adk-openai-tool": { "type": "web_search" },
        }),
    );
    let responses: Vec<_> =
        client.generate_content(request, false).await.unwrap().try_collect().await.unwrap();

    let sent: Value =
        serde_json::from_slice(&server.received_requests().await.unwrap()[0].body).unwrap();
    assert_eq!(sent["tools"], json!([{ "type": "web_search" }]), "{sent}");
    let parts: Vec<&Part> =
        responses.iter().filter_map(|r| r.content.as_ref()).flat_map(|c| &c.parts).collect();
    let call = parts
        .iter()
        .find_map(|part| match part {
            Part::ServerToolCall { server_tool_call } => Some(server_tool_call),
            _ => None,
        })
        .expect("the web_search_call is kept as a server tool call");
    assert_eq!(call["type"], "web_search_call");
    assert_eq!(call["action"]["query"], "latest stable rust release");
}
