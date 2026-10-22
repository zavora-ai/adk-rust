//! Each provider sends `GenerateContentConfig::effort` on the wire for the call that sets it,
//! over the effort its client was built with, and leaves it out where the model has no
//! effort control (spec 3.1 §7.23, §10 C4).
#![cfg(any(
    feature = "anthropic",
    feature = "openai",
    feature = "gemini",
    feature = "openrouter",
    feature = "deepseek",
    feature = "groq"
))]

use adk_core::{Content, Effort, GenerateContentConfig, Llm, LlmRequest};
use futures::StreamExt;
use serde_json::{Value, json};
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A request for `model` whose config sets `effort`.
fn request(model: &str, effort: Option<Effort>) -> LlmRequest {
    let mut request = LlmRequest::new(model, vec![Content::new("user").with_text("Say hi.")]);
    request.config = Some(GenerateContentConfig { effort, ..Default::default() });
    request
}

/// Sends `request` and returns the JSON body of every request the server received; the
/// answer is not inspected, only what went out.
async fn sent(model: &dyn Llm, request: LlmRequest, server: &MockServer) -> Vec<Value> {
    if let Ok(mut stream) = model.generate_content(request, false).await {
        while stream.next().await.is_some() {}
    }
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|request| serde_json::from_slice(&request.body).unwrap())
        .collect()
}

#[cfg(any(feature = "openai", feature = "openrouter", feature = "deepseek", feature = "groq"))]
fn chat_body() -> Value {
    json!({
        "id": "chatcmpl-1", "object": "chat.completion", "created": 0, "model": "fixture",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "Hi"},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 5, "completion_tokens": 1, "total_tokens": 6}
    })
}

#[cfg(any(
    feature = "anthropic",
    feature = "gemini",
    feature = "openrouter",
    feature = "deepseek",
    feature = "groq"
))]
async fn server_answering(body: Value) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;
    server
}

#[cfg(feature = "anthropic")]
mod anthropic {
    use super::*;
    use adk_model::anthropic::{AnthropicClient, AnthropicConfig, ThinkingMode};
    use adk_model::retry::RetryConfig;

    fn message_body() -> Value {
        json!({
            "id": "msg_1", "type": "message", "role": "assistant", "model": "fixture",
            "content": [{"type": "text", "text": "Hi"}],
            "stop_reason": "end_turn", "stop_sequence": null,
            "usage": {"input_tokens": 3, "output_tokens": 1}
        })
    }

    /// The `thinking` and `output_config` the client sent for one request.
    async fn posture(config: AnthropicConfig, effort: Option<Effort>) -> (Value, Value) {
        let server = server_answering(message_body()).await;
        let model = config.model.clone();
        let client = AnthropicClient::new(config.with_base_url(server.uri()))
            .unwrap()
            .with_retry_config(RetryConfig::disabled());
        let body = sent(&client, request(&model, effort), &server).await.remove(0);
        (body["thinking"].clone(), body["output_config"].clone())
    }

    #[tokio::test]
    async fn a_request_effort_is_sent_with_adaptive_thinking() {
        let (thinking, output) =
            posture(AnthropicConfig::new("test", "claude-sonnet-5-5"), Some(Effort::High)).await;
        assert_eq!((thinking, output), (json!({"type": "adaptive"}), json!({"effort": "high"})));
    }

    #[tokio::test]
    async fn a_request_effort_wins_over_the_client_effort() {
        let config = AnthropicConfig::new("test", "claude-opus-4-8")
            .with_thinking_mode(ThinkingMode::Adaptive)
            .with_effort(adk_model::anthropic::Effort::XHigh);
        assert_eq!(posture(config.clone(), Some(Effort::Low)).await.1, json!({"effort": "low"}));
        assert_eq!(posture(config, None).await.1, json!({"effort": "xhigh"}));
    }

    #[tokio::test]
    async fn opus_4_5_takes_the_effort_without_adaptive_thinking() {
        let (thinking, output) =
            posture(AnthropicConfig::new("test", "claude-opus-4-5"), Some(Effort::Medium)).await;
        assert_eq!((thinking, output), (Value::Null, json!({"effort": "medium"})));
    }

