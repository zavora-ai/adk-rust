//! Item 3: every client fails a stalled request or stream with `ErrorCategory::Timeout`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use adk_core::{AdkError, ErrorCategory, Llm};
use adk_model::azure_ai::{AzureAIClient, AzureAIConfig};
use adk_model::bedrock::{BedrockClient, BedrockConfig};
use adk_model::openai::{OpenAIResponsesClient, OpenAIResponsesConfig};
use adk_model::{
    DeepSeekClient, DeepSeekConfig, GroqClient, GroqConfig, HttpTimeouts, OllamaConfig,
    OllamaModel, OpenRouterApiMode, OpenRouterClient, OpenRouterConfig, RetryConfig,
};
use serde_json::json;

use crate::common::{Mode, Report, Verdict, agent_turn, check, describe, direct_call, fail};
use crate::upstream::{Reply, Upstream, named_sse, sse};

const IDLE: Duration = Duration::from_secs(1);

fn timeouts() -> HttpTimeouts {
    HttpTimeouts::new(Duration::from_secs(1), IDLE)
}

/// Where the upstream stops sending.
#[derive(Clone, Copy)]
enum Stall {
    /// Before the response headers, through `LlmAgent` (a streamed call).
    Headers,
    /// After the first stream event, through `LlmAgent`.
    Stream,
    /// After the headers of a non-streamed response, through `Llm::generate_content`.
    Body,
}

#[derive(Clone, Copy)]
enum Provider {
    Groq,
    DeepSeek,
    AzureAi,
    Ollama,
    OpenRouterChat,
    OpenRouterResponses,
    OpenAiResponses,
    Bedrock,
}

impl Provider {
    async fn build(self, base: &str) -> anyhow::Result<Arc<dyn Llm>> {
        let disabled = RetryConfig::disabled();
        Ok(match self {
            Self::Groq => Arc::new(
                GroqClient::new(
                    GroqConfig::new("test-key", "llama-3.3-70b-versatile").with_base_url(base),
                )?
                .with_timeouts(timeouts())?
                .with_retry_config(disabled),
            ),
            Self::DeepSeek => Arc::new(
                DeepSeekClient::new(
                    DeepSeekConfig::new("test-key", "deepseek-v4-flash").with_base_url(base),
                )?
                .with_timeouts(timeouts())?
                .with_retry_config(disabled),
            ),
            Self::AzureAi => Arc::new(
                AzureAIClient::new(AzureAIConfig::new(base, "test-key", "Phi-4"))?
                    .with_timeouts(timeouts())?
                    .with_retry_config(disabled),
            ),
            Self::Ollama => Arc::new(
                OllamaModel::new(OllamaConfig::with_host(base, "scripted"))?
                    .with_timeouts(timeouts())?,
            ),
            Self::OpenRouterChat | Self::OpenRouterResponses => {
                let mode = if matches!(self, Self::OpenRouterChat) {
                    OpenRouterApiMode::ChatCompletions
                } else {
                    OpenRouterApiMode::Responses
                };
                Arc::new(
                    OpenRouterClient::new(
                        OpenRouterConfig::new("test-key", "openai/gpt-5.6-terra")
                            .with_base_url(format!("{base}/api/v1"))
                            .with_default_api_mode(mode),
                    )?
                    .with_timeouts(timeouts())?
                    .with_retry_config(disabled),
                )
            }
            Self::OpenAiResponses => Arc::new(
                OpenAIResponsesClient::new(
                    OpenAIResponsesConfig::new("test-key", "gpt-5.6-terra")
                        .with_base_url(format!("{base}/v1")),
                )?
                .with_timeouts(timeouts())?
                .with_retry_config(disabled),
            ),
            Self::Bedrock => Arc::new(
                BedrockClient::new_with_api_key(
                    BedrockConfig::new("us-east-1", "us.anthropic.claude-sonnet-4-6")
                        .with_endpoint_url(base),
                    "test-key",
                )?
                .with_timeouts(timeouts())?
                .with_retry_config(disabled),
            ),
        })
    }

