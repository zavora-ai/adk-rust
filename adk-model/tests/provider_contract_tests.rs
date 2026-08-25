#![allow(clippy::result_large_err)]
use adk_core::{
    AdkError, Content, GenerateContentConfig, Llm, LlmRequest, LlmResponse, Part, Result,
};
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
use adk_model::openai::{
    AzureConfig, AzureOpenAIClient, OpenAIClient, OpenAIConfig, OpenAIReasoningEffort,
    OpenAIResponsesClient, OpenAIResponsesConfig,
};
#[cfg(feature = "openrouter")]
use adk_model::openrouter::{OpenRouterApiMode, OpenRouterClient, OpenRouterConfig};

#[cfg(feature = "azure-ai")]
use adk_model::azure_ai::{AzureAIClient, AzureAIConfig};
#[cfg(feature = "bedrock")]
use adk_model::bedrock::{BedrockClient, BedrockConfig};
#[cfg(feature = "openai")]
use adk_model::openai_compatible::{OpenAICompatible, OpenAICompatibleConfig};

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
    env::var(var).map_err(|_| AdkError::model(format!("missing required env var: {var}")))
}

fn base_request(model_name: &str, prompt: &str) -> LlmRequest {
    let content = Content::new("user").with_text(prompt);
    LlmRequest::new(model_name, vec![content])
        .with_config(GenerateContentConfig { max_output_tokens: Some(256), ..Default::default() })
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
            if let Part::FunctionCall { name, args, id, .. } = part {
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

    let has_function_calls = responses.iter().any(|response| {
        response.content.as_ref().is_some_and(|content| content.has_function_calls())
    });
    if mode == "tools" && has_function_calls {
        assert!(
            final_indices.is_empty(),
            "{} tool-call turn must remain open for tool execution",
            spec.name
        );
        let last = responses.last().expect("responses are non-empty");
        assert!(!last.partial, "{} tool-call protocol chunk must not be partial", spec.name);
        assert!(
            last.finish_reason.is_some(),
            "{} tool-call protocol chunk should include finish_reason",
            spec.name
        );
        return;
    }

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

            #[tokio::test(flavor = "multi_thread")]
            #[ignore = "integration test; requires provider credentials"]
            async fn non_streaming_contract() {
                run_non_streaming_contract($spec_fn()).await;
            }

            #[tokio::test(flavor = "multi_thread")]
            #[ignore = "integration test; requires provider credentials"]
            async fn streaming_contract() {
                run_streaming_contract($spec_fn()).await;
            }

            #[tokio::test(flavor = "multi_thread")]
            #[ignore = "integration test; requires provider credentials"]
            async fn tool_declaration_contract() {
                run_tools_contract($spec_fn()).await;
            }
        }
    };
}

#[cfg(feature = "gemini")]
fn gemini_default_spec() -> ProviderSpec {
    ProviderSpec {
        name: "gemini-default",
        model_env_candidates: &["GEMINI_MODEL", "GEMINI_CHEAPEST_MODEL"],
        default_model: adk_model::catalog::GEMINI_DEFAULT,
        required_envs: &["GEMINI_API_KEY"],
        supports_tools: true,
        build_model: |model_name| {
            let api_key = required_env("GEMINI_API_KEY")?;
            Ok(Box::new(GeminiModel::new(api_key, model_name)?))
        },
    }
}

#[cfg(feature = "openai")]
fn openai_default_spec() -> ProviderSpec {
    ProviderSpec {
        name: "openai-default",
        model_env_candidates: &["OPENAI_MODEL", "OPENAI_CHEAPEST_MODEL"],
        default_model: adk_model::catalog::OPENAI_DEFAULT,
        required_envs: &["OPENAI_API_KEY"],
        supports_tools: true,
        build_model: |model_name| {
            let api_key = required_env("OPENAI_API_KEY")?;
            Ok(Box::new(OpenAIClient::new(OpenAIConfig::new(api_key, model_name))?))
        },
    }
}

#[cfg(feature = "openai")]
fn xai_default_spec() -> ProviderSpec {
    ProviderSpec {
        name: "xai-default",
        model_env_candidates: &["XAI_MODEL", "XAI_CHEAPEST_MODEL"],
        default_model: adk_model::catalog::XAI_DEFAULT,
        required_envs: &["XAI_API_KEY"],
        supports_tools: true,
        build_model: |model_name| {
            let api_key = required_env("XAI_API_KEY")?;
            Ok(Box::new(OpenAICompatible::new(OpenAICompatibleConfig::xai(api_key, model_name))?))
        },
    }
}

