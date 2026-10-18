//! ML5: a content holding several function responses must become one tool message per
//! response on every Chat Completions client.
//!
//! A run budget that refuses a batch of two parallel calls answers both calls in one
//! `function` content (the standard `LlmAgent` path). The next turn replays that content.

use std::sync::Arc;

use adk_core::{Llm, RunBudget, RunConfig};
use adk_model::azure_ai::{AzureAIClient, AzureAIConfig};
use adk_model::deepseek::{DeepSeekClient, DeepSeekConfig};
use adk_model::groq::{GroqClient, GroqConfig};
use adk_model::openai::{AzureConfig, AzureOpenAIClient, OpenAIReasoningEffort};
use adk_model::{OpenAIClient, OpenAICompatible, OpenAICompatibleConfig, OpenAIConfig};
use serde_json::{Value, json};

use crate::common::{
    AgentSpec, Harness, Keys, Mode, Outcome, Verdict, fail, pass, short, time_tool, weather_tool,
};
use crate::upstream::{Reply, Upstream, chat};

const FINDING: &str = "ML5";
const INSTRUCTION: &str = "You answer weather and time questions with the tools. When a \
    question names several cities, call the tool for every city in the same turn, in parallel.";

fn spec() -> AgentSpec {
    AgentSpec::new(INSTRUCTION).tool(weather_tool()).tool(time_tool())
}

fn one_tool_call_budget() -> RunConfig {
    RunConfig::builder().budget(RunBudget::new().max_tool_calls(1)).build()
}

/// Two parallel calls refused by the budget, then two more turns on the same session.
async fn parallel_calls_over_turns(model: Arc<dyn Llm>) -> anyhow::Result<Verdict> {
    for attempt in 1..=2 {
        let harness = Harness::new("ml5", model.clone(), &spec()).await?;
        let first = harness
            .turn_with(
                "What is the weather in Paris and in Tokyo? Call get_weather for both cities in \
                 one turn.",
                one_tool_call_budget(),
            )
            .await;
        if first.max_responses_in_one_event() < 2 {
            if let Some(error) = &first.error
                && is_unpaid(error)
            {
                return Ok(Verdict::Skip(format!(
                    "account cannot pay for calls: {}",
                    short(error, 120)
                )));
            }
            if attempt == 2 {
                return Ok(Verdict::Skip(format!(
                    "the model did not make two parallel calls (error: {})",
                    first.error.as_deref().unwrap_or("none")
                )));
            }
            continue;
        }
        let second = harness.turn("Thanks. Now, what is the weather in Paris and in Tokyo?").await;
        if let Some(error) = second.error {
            return Ok(fail(format!(
                "turn 2 after the two-response content failed: {}",
                short(&error, 220)
            )));
        }
        let third = harness.turn("And what is the local time in both cities?").await;
        if let Some(error) = third.error {
            return Ok(fail(format!("turn 3 failed: {}", short(&error, 220))));
        }
        return Ok(pass(format!(
            "turns 2 and 3 answered: {:?} / {:?}",
            short(&second.answer(), 60),
            short(&third.answer(), 60)
        )));
    }
    unreachable!("the loop returns on its last attempt")
}

fn is_unpaid(error: &str) -> bool {
    error.contains("402")
        || error.contains("Insufficient Balance")
        || error.contains("insufficient")
}

/// The first problem OpenAI would reject in a Chat Completions request: a tool call without a
/// tool message answering it before the next non-tool message.
pub fn chat_history_problem(body: &Value) -> Option<String> {
    let messages = body["messages"].as_array()?;
    for (index, message) in messages.iter().enumerate() {
        let Some(calls) = message["tool_calls"].as_array() else { continue };
        let answered: Vec<&str> = messages[index + 1..]
            .iter()
            .take_while(|next| next["role"] == "tool")
            .filter_map(|next| next["tool_call_id"].as_str())
            .collect();
        for call in calls {
            let id = call["id"].as_str().unwrap_or_default();
            if !answered.contains(&id) {
                return Some(format!(
                    "tool call {id} has no tool message (answered: {answered:?})"
                ));
            }
        }
    }
    None
}

