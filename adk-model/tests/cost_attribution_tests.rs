//! Every provider response carries its provider, model and — for priced models —
//! a cost computed from the provider's own usage report.
//!
//! The expected costs are computed by hand from the published rates, with each
//! prompt token billed once: cached tokens are part of the reported prompt.

use adk_core::{Content, Llm, LlmRequest, LlmResponse};
use futures::StreamExt;
use serde_json::json;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn mock_json(body: serde_json::Value) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;
    server
}

async fn final_response(model: &dyn Llm, model_id: &str) -> LlmResponse {
    let request = LlmRequest::new(model_id, vec![Content::new("user").with_text("Hello")]);
    let responses: Vec<LlmResponse> = model
        .generate_content(request, false)
        .await
        .unwrap()
        .map(|item| item.unwrap())
        .collect()
        .await;
    responses.into_iter().rev().find(|response| response.usage_metadata.is_some()).unwrap()
}

fn assert_cost(response: &LlmResponse, expected: f64) {
    let cost = response.usage_metadata.as_ref().and_then(|usage| usage.cost).expect("cost");
    assert!((cost - expected).abs() < 1e-9, "cost {cost} != {expected}");
}

#[cfg(feature = "gemini")]
#[tokio::test]
async fn gemini_cost_bills_cached_prompt_tokens_once() {
    let server = mock_json(json!({
        "candidates": [{
            "content": {"role": "model", "parts": [{"text": "ok"}]},
            "finishReason": "STOP"
        }],
        "usageMetadata": {
            "promptTokenCount": 1_000_000,
            "cachedContentTokenCount": 400_000,
            "candidatesTokenCount": 150_000,
            "thoughtsTokenCount": 50_000,
            "totalTokenCount": 1_200_000
        },
        "modelVersion": "gemini-2.5-flash-001"
    }))
    .await;
    let model = adk_model::GeminiModel::new_with_base_url(
        "test-key",
        "gemini-2.5-flash",
        format!("{}/v1beta/", server.uri()),
    )
    .unwrap();

    let response = final_response(&model, "gemini-2.5-flash").await;

    assert_eq!(response.provider.as_deref(), Some("gemini"));
    assert_eq!(response.model.as_deref(), Some("gemini-2.5-flash-001"));
    // 600K uncached × $0.30 + 400K cached × $0.03 + (150K + 50K thinking) × $2.50.
    assert_cost(&response, 0.18 + 0.012 + 0.50);
}

#[cfg(feature = "gemini")]
#[tokio::test]
async fn gemini_streaming_chunks_are_attributed_and_priced() {
    let chunk = |text: &str, finish: Option<&str>, output: i32| {
        let mut candidate = json!({"content": {"role": "model", "parts": [{"text": text}]}});
        if let Some(finish) = finish {
            candidate["finishReason"] = json!(finish);
        }
        json!({
            "candidates": [candidate],
            "usageMetadata": {
                "promptTokenCount": 1_000,
                "candidatesTokenCount": output,
                "totalTokenCount": 1_000 + output
            }
        })
    };
    let body =
        format!("data: {}\n\ndata: {}\n\n", chunk("Hel", None, 10), chunk("lo", Some("STOP"), 20));
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .mount(&server)
        .await;
    let model = adk_model::GeminiModel::new_with_base_url(
        "test-key",
        "gemini-3.5-flash-lite",
        format!("{}/v1beta/", server.uri()),
    )
    .unwrap();

    let request =
        LlmRequest::new("gemini-3.5-flash-lite", vec![Content::new("user").with_text("Hi")]);
    let responses: Vec<LlmResponse> = model
        .generate_content(request, true)
        .await
        .unwrap()
        .map(|item| item.unwrap())
        .collect()
        .await;

    let priced: Vec<&LlmResponse> =
        responses.iter().filter(|response| response.usage_metadata.is_some()).collect();
    assert_eq!(priced.len(), 2);
    assert!(priced[0].partial && !priced[1].partial);
    // Every chunk reports cumulative usage, so the final chunk alone carries the call's cost.
    assert_cost(priced[1], (1_000.0 * 0.30 + 20.0 * 2.50) / 1e6);
    assert!(responses.iter().all(|response| response.provider.as_deref() == Some("gemini")));
}