#[cfg(feature = "anthropic")]
fn anthropic_default_spec() -> ProviderSpec {
    ProviderSpec {
        name: "anthropic-default",
        model_env_candidates: &["ANTHROPIC_MODEL", "ANTHROPIC_CHEAPEST_MODEL"],
        default_model: adk_model::catalog::ANTHROPIC_DEFAULT,
        required_envs: &["ANTHROPIC_API_KEY"],
        supports_tools: true,
        build_model: |model_name| {
            let api_key = required_env("ANTHROPIC_API_KEY")?;
            Ok(Box::new(AnthropicClient::new(AnthropicConfig::new(api_key, model_name))?))
        },
    }
}

#[cfg(feature = "deepseek")]
fn deepseek_default_spec() -> ProviderSpec {
    ProviderSpec {
        name: "deepseek-default",
        model_env_candidates: &["DEEPSEEK_MODEL", "DEEPSEEK_CHEAPEST_MODEL"],
        default_model: adk_model::catalog::DEEPSEEK_DEFAULT,
        required_envs: &["DEEPSEEK_API_KEY"],
        supports_tools: true,
        build_model: |model_name| {
            let api_key = required_env("DEEPSEEK_API_KEY")?;
            Ok(Box::new(DeepSeekClient::new(DeepSeekConfig::new(api_key, model_name))?))
        },
    }
}

#[cfg(feature = "groq")]
fn groq_default_spec() -> ProviderSpec {
    ProviderSpec {
        name: "groq-default",
        model_env_candidates: &["GROQ_MODEL", "GROQ_CHEAPEST_MODEL"],
        default_model: adk_model::catalog::GROQ_DEFAULT,
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
        default_model: adk_model::catalog::OLLAMA_DEFAULT,
        required_envs: &["OLLAMA_HOST"],
        supports_tools: true,
        build_model: |model_name| {
            let host = required_env("OLLAMA_HOST")?;
            Ok(Box::new(OllamaModel::new(OllamaConfig::with_host(host, model_name))?))
        },
    }
}

#[cfg(feature = "openrouter")]
fn openrouter_cheapest_spec() -> ProviderSpec {
    ProviderSpec {
        name: "openrouter-cheapest",
        model_env_candidates: &["OPENROUTER_CHEAPEST_MODEL", "OPENROUTER_MODEL"],
        default_model: adk_model::catalog::OPENROUTER_DEFAULT,
        required_envs: &["OPENROUTER_API_KEY"],
        supports_tools: true,
        build_model: |model_name| {
            let api_key = required_env("OPENROUTER_API_KEY")?;
            Ok(Box::new(OpenRouterClient::new(
                OpenRouterConfig::new(api_key, model_name)
                    .with_http_referer("https://github.com/zavora-ai/adk-rust")
                    .with_title("ADK-Rust Provider Contract Tests")
                    .with_default_api_mode(OpenRouterApiMode::ChatCompletions),
            )?))
        },
    }
}

#[cfg(feature = "gemini")]
provider_contract_tests!(gemini_default_provider, gemini_default_spec);
#[cfg(feature = "openai")]
provider_contract_tests!(openai_default_provider, openai_default_spec);

#[cfg(feature = "openai")]
mod openai_current_reasoning_contract {
    use super::*;