async fn scripted(
    id: &'static str,
    build: impl FnOnce(&str) -> anyhow::Result<Arc<dyn Llm>>,
) -> Outcome {
    let verdict = async {
        let upstream = Upstream::start().await?;
        upstream.push(Reply::Sse(chat::turn(
            None,
            &[
                ("call_paris", "get_weather", json!({"city": "Paris"})),
                ("call_tokyo", "get_weather", json!({"city": "Tokyo"})),
            ],
        )));
        upstream.push(Reply::Sse(chat::turn(Some("Paris is 17C and Tokyo is 17C."), &[])));
        let harness = Harness::new("ml5", build(&upstream.base)?, &spec()).await?;
        let first = harness.turn_with("Weather in Paris and Tokyo?", one_tool_call_budget()).await;
        if first.max_responses_in_one_event() < 2 {
            return Ok(fail("the budget did not answer both calls in one content"));
        }
        let second = harness.turn("Again, please.").await;
        let captured = upstream.captured();
        let Some(request) = captured.get(1) else {
            return Ok(fail(format!("turn 2 sent no request (error: {:?})", second.error)));
        };
        Ok::<_, anyhow::Error>(match chat_history_problem(&request.body) {
            Some(problem) => fail(problem),
            None if second.error.is_some() => fail(format!("turn 2 failed: {:?}", second.error)),
            None => pass("one tool message per function response"),
        })
    }
    .await
    .unwrap_or_else(|error| fail(format!("setup: {error}")));
    Outcome { id, finding: FINDING, mode: Mode::Scripted, verdict }
}

pub async fn run(keys: &Keys, outcomes: &mut Vec<Outcome>) {
    // Live: gpt-5.6-terra on Chat Completions (reasoning `none` keeps tools on Chat).
    let verdict = match &keys.openai {
        None => Verdict::Skip("OPENAI_API_KEY not set".into()),
        Some(key) => async {
            let client = OpenAIClient::new_with_reasoning_effort(
                OpenAIConfig::new(key, "gpt-5.6-terra"),
                OpenAIReasoningEffort::None,
            )?;
            parallel_calls_over_turns(Arc::new(client)).await
        }
        .await
        .unwrap_or_else(|error| fail(format!("setup: {error}"))),
    };
    outcomes.push(Outcome { id: "ml5-openai-chat", finding: FINDING, mode: Mode::Live, verdict });

    let verdict = match &keys.groq {
        None => Verdict::Skip("GROQ_API_KEY not set".into()),
        Some(key) => async {
            let client =
                GroqClient::new(GroqConfig::new(key, "qwen/qwen3.8-27b").with_max_tokens(600))?;
            parallel_calls_over_turns(Arc::new(client)).await
        }
        .await
        .unwrap_or_else(|error| fail(format!("setup: {error}"))),
    };
    outcomes.push(Outcome { id: "ml5-groq", finding: FINDING, mode: Mode::Live, verdict });

    let verdict = match &keys.deepseek {
        None => Verdict::Skip("DEEPSEEK_API_KEY not set".into()),
        Some(key) => async {
            let client = DeepSeekClient::new(DeepSeekConfig::v4_flash(key))?;
            parallel_calls_over_turns(Arc::new(client)).await
        }
        .await
        .unwrap_or_else(|error| fail(format!("setup: {error}"))),
    };
    outcomes.push(Outcome { id: "ml5-deepseek", finding: FINDING, mode: Mode::Live, verdict });

    outcomes.push(
        scripted("ml5-compatible", |base| {
            let config = OpenAICompatibleConfig::new("test-key", "scripted-model")
                .with_base_url(format!("{base}/v1"));
            Ok(Arc::new(OpenAICompatible::new(config)?))
        })
        .await,
    );
    outcomes.push(
        scripted("ml5-azure-openai", |base| {
            let config = AzureConfig::new("test-key", base, "2024-12-01-preview", "gpt-scripted");
            Ok(Arc::new(AzureOpenAIClient::new(config)?))
        })
        .await,
    );
    outcomes.push(
        scripted("ml5-groq-scripted", |base| {
            Ok(Arc::new(GroqClient::new(
                GroqConfig::new("test-key", "scripted").with_base_url(base),
            )?))
        })
        .await,
    );
    outcomes.push(
        scripted("ml5-deepseek-scripted", |base| {
            let config = DeepSeekConfig::new("test-key", "deepseek-chat").with_base_url(base);
            Ok(Arc::new(DeepSeekClient::new(config)?))
        })
        .await,
    );
    outcomes.push(
        scripted("ml5-azure-ai", |base| {
            Ok(Arc::new(AzureAIClient::new(AzureAIConfig::new(base, "test-key", "scripted"))?))
        })
        .await,
    );
}
