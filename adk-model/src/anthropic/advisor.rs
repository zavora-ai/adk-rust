//! Pricing for Claude's advisor tool (`advisor_20260301`): the advisor's sub-inferences are
//! billed at the advisor model's rates and reported apart from the executor's usage.

use adk_core::{LlmRequest, LlmResponse, UsageMetadata};
use serde_json::Value;

use crate::pricing::{PricingCatalog, REQUEST_OVERHEAD_TOKENS, request_input_token_bound};

/// The tool type of Claude's advisor tool in an `x-adk-anthropic-tool` declaration.
const ADVISOR_TOOL_TYPE: &str = "advisor_20260301";

/// Adds the cost of the advisor calls `response` reports to its usage cost.
///
/// The top-level usage covers the executor only; each `advisor_message` entry of
/// `usage.iterations` is one advisor call, billed at its model's rates. When an advisor model
/// is unpriced, the cost becomes unknown, so a cost cap fails closed instead of undercounting.
pub(super) fn add_advisor_cost(response: &mut LlmResponse) {
    let Some(usage) = response.usage_metadata.as_mut() else {
        return;
    };
    let Some(iterations) = usage
        .provider_usage
        .as_ref()
        .and_then(|raw| raw.get("iterations"))
        .and_then(Value::as_array)
    else {
        return;
    };
    let catalog = PricingCatalog::standard();
    let mut advisor_cost = Some(0.0);
    let mut calls = 0usize;
    for iteration in iterations.iter().filter(|iteration| iteration["type"] == "advisor_message") {
        calls += 1;
        let tokens = |field: &str| {
            iteration.get(field).and_then(Value::as_i64).and_then(|n| i32::try_from(n).ok())
        };
        let input = tokens("input_tokens").unwrap_or(0);
        let cache_read = tokens("cache_read_input_tokens");
        let cache_creation = tokens("cache_creation_input_tokens");
        let output = tokens("output_tokens").unwrap_or(0);
        let prompt = input
            .saturating_add(cache_read.unwrap_or(0))
            .saturating_add(cache_creation.unwrap_or(0));
        let advisor_usage = UsageMetadata {
            prompt_token_count: prompt,
            candidates_token_count: output,
            total_token_count: prompt.saturating_add(output),
            cache_read_input_token_count: cache_read,
            cache_creation_input_token_count: cache_creation,
            ..Default::default()
        };
        let model = iteration.get("model").and_then(Value::as_str).unwrap_or_default();
        let cost = catalog.cost_usd(Some("anthropic"), model, &advisor_usage);
        if cost.is_none() {
            tracing::warn!(
                anthropic.advisor_model = model,
                "advisor model has no price; the response's cost is unknown"
            );
        }
        advisor_cost = advisor_cost.zip(cost).map(|(total, cost)| total + cost);
    }
    if calls == 0 {
        return;
    }
    usage.cost = usage.cost.zip(advisor_cost).map(|(executor, advisor)| executor + advisor);
}