    #[tokio::test]
    async fn a_model_without_effort_control_gets_neither() {
        for model in ["claude-haiku-4-5", "claude-sonnet-4-5-20250929", "claude-3-5-haiku-20241022"]
        {
            let (thinking, output) =
                posture(AnthropicConfig::new("test", model), Some(Effort::High)).await;
            assert_eq!((thinking, output), (Value::Null, Value::Null), "{model}");
        }
    }
}

#[cfg(feature = "openai")]
mod openai {
    use super::*;
    use adk_model::openai::{
        OpenAIClient, OpenAIConfig, OpenAIReasoningEffort, OpenAIResponsesClient,
        OpenAIResponsesConfig, ReasoningEffort,
    };
    use adk_model::openai_compatible::{OpenAICompatible, OpenAICompatibleConfig};
    use adk_model::retry::RetryConfig;
    use wiremock::matchers::path;

    fn responses_body(model: &str) -> Value {
        json!({
            "id": "resp_1", "object": "response", "created_at": 0, "model": model,
            "status": "completed",
            "output": [{
                "type": "message", "id": "msg_1", "role": "assistant", "status": "completed",
                "content": [{"type": "output_text", "text": "Hi", "annotations": []}]
            }],
            "usage": {
                "input_tokens": 5, "input_tokens_details": {"cached_tokens": 0},
                "output_tokens": 1, "output_tokens_details": {"reasoning_tokens": 0},
                "total_tokens": 6
            }
        })
    }

    async fn both_endpoints(model: &str) -> MockServer {
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

    fn chat_client(
        server: &MockServer,
        model: &str,
        effort: Option<ReasoningEffort>,
    ) -> OpenAIClient {
        let mut config = OpenAIConfig::new("test-key", model);
        config.base_url = Some(server.uri());
        config.reasoning_effort = effort;
        OpenAIClient::new(config).unwrap().with_retry_config(RetryConfig::disabled())
    }

    fn with_tool(mut request: LlmRequest) -> LlmRequest {
        request.tools.insert(
            "lookup".to_string(),
            json!({"description": "Look up a word", "parameters": {"type": "object"}}),
        );
        request
    }

    #[tokio::test]
    async fn chat_completions_send_the_request_effort() {
        let server = both_endpoints("gpt-5.6-terra").await;
        let client = chat_client(&server, "gpt-5.6-terra", Some(ReasoningEffort::High));
        let bodies = sent(&client, request("gpt-5.6-terra", Some(Effort::Low)), &server).await;
        assert_eq!(bodies[0]["reasoning_effort"], "low");
        let bodies = sent(&client, request("gpt-5.6-terra", None), &server).await;
        assert_eq!(bodies[1]["reasoning_effort"], "high");
    }

    #[tokio::test]
    async fn a_routed_tool_call_sends_the_request_effort_to_responses() {
        let server = both_endpoints("gpt-5.6-terra").await;
        let client = chat_client(&server, "gpt-5.6-terra", None);
        let bodies =
            sent(&client, with_tool(request("gpt-5.6-terra", Some(Effort::High))), &server).await;
        let received = server.received_requests().await.unwrap();
        assert_eq!(received[0].url.path(), "/responses");
        assert_eq!(bodies[0]["reasoning"]["effort"], "high");
    }

    #[tokio::test]
    async fn a_request_effort_turns_reasoning_on_for_a_client_built_without_it() {
        let server = both_endpoints("gpt-5.6-terra").await;
        let mut config = OpenAIConfig::new("test-key", "gpt-5.6-terra");
        config.base_url = Some(server.uri());
        let client = OpenAIClient::new_with_reasoning_effort(config, OpenAIReasoningEffort::None)
            .unwrap()
            .with_retry_config(RetryConfig::disabled());
        sent(&client, with_tool(request("gpt-5.6-terra", None)), &server).await;
        sent(&client, with_tool(request("gpt-5.6-terra", Some(Effort::Medium))), &server).await;
        let received = server.received_requests().await.unwrap();
        let routes: Vec<&str> = received.iter().map(|request| request.url.path()).collect();
        assert_eq!(routes, vec!["/chat/completions", "/responses"]);
        let routed: Value = serde_json::from_slice(&received[1].body).unwrap();
        assert_eq!(routed["reasoning"]["effort"], "medium");
    }

    #[tokio::test]
    async fn a_model_without_reasoning_gets_no_effort() {
        let server = both_endpoints("gpt-4.1").await;
        let client = chat_client(&server, "gpt-4.1", None);
        let bodies = sent(&client, request("gpt-4.1", Some(Effort::High)), &server).await;
        assert_eq!(bodies[0].get("reasoning_effort"), None);

        let compatible = OpenAICompatible::new(
            OpenAICompatibleConfig::new("key", "llama-4-scout").with_base_url(server.uri()),
        )
        .unwrap()
        .with_retry_config(RetryConfig::disabled());
        let bodies = sent(&compatible, request("llama-4-scout", Some(Effort::High)), &server).await;
        assert_eq!(bodies[1].get("reasoning_effort"), None);
    }

    #[tokio::test]
    async fn responses_send_the_request_effort_over_a_max_client() {
        let server = both_endpoints("gpt-5.6-terra").await;
        let mut config = OpenAIResponsesConfig::new("test-key", "gpt-5.6-terra");
        config.base_url = Some(server.uri());
        let client =
            OpenAIResponsesClient::new_with_reasoning_effort(config, OpenAIReasoningEffort::Max)
                .unwrap()
                .with_retry_config(RetryConfig::disabled());
        let bodies = sent(&client, request("gpt-5.6-terra", Some(Effort::Low)), &server).await;
        assert_eq!(bodies[0]["reasoning"]["effort"], "low");
        let bodies = sent(&client, request("gpt-5.6-terra", None), &server).await;
        assert_eq!(bodies[1]["reasoning"]["effort"], "max");
    }
}

#[cfg(feature = "gemini")]
mod gemini {
    use super::*;
    use adk_model::GeminiModel;
    use adk_model::gemini::{ThinkingConfig, ThinkingLevel};