    /// The first event of a streamed answer, before the upstream stalls.
    fn first_event(self) -> String {
        match self {
            Self::OpenRouterResponses | Self::OpenAiResponses => named_sse(
                "response.created",
                &json!({"type": "response.created", "sequence_number": 0, "response": {
                    "id": "resp_1", "object": "response", "created_at": 1, "status": "in_progress",
                    "model": "gpt-5.6-terra", "output": []
                }}),
            ),
            Self::Groq
            | Self::DeepSeek
            | Self::AzureAi
            | Self::Ollama
            | Self::OpenRouterChat
            | Self::Bedrock => sse(&json!({
                "id": "chatcmpl-1", "object": "chat.completion.chunk", "created": 1, "model": "scripted",
                "choices": [{"index": 0, "delta": {"role": "assistant", "content": "Thinking"}, "finish_reason": null}]
            })),
        }
    }
}

pub async fn run(report: &mut Report) {
    use Provider::{
        AzureAi, Bedrock, DeepSeek, Groq, Ollama, OpenAiResponses, OpenRouterChat,
        OpenRouterResponses,
    };
    let cases: [(&'static str, Provider, Stall); 15] = [
        ("timeout-headers-groq", Groq, Stall::Headers),
        ("timeout-headers-deepseek", DeepSeek, Stall::Headers),
        ("timeout-headers-azure-ai", AzureAi, Stall::Headers),
        ("timeout-headers-openrouter-chat", OpenRouterChat, Stall::Headers),
        ("timeout-headers-openrouter-responses", OpenRouterResponses, Stall::Headers),
        ("timeout-headers-openai-responses", OpenAiResponses, Stall::Headers),
        ("timeout-headers-bedrock", Bedrock, Stall::Headers),
        ("timeout-stream-openrouter-chat", OpenRouterChat, Stall::Stream),
        ("timeout-stream-openrouter-responses", OpenRouterResponses, Stall::Stream),
        ("timeout-stream-openai-responses", OpenAiResponses, Stall::Stream),
        ("timeout-body-groq", Groq, Stall::Body),
        ("timeout-body-deepseek", DeepSeek, Stall::Body),
        ("timeout-body-azure-ai", AzureAi, Stall::Body),
        ("timeout-body-ollama", Ollama, Stall::Body),
        ("timeout-body-openai-responses", OpenAiResponses, Stall::Body),
    ];
    for (id, provider, stall) in cases {
        report.record_result(id, 3, Mode::Scripted, case(provider, stall).await);
    }
}

async fn case(provider: Provider, stall: Stall) -> anyhow::Result<Verdict> {
    let upstream = Upstream::start().await?;
    // The AWS SDK retries a timeout itself, so every attempt meets the same stall.
    for _ in 0..3 {
        upstream.push(match stall {
            Stall::Headers => Reply::Hang,
            Stall::Stream => {
                Reply::Stall { content_type: "text/event-stream", first: provider.first_event() }
            }
            Stall::Body => {
                Reply::Stall { content_type: "application/json", first: "{".to_string() }
            }
        });
    }
    let model = provider.build(&upstream.base).await?;
    let started = Instant::now();
    let call = async {
        match stall {
            Stall::Headers | Stall::Stream => {
                agent_turn(model.clone(), None, "Say hi.").await.map(|turn| turn.error)
            }
            Stall::Body => Ok(direct_call(model.as_ref(), false).await.1),
        }
    };
    // A client that ignores its idle bound would hold the run; cut it off well after it.
    let error: Option<AdkError> = match tokio::time::timeout(Duration::from_secs(20), call).await {
        Ok(result) => result?,
        Err(_) => return Ok(fail("no error within 20 s: the stall held the call")),
    };
    let elapsed = started.elapsed();
    Ok(match error {
        Some(error) => check(
            error.category == ErrorCategory::Timeout,
            format!("{} after {:.1}s", describe(&error), elapsed.as_secs_f32()),
        ),
        None => fail("the call succeeded against a stalled upstream"),
    })
}