/// Upper bound in micro-USD on what the advisor calls of one response to `request` can cost,
/// or `None` when they cannot be bounded.
///
/// `Some(0)` when the request declares no advisor tool. Each declared advisor is bounded by
/// its `max_uses` calls, each reading at most the whole request plus `executor_output` tokens
/// and writing at most its `max_tokens` (or its model's output cap); an advisor without
/// `max_uses`, or with an unpriced model, cannot be bounded.
pub(super) fn advisor_cost_bound_micro_usd(
    request: &LlmRequest,
    executor_output: u64,
) -> Option<u64> {
    let advisors: Vec<&Value> = request
        .tools
        .values()
        .filter_map(|declaration| declaration.get("x-adk-anthropic-tool"))
        .filter(|tool| tool["type"] == ADVISOR_TOOL_TYPE)
        .collect();
    if advisors.is_empty() {
        return Some(0);
    }
    let catalog = PricingCatalog::standard();
    // The advisor reads the executor's transcript under a system prompt of its own.
    let prompt = request_input_token_bound(request)?
        .saturating_add(executor_output)
        .saturating_add(REQUEST_OVERHEAD_TOKENS);
    let today = chrono::Utc::now().date_naive();
    let tomorrow = today.succ_opt().unwrap_or(today);
    let mut usd = 0.0;
    for advisor in advisors {
        let price = catalog.lookup(Some("anthropic"), advisor.get("model")?.as_str()?)?;
        let uses = advisor.get("max_uses")?.as_u64()?;
        let output =
            advisor.get("max_tokens").and_then(Value::as_u64).or(price.max_output_tokens)?;
        let per_call = price
            .max_cost_usd(prompt, output, today)
            .max(price.max_cost_usd(prompt, output, tomorrow));
        usd += uses as f64 * per_call;
    }
    adk_core::usd_to_micro_usd(usd).map(|micro| micro.saturating_add(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::Content;
    use serde_json::json;

    fn response_with(cost: Option<f64>, iterations: Value) -> LlmResponse {
        let mut response = LlmResponse::new(Content::new("model").with_text("ok"));
        response.usage_metadata = Some(UsageMetadata {
            prompt_token_count: 1_760,
            candidates_token_count: 531,
            total_token_count: 2_291,
            cost,
            provider_usage: Some(json!({ "iterations": iterations })),
            ..Default::default()
        });
        response
    }

    #[test]
    fn advisor_calls_are_priced_at_the_advisor_model_rates() {
        // Claude Sonnet 5.5 at $2 / $10 per MTok: 823 input and 1,612 output tokens.
        let mut response = response_with(
            Some(0.001),
            json!([
                { "type": "message", "input_tokens": 412, "output_tokens": 89 },
                {
                    "type": "advisor_message",
                    "model": "claude-sonnet-5-5",
                    "input_tokens": 823,
                    "cache_read_input_tokens": 0,
                    "cache_creation_input_tokens": 0,
                    "output_tokens": 1_612
                },
                { "type": "message", "input_tokens": 1_348, "output_tokens": 442 }
            ]),
        );
        add_advisor_cost(&mut response);
        let cost = response.usage_metadata.unwrap().cost.unwrap();
        let advisor = (823.0 * 2.0 + 1_612.0 * 10.0) / 1_000_000.0;
        assert!((cost - (0.001 + advisor)).abs() < 1e-12, "{cost}");
    }

    #[test]
    fn an_unpriced_advisor_makes_the_cost_unknown() {
        let mut response = response_with(
            Some(0.001),
            json!([{ "type": "advisor_message", "model": "claude-from-the-future", "output_tokens": 10 }]),
        );
        add_advisor_cost(&mut response);
        assert_eq!(response.usage_metadata.unwrap().cost, None);
    }

    #[test]
    fn a_response_without_advisor_calls_keeps_its_cost() {
        let mut response =
            response_with(Some(0.001), json!([{ "type": "message", "output_tokens": 10 }]));
        add_advisor_cost(&mut response);
        assert_eq!(response.usage_metadata.unwrap().cost, Some(0.001));
    }

    fn request_with(advisor: Value) -> LlmRequest {
        let mut request =
            LlmRequest::new("claude-haiku-5-5", vec![Content::new("user").with_text("hello")]);
        request.tools.insert(
            "advisor".to_string(),
            json!({ "name": "advisor", "x-adk-anthropic-tool": advisor }),
        );
        request
    }

    #[test]
    fn the_bound_covers_every_allowed_advisor_call() {
        let request = request_with(json!({
            "type": "advisor_20260301",
            "name": "advisor",
            "model": "claude-sonnet-5-5",
            "max_uses": 2,
            "max_tokens": 2_000
        }));
        let prompt = request_input_token_bound(&request).unwrap() + 1_000 + REQUEST_OVERHEAD_TOKENS;
        let one_call = (prompt as f64 * 4.0 + 2_000.0 * 10.0) / 1_000_000.0;
        let bound = advisor_cost_bound_micro_usd(&request, 1_000).unwrap();
        assert!(bound >= adk_core::usd_to_micro_usd(2.0 * one_call).unwrap(), "{bound}");

        let no_advisor =
            LlmRequest::new("claude-haiku-5-5", vec![Content::new("user").with_text("hello")]);
        assert_eq!(advisor_cost_bound_micro_usd(&no_advisor, 1_000), Some(0));
    }

    #[test]
    fn an_advisor_without_max_uses_cannot_be_bounded() {
        let request = request_with(json!({
            "type": "advisor_20260301",
            "name": "advisor",
            "model": "claude-sonnet-5-5"
        }));
        assert_eq!(advisor_cost_bound_micro_usd(&request, 1_000), None);
    }
}
