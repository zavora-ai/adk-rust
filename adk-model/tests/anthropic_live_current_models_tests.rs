//! Live tool-call turns on the current Claude models, including Claude Sonnet 5.5 with thinking
//! turned off through `between_tools`. Require `ANTHROPIC_API_KEY`.
#![cfg(feature = "anthropic")]

use adk_core::{Content, GenerateContentConfig, Llm, LlmRequest, LlmResponse, Part};
use adk_model::anthropic::{AnthropicClient, AnthropicConfig, Effort, ThinkingMode};
use futures::TryStreamExt;
use serde_json::json;

fn weather_request() -> LlmRequest {
    let mut request = LlmRequest::new(
        "ignored",
        vec![Content::new("user").with_text("Use get_weather for Paris, then answer in one line.")],
    );
    request.tools.insert(
        "get_weather".to_string(),
        json!({
            "description": "Get the current weather for a city",
            "parameters": {
                "type": "object",
                "properties": {"city": {"type": "string"}},
                "required": ["city"]
            }
        }),
    );
    request.config =
        Some(GenerateContentConfig { max_output_tokens: Some(2_048), ..Default::default() });
    request
}

async fn turn(client: &AnthropicClient, request: LlmRequest, stream: bool) -> Content {
    let responses: Vec<LlmResponse> = client
        .generate_content(request, stream)
        .await
        .expect("request accepted")
        .try_collect()
        .await
        .expect("response parsed");
    let last = responses.last().expect("a response");
    assert_eq!(last.error_code, None, "turn failed: {last:?}");
    responses.into_iter().rev().find_map(|response| response.content).expect("turn content")
}

/// Calls the tool, answers it, and returns the final text.
async fn tool_round_trip(config: AnthropicConfig, stream: bool) -> String {
    let client = AnthropicClient::new(config).unwrap();
    let mut request = weather_request();
    let call = turn(&client, request.clone(), stream).await;
    let call_id = call
        .parts
        .iter()
        .find_map(|part| match part {
            Part::FunctionCall { name, id, .. } if name == "get_weather" => id.clone(),
            _ => None,
        })
        .expect("the model calls get_weather");
    request.contents.push(call);
    request.contents.push(Content {
        role: "function".into(),
        parts: vec![Part::FunctionResponse {
            function_response: adk_core::FunctionResponseData::new(
                "get_weather",
                json!({"city": "Paris", "temperature_c": 9, "conditions": "rain"}),
            ),
            id: Some(call_id),
            annotations: None,
        }],
    });
    let answer = turn(&client, request, stream).await;
    answer.parts.iter().filter_map(Part::text).collect()
}

#[tokio::test]
#[ignore = "calls the Anthropic API; requires ANTHROPIC_API_KEY"]
async fn live_current_models_complete_a_tool_call() {
    let Ok(key) = std::env::var("ANTHROPIC_API_KEY") else { return };
    let cases = [
        (AnthropicConfig::new(key.clone(), "claude-haiku-5-5").with_effort(Effort::Low), false),
        (AnthropicConfig::new(key.clone(), "claude-sonnet-5-5").with_effort(Effort::Low), true),
        (
            AnthropicConfig::new(key.clone(), "claude-sonnet-5-5")
                .with_thinking_mode(ThinkingMode::BetweenTools)
                .with_effort(Effort::Low),
            false,
        ),
        (AnthropicConfig::new(key, "claude-fable-5-2").with_effort(Effort::Low), false),
    ];
    for (config, stream) in cases {
        let label = format!("{} thinking={:?} stream={stream}", config.model, config.thinking);
        let answer = tool_round_trip(config, stream).await;
        println!("{label}: {answer}");
        assert!(!answer.trim().is_empty(), "{label}: empty answer");
    }
}

/// Claude Haiku 5.5 consults Claude Sonnet 5.5 through the advisor tool; the response's cost
/// includes the advisor call, and the spend ledger commits it.
#[tokio::test]
#[ignore = "calls the Anthropic API; needs ANTHROPIC_API_KEY"]
async fn haiku_consults_a_sonnet_advisor_and_the_ledger_counts_it() {
    use adk_core::{InMemorySpendLedger, ModelCallScope, SpendKey, SpendLedger, SpendPeriod};
    use std::sync::Arc;

    let key = std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY");
    let client =
        AnthropicClient::new(AnthropicConfig::new(key, "claude-haiku-5-5").with_max_tokens(2_048))
            .unwrap();
    let mut request = LlmRequest::new(
        "claude-haiku-5-5",
        vec![
            Content::new("system").with_text(
                "Before you answer, call the advisor tool once for guidance, then answer in at \
                 most three sentences.",
            ),
            Content::new("user").with_text(
                "Our nightly job deletes rows older than 30 days with one DELETE statement and \
                 now times out on a 400M-row PostgreSQL table. How should we fix it safely?",
            ),
        ],
    );
    request.tools.insert(
        "advisor".to_string(),
        json!({
            "name": "advisor",
            "description": "Consults a stronger Claude model for guidance mid-task (server-side).",
            "x-adk-anthropic-tool": {
                "type": "advisor_20260301",
                "name": "advisor",
                "model": "claude-sonnet-5-5",
                "max_uses": 1,
                "max_tokens": 2_048
            }
        }),
    );
    let bound = client.max_cost_micro_usd(&request).expect("a bounded advisor");

    let ledger = Arc::new(InMemorySpendLedger::default());
    let scope = ModelCallScope::new().with_spend_ledger(ledger.clone(), "acme");
    let responses: Vec<LlmResponse> = scope
        .generate(&client, request, false)
        .await
        .expect("request accepted")
        .try_collect()
        .await
        .expect("response parsed");
    let response = responses.last().expect("a response");
    let usage = response.usage_metadata.as_ref().expect("usage");
    let iterations = usage.provider_usage.as_ref().and_then(|raw| raw.get("iterations"));
    let advisor_calls = iterations
        .and_then(|iterations| iterations.as_array())
        .map(|iterations| iterations.iter().filter(|it| it["type"] == "advisor_message").count())
        .unwrap_or(0);
    let executor_only = adk_model::PricingCatalog::standard()
        .cost_usd(Some("anthropic"), "claude-haiku-5-5", usage)
        .unwrap();
    let spent = ledger
        .spent(&SpendKey::org("acme").with_vendor("anthropic"), SpendPeriod::Day)
        .await
        .unwrap();
    println!("iterations: {}", iterations.map(ToString::to_string).unwrap_or_default());
    println!(
        "cost: {:?} USD (executor alone {executor_only} USD); bound {bound} micro-USD; ledger {spent} micro-USD",
        usage.cost
    );
    let text: String = response
        .content
        .iter()
        .flat_map(|content| content.parts.iter())
        .filter_map(Part::text)
        .collect();
    println!("answer: {text}");
    assert_eq!(response.error_code, None);
    assert_eq!(advisor_calls, 1, "the executor consulted the advisor once");
    assert!(usage.cost.unwrap() > executor_only);
    assert_eq!(Some(spent), adk_core::usd_to_micro_usd(usage.cost.unwrap()));
    assert!(spent <= bound);
}
