use adk_core::{AdkError, Content, Llm, LlmRequest, LlmResponse, Part, Result};
use adk_model::RetryConfig;
use futures::StreamExt;
use serde_json::json;
use std::env;
use std::time::Duration;

#[cfg(feature = "anthropic")]
use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
#[cfg(feature = "deepseek")]
use adk_model::deepseek::{DeepSeekClient, DeepSeekConfig};
#[cfg(feature = "gemini")]
use adk_model::gemini::GeminiModel;
#[cfg(feature = "groq")]
use adk_model::groq::{GroqClient, GroqConfig};
#[cfg(feature = "ollama")]
use adk_model::ollama::{OllamaConfig, OllamaModel};
#[cfg(feature = "openai")]
use adk_model::openai::{OpenAIClient, OpenAIConfig};
#[cfg(feature = "xai")]
use adk_model::xai::{XAIClient, XAIConfig};

type BuildModelFn = fn(&str) -> Result<Box<dyn Llm>>;

#[derive(Clone, Copy)]
struct ProviderSpec {
    name: &'static str,
    model_env_candidates: &'static [&'static str],
    default_model: &'static str,
    required_envs: &'static [&'static str],
    supports_tools: bool,
    build_model: BuildModelFn,
}

impl ProviderSpec {
    fn configured_model_name(self) -> Option<String> {
        let missing: Vec<&str> =
            self.required_envs.iter().copied().filter(|var| env::var(var).is_err()).collect();

        if !missing.is_empty() {
            println!(
                "Skipping {} integration tests: missing env vars: {}",
                self.name,
                missing.join(", ")
            );
            return None;
        }

        for model_env in self.model_env_candidates {
            if let Ok(model_name) = env::var(model_env) {
                let trimmed = model_name.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }

        Some(self.default_model.to_string())
    }
}

fn required_env(var: &str) -> Result<String> {
    env::var(var).map_err(|_| AdkError::Model(format!("missing required env var: {var}")))
}

fn base_request(model_name: &str, prompt: &str) -> LlmRequest {
    let content = Content::new("user").with_text(prompt);
    LlmRequest::new(model_name, vec![content])
}

fn tools_request(model_name: &str) -> LlmRequest {
    let mut request = base_request(
        model_name,
        "A tool named get_weather is available. If a tool is required, call get_weather with city=\"Boston\".",
    );
    request.tools.insert(
        "get_weather".to_string(),
        json!({
            "name": "get_weather",
            "description": "Get current weather for a city.",
            "parameters": {
                "type": "object",
                "properties": {
                    "city": { "type": "string" }
                },
                "required": ["city"]
            }
        }),
    );
    request
}

fn response_has_text(response: &LlmResponse) -> bool {
    response.content.as_ref().is_some_and(|content| {
        content
            .parts
            .iter()
            .any(|part| matches!(part, Part::Text { text } if !text.trim().is_empty()))
    })
}

fn response_has_content_parts(response: &LlmResponse) -> bool {
    response.content.as_ref().is_some_and(|content| !content.parts.is_empty())
}

fn collect_function_calls(
    responses: &[LlmResponse],
) -> Vec<(String, serde_json::Value, Option<String>)> {
    responses
        .iter()
        .flat_map(|response| response.content.as_ref().into_iter())
        .flat_map(|content| content.parts.iter())
        .filter_map(|part| {
            if let Part::FunctionCall { name, args, id } = part {
                Some((name.clone(), args.clone(), id.clone()))
            } else {
                None
            }
        })
        .collect()
}

fn assert_response_invariants(spec: ProviderSpec, mode: &str, responses: &[LlmResponse]) {
    assert!(!responses.is_empty(), "{} {mode} should yield at least one response", spec.name);

    for (index, response) in responses.iter().enumerate() {
        assert!(
            response.error_code.is_none(),
            "{} {mode} chunk #{index} unexpectedly has error_code={:?}",
            spec.name,
            response.error_code
        );
        assert!(
            response.error_message.is_none(),
            "{} {mode} chunk #{index} unexpectedly has error_message={:?}",
            spec.name,
            response.error_message
        );
        assert!(
            !(response.partial && response.turn_complete),
            "{} {mode} chunk #{index} cannot be both partial and turn_complete",
            spec.name
        );

        if let Some(content) = &response.content {
            assert_eq!(
                content.role, "model",
                "{} {mode} chunk #{index} should use role=model when content is present",
                spec.name
            );
            assert!(
                !content.parts.is_empty(),
                "{} {mode} chunk #{index} content must include at least one part",
                spec.name
            );
        }
    }

    let final_indices: Vec<usize> = responses
        .iter()
        .enumerate()
        .filter_map(|(index, response)| response.turn_complete.then_some(index))
        .collect();

    assert_eq!(final_indices.len(), 1, "{} {mode} should have exactly one final chunk", spec.name);

    let final_index = final_indices[0];
    assert_eq!(final_index, responses.len() - 1, "{} {mode} final chunk should be last", spec.name);

    let final_response = &responses[final_index];
    assert!(!final_response.partial, "{} {mode} final chunk must have partial=false", spec.name);
    assert!(
        final_response.finish_reason.is_some(),
        "{} {mode} final chunk should include finish_reason",
        spec.name
    );
}

async fn run_non_streaming_contract(spec: ProviderSpec) {
    let Some(model_name) = spec.configured_model_name() else {
        return;
    };

    let model = (spec.build_model)(&model_name)
        .unwrap_or_else(|err| panic!("{} model construction failed: {err}", spec.name));

    let request = base_request(&model_name, "Reply with exactly one short greeting.");
    let mut stream = model
        .generate_content(request, false)
        .await
        .unwrap_or_else(|err| panic!("{} non-streaming request failed: {err}", spec.name));

    let mut responses = Vec::new();

    while let Some(item) = stream.next().await {
        let response =
            item.unwrap_or_else(|err| panic!("{} non-streaming chunk failed: {err}", spec.name));
        responses.push(response);
    }

    assert_response_invariants(spec, "non-streaming", &responses);
    assert!(
        responses.iter().any(response_has_text),
        "{} non-streaming should emit non-empty text for a basic prompt",
        spec.name
    );
}

async fn run_streaming_contract(spec: ProviderSpec) {
    let Some(model_name) = spec.configured_model_name() else {
        return;
    };

    let model = (spec.build_model)(&model_name)
        .unwrap_or_else(|err| panic!("{} model construction failed: {err}", spec.name));

    let request = base_request(&model_name, "Count from 1 to 5.");
    let mut stream = model
        .generate_content(request, true)
        .await
        .unwrap_or_else(|err| panic!("{} streaming request failed: {err}", spec.name));

    let mut responses = Vec::new();

    while let Some(item) = stream.next().await {
        let response =
            item.unwrap_or_else(|err| panic!("{} streaming chunk failed: {err}", spec.name));
        responses.push(response);
    }

    assert_response_invariants(spec, "streaming", &responses);
    assert!(
        responses.iter().any(response_has_text),
        "{} streaming should emit text content",
        spec.name
    );
    assert!(
        responses.iter().any(|response| response.partial),
        "{} streaming should emit at least one partial chunk",
        spec.name
    );
}

async fn run_tools_contract(spec: ProviderSpec) {
    if !spec.supports_tools {
        println!("Skipping {} tool contract: tools not supported", spec.name);
        return;
    }

    let Some(model_name) = spec.configured_model_name() else {
        return;
    };

    let model = (spec.build_model)(&model_name)
        .unwrap_or_else(|err| panic!("{} model construction failed: {err}", spec.name));

    let request = tools_request(&model_name);
    let mut stream = model
        .generate_content(request, false)
        .await
        .unwrap_or_else(|err| panic!("{} tools request failed: {err}", spec.name));

    let mut responses = Vec::new();

    while let Some(item) = stream.next().await {
        let response = item.unwrap_or_else(|err| panic!("{} tools chunk failed: {err}", spec.name));
        responses.push(response);
    }

    assert_response_invariants(spec, "tools", &responses);
    assert!(
        !responses.is_empty(),
        "{} should return at least one response when tools are declared",
        spec.name
    );
    assert!(
        responses.iter().any(response_has_content_parts),
        "{} tool-enabled request should return content",
        spec.name
    );

    let function_calls = collect_function_calls(&responses);
    for (name, args, id) in &function_calls {
        assert!(!name.trim().is_empty(), "{} tool call name must be non-empty", spec.name);
        assert!(args.is_object(), "{} tool call args should be a JSON object", spec.name);
        if let Some(call_id) = id {
            assert!(!call_id.trim().is_empty(), "{} tool call id must be non-empty", spec.name);
        }
    }

    if !function_calls.is_empty() {
        assert!(
            function_calls.iter().any(|(name, _, _)| name == "get_weather"),
            "{} emitted function calls, but none targeted declared tool get_weather",
            spec.name
        );
    }
}

macro_rules! provider_contract_tests {
    ($module:ident, $spec_fn:ident) => {
        mod $module {
            use super::*;

            #[tokio::test]
            #[ignore = "integration test; requires provider credentials"]
            async fn non_streaming_contract() {
                run_non_streaming_contract($spec_fn()).await;
            }

            #[tokio::test]
            #[ignore = "integration test; requires provider credentials"]
            async fn streaming_contract() {
                run_streaming_contract($spec_fn()).await;
            }

            #[tokio::test]
            #[ignore = "integration test; requires provider credentials"]
            async fn tool_declaration_contract() {
                run_tools_contract($spec_fn()).await;
            }
        }
    };
}

#[cfg(feature = "gemini")]
fn gemini_cheapest_spec() -> ProviderSpec {
    ProviderSpec {
        name: "gemini-cheapest",
        model_env_candidates: &["GEMINI_CHEAPEST_MODEL", "GEMINI_MODEL"],
        default_model: "gemini-2.5-flash-lite",
        required_envs: &["GEMINI_API_KEY"],
        supports_tools: true,
        build_model: |model_name| {
            let api_key = required_env("GEMINI_API_KEY")?;
            Ok(Box::new(GeminiModel::new(api_key, model_name)?))
        },
    }
}

#[cfg(feature = "openai")]
fn openai_cheapest_spec() -> ProviderSpec {
    ProviderSpec {
        name: "openai-cheapest",
        model_env_candidates: &["OPENAI_CHEAPEST_MODEL", "OPENAI_MODEL"],
        default_model: "gpt-5-mini",
        required_envs: &["OPENAI_API_KEY"],
        supports_tools: true,
        build_model: |model_name| {
            let api_key = required_env("OPENAI_API_KEY")?;
            Ok(Box::new(OpenAIClient::new(OpenAIConfig::new(api_key, model_name))?))
        },
    }
}

#[cfg(feature = "xai")]
fn xai_cheapest_spec() -> ProviderSpec {
    ProviderSpec {
        name: "xai-cheapest",
        model_env_candidates: &["XAI_CHEAPEST_MODEL", "XAI_MODEL"],
        default_model: "grok-3-fast",
        required_envs: &["XAI_API_KEY"],
        supports_tools: true,
        build_model: |model_name| {
            let api_key = required_env("XAI_API_KEY")?;
            Ok(Box::new(XAIClient::new(XAIConfig::new(api_key, model_name))?))
        },
    }
}

#[cfg(feature = "anthropic")]
fn anthropic_cheapest_spec() -> ProviderSpec {
    ProviderSpec {
        name: "anthropic-cheapest",
        model_env_candidates: &["ANTHROPIC_CHEAPEST_MODEL", "ANTHROPIC_MODEL"],
        default_model: "claude-haiku-3.5",
        required_envs: &["ANTHROPIC_API_KEY"],
        supports_tools: true,
        build_model: |model_name| {
            let api_key = required_env("ANTHROPIC_API_KEY")?;
            Ok(Box::new(AnthropicClient::new(AnthropicConfig::new(api_key, model_name))?))
        },
    }
}

#[cfg(feature = "deepseek")]
fn deepseek_cheapest_spec() -> ProviderSpec {
    ProviderSpec {
        name: "deepseek-cheapest",
        model_env_candidates: &["DEEPSEEK_CHEAPEST_MODEL", "DEEPSEEK_MODEL"],
        default_model: "deepseek-chat",
        required_envs: &["DEEPSEEK_API_KEY"],
        supports_tools: true,
        build_model: |model_name| {
            let api_key = required_env("DEEPSEEK_API_KEY")?;
            Ok(Box::new(DeepSeekClient::new(DeepSeekConfig::new(api_key, model_name))?))
        },
    }
}

#[cfg(feature = "groq")]
fn groq_cheapest_spec() -> ProviderSpec {
    ProviderSpec {
        name: "groq-cheapest",
        model_env_candidates: &["GROQ_CHEAPEST_MODEL", "GROQ_MODEL"],
        default_model: "llama-3.1-8b-instant",
        required_envs: &["GROQ_API_KEY"],
        supports_tools: true,
        build_model: |model_name| {
            let api_key = required_env("GROQ_API_KEY")?;
            Ok(Box::new(GroqClient::new(GroqConfig::new(api_key, model_name))?))
        },
    }
}

#[cfg(feature = "ollama")]
fn ollama_cheapest_spec() -> ProviderSpec {
    ProviderSpec {
        name: "ollama-cheapest",
        model_env_candidates: &["OLLAMA_CHEAPEST_MODEL", "OLLAMA_MODEL"],
        default_model: "qwen2.5:7b",
        required_envs: &["OLLAMA_HOST"],
        supports_tools: true,
        build_model: |model_name| {
            let host = required_env("OLLAMA_HOST")?;
            Ok(Box::new(OllamaModel::new(OllamaConfig::with_host(host, model_name))?))
        },
    }
}

#[cfg(feature = "gemini")]
provider_contract_tests!(gemini_cheapest_provider, gemini_cheapest_spec);
#[cfg(feature = "openai")]
provider_contract_tests!(openai_cheapest_provider, openai_cheapest_spec);
#[cfg(feature = "xai")]
provider_contract_tests!(xai_cheapest_provider, xai_cheapest_spec);
#[cfg(feature = "anthropic")]
provider_contract_tests!(anthropic_cheapest_provider, anthropic_cheapest_spec);
#[cfg(feature = "deepseek")]
provider_contract_tests!(deepseek_cheapest_provider, deepseek_cheapest_spec);
#[cfg(feature = "groq")]
provider_contract_tests!(groq_cheapest_provider, groq_cheapest_spec);
#[cfg(feature = "ollama")]
provider_contract_tests!(ollama_cheapest_provider, ollama_cheapest_spec);

#[test]
fn llm_request_creation_is_provider_agnostic() {
    let content = Content::new("user").with_text("Hello");
    let request = LlmRequest::new("test-model", vec![content]);

    assert_eq!(request.model, "test-model");
    assert_eq!(request.contents.len(), 1);
    assert!(request.tools.is_empty());
}

#[test]
fn retry_config_builder_is_additive() {
    let retry_config = RetryConfig::default()
        .with_max_retries(5)
        .with_initial_delay(Duration::from_millis(50))
        .with_max_delay(Duration::from_secs(1));

    assert!(retry_config.enabled);
    assert_eq!(retry_config.max_retries, 5);
    assert_eq!(retry_config.initial_delay, Duration::from_millis(50));
    assert_eq!(retry_config.max_delay, Duration::from_secs(1));
}
