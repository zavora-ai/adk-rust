#![allow(clippy::result_large_err)]

#[path = "openrouter/common.rs"]
mod common;
#[path = "openrouter/llm_support.rs"]
mod llm_support;

use adk_core::{Content, GenerateContentConfig, Llm, LlmRequest};
use adk_model::openrouter::{
    OpenRouterApiMode, OpenRouterProviderPreferences, OpenRouterReasoningConfig,
    OpenRouterRequestOptions,
};
use anyhow::Result;

const MAX_OUTPUT_TOKENS: i32 = 256;

#[tokio::main]
async fn main() -> Result<()> {
    let (client, config) = common::build_client(OpenRouterApiMode::ChatCompletions)?;

    common::print_section("llm adapter chat mode");
    let mut chat_config =
        GenerateContentConfig { max_output_tokens: Some(MAX_OUTPUT_TOKENS), ..Default::default() };
    OpenRouterRequestOptions::default()
        .with_reasoning(OpenRouterReasoningConfig {
            effort: Some("medium".to_string()),
            ..Default::default()
        })
        .with_route("fallback")
        .with_provider_preferences(OpenRouterProviderPreferences {
            allow_fallbacks: Some(true),
            ..Default::default()
        })
        .insert_into_config(&mut chat_config)?;

    let chat_request = LlmRequest::new(
        config.model.clone(),
        vec![Content::new("user").with_text(
            "In one sentence, explain what OpenRouter routing buys an application developer.",
        )],
    )
    .with_config(chat_config);

    let chat_responses =
        llm_support::collect_llm_responses(client.generate_content(chat_request, false).await?)
            .await?;
    llm_support::print_llm_responses(&chat_responses);

    common::print_section("llm adapter responses mode");
    let mut responses_config =
        GenerateContentConfig { max_output_tokens: Some(MAX_OUTPUT_TOKENS), ..Default::default() };
    OpenRouterRequestOptions::default()
        .with_api_mode(OpenRouterApiMode::Responses)
        .with_reasoning(OpenRouterReasoningConfig {
            effort: Some("low".to_string()),
            ..Default::default()
        })
        .insert_into_config(&mut responses_config)?;

    let responses_request = LlmRequest::new(
        config.model,
        vec![Content::new("user").with_text("Count from 1 to 4, one line at a time.")],
    )
    .with_config(responses_config);

    let responses =
        llm_support::collect_llm_responses(client.generate_content(responses_request, true).await?)
            .await?;
    llm_support::print_llm_responses(&responses);

    Ok(())
}