    async fn assert_reasoning_response(
        model: Box<dyn Llm>,
        api_name: &str,
        effort: &str,
        stream: bool,
    ) {
        let request =
            base_request(adk_model::catalog::OPENAI_DEFAULT, "Reply with exactly the word OK.");
        let mut stream = model
            .generate_content(request, stream)
            .await
            .unwrap_or_else(|error| panic!("OpenAI {api_name} {effort} request failed: {error}"));
        let mut responses = Vec::new();
        while let Some(response) = stream.next().await {
            responses.push(response.unwrap_or_else(|error| {
                panic!("OpenAI {api_name} {effort} chunk failed: {error}")
            }));
        }
        assert!(
            responses.iter().any(response_has_text),
            "OpenAI {api_name} {effort} request should return text"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "live contract test; requires OpenAI credentials"]
    async fn chat_completions_accepts_xhigh() {
        let api_key = required_env("OPENAI_API_KEY").expect("OPENAI_API_KEY is required");
        let config = OpenAIConfig::new(api_key, adk_model::catalog::OPENAI_DEFAULT);
        let model = OpenAIClient::new_with_reasoning_effort(config, OpenAIReasoningEffort::XHigh)
            .expect("OpenAI Chat client should build");
        assert_reasoning_response(Box::new(model), "Chat Completions", "xhigh", false).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "live contract test; requires OpenAI credentials"]
    async fn responses_accepts_max() {
        let api_key = required_env("OPENAI_API_KEY").expect("OPENAI_API_KEY is required");
        let config = OpenAIResponsesConfig::new(api_key, adk_model::catalog::OPENAI_DEFAULT);
        let model =
            OpenAIResponsesClient::new_with_reasoning_effort(config, OpenAIReasoningEffort::Max)
                .expect("OpenAI Responses client should build");
        assert_reasoning_response(Box::new(model), "Responses", "max", false).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "live contract test; requires OpenAI credentials"]
    async fn responses_streaming_accepts_max() {
        let api_key = required_env("OPENAI_API_KEY").expect("OPENAI_API_KEY is required");
        let config = OpenAIResponsesConfig::new(api_key, adk_model::catalog::OPENAI_DEFAULT);
        let model =
            OpenAIResponsesClient::new_with_reasoning_effort(config, OpenAIReasoningEffort::Max)
                .expect("OpenAI Responses client should build");
        assert_reasoning_response(Box::new(model), "streaming Responses", "max", true).await;
    }
}

#[cfg(feature = "openai")]
fn azure_openai_spec() -> ProviderSpec {
    ProviderSpec {
        name: "azure-openai",
        model_env_candidates: &["AZURE_OPENAI_DEPLOYMENT"],
        default_model: "mistral-small-2503",
        required_envs: &[
            "AZURE_OPENAI_ENDPOINT",
            "AZURE_OPENAI_API_KEY",
            "AZURE_OPENAI_DEPLOYMENT",
        ],
        supports_tools: true,
        build_model: |model_name| {
            let api_key = required_env("AZURE_OPENAI_API_KEY")?;
            let endpoint = required_env("AZURE_OPENAI_ENDPOINT")?;
            let api_version = env::var("AZURE_OPENAI_API_VERSION")
                .unwrap_or_else(|_| "2024-12-01-preview".to_string());
            let config = AzureConfig::new(api_key, endpoint, api_version, model_name);
            Ok(Box::new(AzureOpenAIClient::new(config)?))
        },
    }
}

#[cfg(feature = "openai")]
provider_contract_tests!(azure_openai_provider, azure_openai_spec);
#[cfg(feature = "openai")]
provider_contract_tests!(xai_default_provider, xai_default_spec);
#[cfg(feature = "anthropic")]
provider_contract_tests!(anthropic_default_provider, anthropic_default_spec);
#[cfg(feature = "deepseek")]
provider_contract_tests!(deepseek_default_provider, deepseek_default_spec);
#[cfg(feature = "groq")]
provider_contract_tests!(groq_default_provider, groq_default_spec);
#[cfg(feature = "ollama")]
provider_contract_tests!(ollama_cheapest_provider, ollama_cheapest_spec);
#[cfg(feature = "openrouter")]
provider_contract_tests!(openrouter_cheapest_provider, openrouter_cheapest_spec);

#[cfg(feature = "openai")]
fn fireworks_cheapest_spec() -> ProviderSpec {
    ProviderSpec {
        name: "fireworks-cheapest",
        model_env_candidates: &["FIREWORKS_CHEAPEST_MODEL", "FIREWORKS_MODEL"],
        default_model: adk_model::catalog::FIREWORKS_DEFAULT,
        required_envs: &["FIREWORKS_API_KEY"],
        supports_tools: true,
        build_model: |model_name| {
            let api_key = required_env("FIREWORKS_API_KEY")?;
            Ok(Box::new(OpenAICompatible::new(OpenAICompatibleConfig::fireworks(
                api_key, model_name,
            ))?))
        },
    }
}

#[cfg(feature = "openai")]
fn together_cheapest_spec() -> ProviderSpec {
    ProviderSpec {
        name: "together-cheapest",
        model_env_candidates: &["TOGETHER_CHEAPEST_MODEL", "TOGETHER_MODEL"],
        default_model: adk_model::catalog::TOGETHER_DEFAULT,
        required_envs: &["TOGETHER_API_KEY"],
        supports_tools: true,
        build_model: |model_name| {
            let api_key = required_env("TOGETHER_API_KEY")?;
            Ok(Box::new(OpenAICompatible::new(OpenAICompatibleConfig::together(
                api_key, model_name,
            ))?))
        },
    }
}

#[cfg(feature = "openai")]
fn mistral_cheapest_spec() -> ProviderSpec {
    ProviderSpec {
        name: "mistral-cheapest",
        model_env_candidates: &["MISTRAL_CHEAPEST_MODEL", "MISTRAL_MODEL"],
        default_model: adk_model::catalog::MISTRAL_DEFAULT,
        required_envs: &["MISTRAL_API_KEY"],
        supports_tools: true,
        build_model: |model_name| {
            let api_key = required_env("MISTRAL_API_KEY")?;
            Ok(Box::new(OpenAICompatible::new(OpenAICompatibleConfig::mistral(
                api_key, model_name,
            ))?))
        },
    }
}

#[cfg(feature = "openai")]
fn perplexity_cheapest_spec() -> ProviderSpec {
    ProviderSpec {
        name: "perplexity-cheapest",
        model_env_candidates: &["PERPLEXITY_CHEAPEST_MODEL", "PERPLEXITY_MODEL"],
        default_model: adk_model::catalog::PERPLEXITY_DEFAULT,
        required_envs: &["PERPLEXITY_API_KEY"],
        supports_tools: false,
        build_model: |model_name| {
            let api_key = required_env("PERPLEXITY_API_KEY")?;
            Ok(Box::new(OpenAICompatible::new(OpenAICompatibleConfig::perplexity(
                api_key, model_name,
            ))?))
        },
    }
}

#[cfg(feature = "openai")]
fn cerebras_cheapest_spec() -> ProviderSpec {
    ProviderSpec {
        name: "cerebras-cheapest",
        model_env_candidates: &["CEREBRAS_CHEAPEST_MODEL", "CEREBRAS_MODEL"],
        default_model: adk_model::catalog::CEREBRAS_DEFAULT,
        required_envs: &["CEREBRAS_API_KEY"],
        supports_tools: true,
        build_model: |model_name| {
            let api_key = required_env("CEREBRAS_API_KEY")?;
            Ok(Box::new(OpenAICompatible::new(OpenAICompatibleConfig::cerebras(
                api_key, model_name,
            ))?))
        },
    }
}

#[cfg(feature = "openai")]
fn sambanova_cheapest_spec() -> ProviderSpec {
    ProviderSpec {
        name: "sambanova-cheapest",
        model_env_candidates: &["SAMBANOVA_CHEAPEST_MODEL", "SAMBANOVA_MODEL"],
        default_model: adk_model::catalog::SAMBANOVA_DEFAULT,
        required_envs: &["SAMBANOVA_API_KEY"],
        supports_tools: true,
        build_model: |model_name| {
            let api_key = required_env("SAMBANOVA_API_KEY")?;
            Ok(Box::new(OpenAICompatible::new(OpenAICompatibleConfig::sambanova(
                api_key, model_name,
            ))?))
        },
    }
}

#[cfg(feature = "openai")]
provider_contract_tests!(fireworks_cheapest_provider, fireworks_cheapest_spec);
#[cfg(feature = "openai")]
provider_contract_tests!(together_cheapest_provider, together_cheapest_spec);
#[cfg(feature = "openai")]
provider_contract_tests!(mistral_cheapest_provider, mistral_cheapest_spec);
#[cfg(feature = "openai")]
provider_contract_tests!(perplexity_cheapest_provider, perplexity_cheapest_spec);
#[cfg(feature = "openai")]
provider_contract_tests!(cerebras_cheapest_provider, cerebras_cheapest_spec);
#[cfg(feature = "openai")]
provider_contract_tests!(sambanova_cheapest_provider, sambanova_cheapest_spec);

#[cfg(feature = "bedrock")]
fn bedrock_cheapest_spec() -> ProviderSpec {
    ProviderSpec {
        name: "bedrock-cheapest",
        model_env_candidates: &["BEDROCK_CHEAPEST_MODEL", "BEDROCK_MODEL"],
        default_model: "us.anthropic.claude-haiku-4-5-20251001-v1:0",
        required_envs: &["AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY"],
        supports_tools: true,
        build_model: |model_name| {
            let region = env::var("AWS_DEFAULT_REGION").unwrap_or_else(|_| "us-east-1".to_string());
            let config = BedrockConfig::new(region, model_name);
            let client = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(BedrockClient::new(config))
            })?;
            Ok(Box::new(client))
        },
    }
}

#[cfg(feature = "azure-ai")]
fn azure_ai_cheapest_spec() -> ProviderSpec {
    ProviderSpec {
        name: "azure-ai-cheapest",
        model_env_candidates: &["AZURE_AI_CHEAPEST_MODEL", "AZURE_AI_MODEL"],
        default_model: "meta-llama-3.1-8b-instruct",
        required_envs: &["AZURE_AI_ENDPOINT", "AZURE_AI_API_KEY"],
        supports_tools: true,
        build_model: |model_name| {
            let endpoint = required_env("AZURE_AI_ENDPOINT")?;
            let api_key = required_env("AZURE_AI_API_KEY")?;
            Ok(Box::new(AzureAIClient::new(AzureAIConfig::new(endpoint, api_key, model_name))?))
        },
    }
}

#[cfg(feature = "bedrock")]
provider_contract_tests!(bedrock_cheapest_provider, bedrock_cheapest_spec);
#[cfg(feature = "azure-ai")]
provider_contract_tests!(azure_ai_cheapest_provider, azure_ai_cheapest_spec);

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