#[cfg(feature = "openai")]
#[tokio::test]
async fn openai_cost_uses_the_reported_dated_model() {
    let server = mock_json(json!({
        "id": "chatcmpl-1",
        "object": "chat.completion",
        "created": 1,
        "model": "gpt-4.1-2025-04-14",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "ok"},
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 1_000_000,
            "completion_tokens": 100_000,
            "total_tokens": 1_100_000,
            "prompt_tokens_details": {"cached_tokens": 400_000}
        }
    }))
    .await;
    let model = adk_model::openai::OpenAIClient::new(adk_model::openai::OpenAIConfig {
        api_key: "test-key".to_string(),
        model: "gpt-4.1".to_string(),
        base_url: Some(server.uri()),
        organization_id: None,
        project_id: None,
        reasoning_effort: None,
    })
    .unwrap()
    .with_retry_config(adk_model::RetryConfig::disabled());

    let response = final_response(&model, "gpt-4.1").await;

    assert_eq!(response.provider.as_deref(), Some("openai"));
    assert_eq!(response.model.as_deref(), Some("gpt-4.1-2025-04-14"));
    // 600K uncached × $2.00 + 400K cached × $0.50 + 100K × $8.00.
    assert_cost(&response, 1.20 + 0.20 + 0.80);
}

#[cfg(feature = "openai")]
#[tokio::test]
async fn unpriced_models_keep_an_unknown_cost() {
    let server = mock_json(json!({
        "id": "chatcmpl-1",
        "object": "chat.completion",
        "created": 1,
        "model": "gpt-4.1",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "ok"},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
    }))
    .await;
    // A gateway that serves an OpenAI model name does not bill at OpenAI's list price.
    let model = adk_model::OpenAICompatible::new(
        adk_model::OpenAICompatibleConfig::together("test-key", "gpt-4.1")
            .with_base_url(server.uri()),
    )
    .unwrap()
    .with_retry_config(adk_model::RetryConfig::disabled());

    let response = final_response(&model, "gpt-4.1").await;

    assert_eq!(response.provider.as_deref(), Some("together"));
    let usage = response.usage_metadata.unwrap();
    assert_eq!(usage.total_token_count, 15);
    assert_eq!(usage.cost, None);
}

#[cfg(feature = "anthropic")]
#[tokio::test]
async fn anthropic_cost_splits_cache_writes_by_ttl() {
    let server = mock_json(json!({
        "id": "msg_1",
        "type": "message",
        "role": "assistant",
        "content": [{"type": "text", "text": "ok"}],
        "model": "claude-sonnet-4-6",
        "stop_reason": "end_turn",
        "stop_sequence": null,
        "usage": {
            "input_tokens": 1_000,
            "output_tokens": 2_000,
            "cache_read_input_tokens": 200_000,
            "cache_creation_input_tokens": 100_000,
            "cache_creation": {
                "ephemeral_5m_input_tokens": 60_000,
                "ephemeral_1h_input_tokens": 40_000
            }
        }
    }))
    .await;
    let model = adk_model::anthropic::AnthropicClient::new(
        adk_model::anthropic::AnthropicConfig::new("test-key", "claude-sonnet-4-6")
            .with_base_url(server.uri()),
    )
    .unwrap()
    .with_retry_config(adk_model::RetryConfig::disabled());

    let response = final_response(&model, "claude-sonnet-4-6").await;

    assert_eq!(response.provider.as_deref(), Some("anthropic"));
    assert_eq!(response.model.as_deref(), Some("claude-sonnet-4-6"));
    let usage = response.usage_metadata.as_ref().unwrap();
    // The normalized prompt includes cache reads and writes; none is billed twice.
    assert_eq!(usage.prompt_token_count, 301_000);
    // 1K × $3.00 + 200K reads × $0.30 + 60K 5m writes × $3.75 + 40K 1h writes × $6.00
    // + 2K × $15.00.
    assert_cost(&response, 0.003 + 0.06 + 0.225 + 0.24 + 0.03);
}