    fn gemini_body() -> Value {
        json!({
            "candidates": [{
                "content": {"role": "model", "parts": [{"text": "Hi"}]},
                "finishReason": "STOP"
            }],
            "usageMetadata": {"promptTokenCount": 3, "candidatesTokenCount": 1, "totalTokenCount": 4}
        })
    }

    async fn thinking_config(
        model: GeminiModel,
        server: &MockServer,
        effort: Option<Effort>,
    ) -> Value {
        let name = model.name().to_string();
        let bodies = sent(&model, request(&name, effort), server).await;
        bodies.last().unwrap()["generationConfig"]["thinkingConfig"].clone()
    }

    fn model(server: &MockServer, name: &str) -> GeminiModel {
        GeminiModel::new_with_base_url("test-key", name, format!("{}/v1beta/", server.uri()))
            .unwrap()
    }

    #[tokio::test]
    async fn gemini_3_sends_the_request_effort_as_thinking_level() {
        let server = server_answering(gemini_body()).await;
        let sent =
            thinking_config(model(&server, "gemini-3.5-flash-lite"), &server, Some(Effort::High))
                .await;
        assert_eq!(sent, json!({"thinkingLevel": "high"}));
    }

    #[tokio::test]
    async fn a_request_effort_wins_over_the_configured_level() {
        let server = server_answering(gemini_body()).await;
        let configured = || {
            model(&server, "gemini-3.5-flash-lite").with_thinking_config(
                ThinkingConfig::new()
                    .with_thinking_level(ThinkingLevel::Low)
                    .with_thoughts_included(true),
            )
        };
        assert_eq!(
            thinking_config(configured(), &server, Some(Effort::Medium)).await,
            json!({"thinkingLevel": "medium", "includeThoughts": true})
        );
        assert_eq!(
            thinking_config(configured(), &server, None).await,
            json!({"thinkingLevel": "low", "includeThoughts": true})
        );
    }

