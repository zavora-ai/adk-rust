//! ML14: the OpenAI Responses API must not receive a model's reasoning back as assistant text;
//! reasoning is replayed only as reasoning items.

use std::sync::Arc;

use adk_model::openai::{
    OpenAIResponsesClient, OpenAIResponsesConfig, ReasoningEffort, ReasoningSummary,
};
use serde_json::Value;

use crate::common::{
    AgentSpec, Harness, Keys, Mode, Outcome, Verdict, fail, pass, short, weather_tool,
};
use crate::upstream::Proxy;

const FINDING: &str = "ML14";

/// Assistant messages in a Responses `input` that are not one of the model's visible answers:
/// reasoning replayed as if the model had said it.
fn replayed_reasoning(body: &Value, answers: &[String]) -> usize {
    body["input"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter(|item| item["role"] == "assistant")
                .filter(|item| {
                    let text = match &item["content"] {
                        Value::String(text) => text.clone(),
                        Value::Array(parts) => parts
                            .iter()
                            .filter_map(|part| part["text"].as_str())
                            .collect::<String>(),
                        _ => String::new(),
                    };
                    !answers.iter().any(|answer| answer.trim() == text.trim())
                })
                .count()
        })
        .unwrap_or(0)
}

/// The forms a turn's visible answer can take in history: its final event's text, its streamed
/// text deltas, and both together.
fn visible_answers(turn: &crate::common::Turn) -> Vec<String> {
    let text = |partial: bool| -> String {
        turn.events
            .iter()
            .filter(|event| event.llm_response.partial == partial)
            .filter_map(|event| event.llm_response.content.as_ref())
            .flat_map(|content| content.parts.iter())
            .filter_map(|part| match part {
                adk_core::Part::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    };
    vec![turn.answer(), text(true), format!("{}{}", text(true), text(false))]
}

/// The reasoning text a turn streamed, from its thinking deltas and final events.
fn streamed_reasoning(turn: &crate::common::Turn) -> String {
    turn.events
        .iter()
        .filter_map(|event| event.llm_response.content.as_ref())
        .flat_map(|content| content.parts.iter())
        .filter_map(|part| match part {
            adk_core::Part::Thinking { thinking, .. } => Some(thinking.as_str()),
            _ => None,
        })
        .collect()
}

/// One two-turn conversation; `None` when the model returned no reasoning summary to check.
async fn reasoning_replay(key: &str) -> anyhow::Result<Option<Verdict>> {
    let proxy = Proxy::start("https://api.openai.com").await?;
    let config = OpenAIResponsesConfig::new(key, "gpt-5.6-terra")
        .with_base_url(format!("{}/v1", proxy.base))
        .with_reasoning_effort(ReasoningEffort::High)
        .with_reasoning_summary(ReasoningSummary::Detailed);
    let spec =
        AgentSpec::new("You compare the weather with the get_weather tool.").tool(weather_tool());
    let harness =
        Harness::new("ml14", Arc::new(OpenAIResponsesClient::new(config)?), &spec).await?;

    let first = harness
        .turn(
            "Think it through step by step: which is warmer right now, Paris or Tokyo, and by \
             how much? Check both with the tool.",
        )
        .await;
    if let Some(error) = &first.error {
        return Ok(Some(fail(format!("turn 1 failed: {}", short(error, 200)))));
    }
    let second = harness.turn("Given that, which city suits a picnic today? One sentence.").await;
    let reasoning = format!("{}{}", streamed_reasoning(&first), streamed_reasoning(&second));
    if reasoning.trim().is_empty() {
        return Ok(None);
    }
    let requests: Vec<Value> = proxy
        .captured()
        .into_iter()
        .filter(|request| request.path.ends_with("/responses"))
        .map(|request| request.body)
        .collect();
    let answers: Vec<String> =
        visible_answers(&first).into_iter().chain(visible_answers(&second)).collect();
    let replayed: usize = requests.iter().map(|body| replayed_reasoning(body, &answers)).sum();
    let reasoning_items: usize = requests
        .iter()
        .filter_map(|body| body["input"].as_array())
        .flatten()
        .filter(|item| item["type"] == "reasoning")
        .count();
    if replayed > 0 {
        return Ok(Some(fail(format!(
            "{replayed} assistant messages that are not answers (replayed reasoning) across {} requests",
            requests.len()
        ))));
    }
    if let Some(error) = second.error {
        return Ok(Some(fail(format!("turn 2 failed: {}", short(&error, 200)))));
    }
    Ok(Some(pass(format!(
        "{} requests, {reasoning_items} reasoning items replayed, no reasoning as assistant text",
        requests.len()
    ))))
}

/// Summaries are optional for the model, so a conversation without one is tried again.
async fn replay_with_summary(key: &str) -> anyhow::Result<Verdict> {
    for _ in 0..3 {
        if let Some(verdict) = reasoning_replay(key).await? {
            return Ok(verdict);
        }
    }
    Ok(Verdict::Skip("the model returned no reasoning summary in three conversations".into()))
}

pub async fn run(keys: &Keys, outcomes: &mut Vec<Outcome>) {
    let verdict = match &keys.openai {
        None => Verdict::Skip("OPENAI_API_KEY not set".into()),
        Some(key) => {
            replay_with_summary(key).await.unwrap_or_else(|error| fail(format!("setup: {error}")))
        }
    };
    outcomes.push(Outcome {
        id: "ml14-responses-reasoning",
        finding: FINDING,
        mode: Mode::Live,
        verdict,
    });
}
