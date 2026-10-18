//! ML25: current Claude models omit thinking text unless the request asks for a summary, so
//! adaptive thinking needs a `display` option.

use std::sync::Arc;

use adk_model::AnthropicClient;
use adk_model::anthropic::{AnthropicConfig, Effort, ThinkingMode};

use crate::common::{AgentSpec, Harness, Keys, Mode, Outcome, Verdict, fail, pass, short};
use crate::upstream::Proxy;

const FINDING: &str = "ML25";
/// Claude Haiku 5.5 omits thinking text by default.
const MODEL: &str = "claude-haiku-5-5";

fn config(key: &str, base_url: &str) -> AnthropicConfig {
    let config = AnthropicConfig::new(key, MODEL)
        .with_base_url(base_url)
        .with_thinking_mode(ThinkingMode::Adaptive)
        .with_effort(Effort::High);
    #[cfg(feature = "fixed")]
    let config = config.with_thinking_display(adk_model::anthropic::ThinkingDisplay::Summarized);
    config
}

async fn summarized(key: &str) -> anyhow::Result<Verdict> {
    let proxy = Proxy::start("https://api.anthropic.com").await?;
    let client = AnthropicClient::new(config(key, &proxy.base))?;
    let harness =
        Harness::new("ml25", Arc::new(client), &AgentSpec::new("Reason carefully.")).await?;
    let turn = harness
        .turn(
            "A bat and a ball cost 1.10 in total. The bat costs 1.00 more than the ball. How much \
             does the ball cost? Work it out step by step before answering.",
        )
        .await;
    if let Some(error) = turn.error {
        return Ok(fail(format!("run failed: {}", short(&error, 200))));
    }
    let thinking = turn.thinking();
    let visible: usize = thinking.iter().map(|text| text.trim().len()).sum();
    let display = proxy
        .captured()
        .last()
        .and_then(|request| request.body.pointer("/thinking/display").cloned());
    if visible == 0 {
        return Ok(fail(format!(
            "no thinking text ({} thinking parts; request thinking.display = {display:?})",
            thinking.len()
        )));
    }
    Ok(pass(format!("{} thinking blocks, {visible} characters of summary", thinking.len())))
}

pub async fn run(keys: &Keys, outcomes: &mut Vec<Outcome>) {
    let verdict = match &keys.anthropic {
        None => Verdict::Skip("ANTHROPIC_API_KEY not set".into()),
        Some(key) => summarized(key).await.unwrap_or_else(|error| fail(format!("setup: {error}"))),
    };
    outcomes.push(Outcome {
        id: "ml25-anthropic-display",
        finding: FINDING,
        mode: Mode::Live,
        verdict,
    });
}