    #[tokio::test]
    async fn gemini_2_5_keeps_its_budget() {
        let server = server_answering(gemini_body()).await;
        let budgeted = model(&server, "gemini-2.5-flash")
            .with_thinking_config(ThinkingConfig::new().with_thinking_budget(512));
        assert_eq!(
            thinking_config(budgeted, &server, Some(Effort::High)).await,
            json!({"thinkingBudget": 512})
        );
    }
}

#[cfg(feature = "openrouter")]
mod openrouter {
    use super::*;
    use adk_model::openrouter::{
        OpenRouterApiMode, OpenRouterClient, OpenRouterConfig, OpenRouterReasoningConfig,
        OpenRouterRequestOptions,
    };

    fn client(server: &MockServer) -> OpenRouterClient {
        OpenRouterClient::new(
            OpenRouterConfig::new("key", "openai/gpt-5.6-terra")
                .with_base_url(format!("{}/api/v1", server.uri()))
                .with_default_api_mode(OpenRouterApiMode::ChatCompletions),
        )
        .unwrap()
        .with_retry_config(adk_model::RetryConfig::disabled())
    }

    #[tokio::test]
    async fn the_request_effort_becomes_reasoning_effort_unless_the_options_choose() {
        let server = server_answering(chat_body()).await;
        let client = client(&server);
        sent(&client, request("openai/gpt-5.6-terra", Some(Effort::Low)), &server).await;

        let mut chosen = request("openai/gpt-5.6-terra", Some(Effort::Low));
        OpenRouterRequestOptions::default()
            .with_reasoning(OpenRouterReasoningConfig {
                effort: Some("high".to_string()),
                ..Default::default()
            })
            .insert_into_config(chosen.config.as_mut().unwrap())
            .unwrap();
        let bodies = sent(&client, chosen, &server).await;

        let efforts: Vec<&Value> = bodies.iter().map(|body| &body["reasoning"]).collect();
        assert_eq!(efforts, vec![&json!({"effort": "low"}), &json!({"effort": "high"})]);
    }
}

#[cfg(feature = "deepseek")]
mod deepseek {
    use super::*;
    use adk_model::deepseek::{DeepSeekClient, DeepSeekConfig, ReasoningEffort, ThinkingMode};

    #[tokio::test]
    async fn the_request_effort_wins_unless_thinking_is_off() {
        let server = server_answering(chat_body()).await;
        let client = |config: DeepSeekConfig| {
            DeepSeekClient::new(config.with_base_url(server.uri())).unwrap()
        };
        let max = DeepSeekConfig::new("key", "deepseek-v4-pro")
            .with_thinking_mode(ThinkingMode::Enabled)
            .with_reasoning_effort(ReasoningEffort::Max);
        sent(&client(max.clone()), request("deepseek-v4-pro", Some(Effort::Medium)), &server).await;
        sent(&client(max), request("deepseek-v4-pro", None), &server).await;
        let off = DeepSeekConfig::new("key", "deepseek-v4-pro")
            .with_thinking_mode(ThinkingMode::Disabled);
        let bodies =
            sent(&client(off), request("deepseek-v4-pro", Some(Effort::High)), &server).await;

        let efforts: Vec<Option<&Value>> =
            bodies.iter().map(|body| body.get("reasoning_effort")).collect();
        assert_eq!(efforts, vec![Some(&json!("high")), Some(&json!("max")), None]);
    }
}

#[cfg(feature = "groq")]
mod groq {
    use super::*;
    use adk_model::{GroqClient, GroqConfig};

    #[tokio::test]
    async fn gpt_oss_takes_the_request_effort_and_other_models_do_not() {
        let server = server_answering(chat_body()).await;
        for model in ["openai/gpt-oss-120b", "llama-3.3-70b-versatile"] {
            let client =
                GroqClient::new(GroqConfig::new("key", model).with_base_url(server.uri())).unwrap();
            sent(&client, request(model, Some(Effort::Medium)), &server).await;
        }
        let bodies = sent_bodies(&server).await;
        let efforts: Vec<Option<&Value>> =
            bodies.iter().map(|body| body.get("reasoning_effort")).collect();
        assert_eq!(efforts, vec![Some(&json!("medium")), None]);
    }

    async fn sent_bodies(server: &MockServer) -> Vec<Value> {
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .map(|request| serde_json::from_slice(&request.body).unwrap())
            .collect()
    }
}
