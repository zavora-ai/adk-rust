//! Item 6: a summary whose stream fails ends with `model.summary_incomplete`, keeping the cause.

use std::sync::Arc;
use std::time::Duration;

use adk_agent::LlmEventSummarizer;
use adk_core::{AdkError, BaseEventsSummarizer, Content, ErrorCategory, Event, Llm};
use adk_model::{HttpTimeouts, OpenAIClient, OpenAIConfig, RetryConfig};
use adk_runner::compaction::{CompactionStrategy, SummarisationCompaction};

use crate::common::{Mode, Report, Verdict, check, describe, fail};
use crate::upstream::{Reply, Upstream};

pub async fn run(report: &mut Report) {
    report.record_result(
        "compaction-summarizer-stream-error",
        6,
        Mode::Scripted,
        summarizer().await,
    );
    report.record_result("compaction-strategy-stream-error", 6, Mode::Scripted, strategy().await);
}

/// An OpenAI client whose upstream sends the headers of a complete response, then stalls.
async fn stalling_model() -> anyhow::Result<(Upstream, Arc<dyn Llm>)> {
    let upstream = Upstream::start().await?;
    upstream.push(Reply::Stall { content_type: "application/json", first: "{".to_string() });
    let client = OpenAIClient::new(OpenAIConfig::compatible(
        "test-key",
        format!("{}/v1", upstream.base),
        "gpt-5.6-terra",
    ))?
    .with_timeouts(HttpTimeouts::new(Duration::from_secs(1), Duration::from_secs(1)))?
    .with_retry_config(RetryConfig::disabled());
    Ok((upstream, Arc::new(client)))
}

fn events(count: usize) -> Vec<Event> {
    (0..count)
        .map(|index| {
            let mut event = Event::new("inv-1");
            event.author = if index % 2 == 0 { "user" } else { "desk" }.to_string();
            event.set_content(
                Content::new(if index % 2 == 0 { "user" } else { "model" }).with_text(format!(
                    "Turn {index}: order A{index} ships from the Nairobi warehouse on Monday."
                )),
            );
            event
        })
        .collect()
}

fn verdict(error: Option<AdkError>) -> Verdict {
    let Some(error) = error else {
        return fail("the summary succeeded against a stalled upstream");
    };
    let cause = error.details.metadata.get("cause_code").and_then(|code| code.as_str());
    check(
        error.code == "model.summary_incomplete"
            && error.category == ErrorCategory::Timeout
            && cause.is_some(),
        format!("{} ; cause_code {cause:?}", describe(&error)),
    )
}

async fn summarizer() -> anyhow::Result<Verdict> {
    let (_upstream, model) = stalling_model().await?;
    let result = LlmEventSummarizer::new(model).summarize_events(&events(4)).await;
    Ok(verdict(result.err()))
}

async fn strategy() -> anyhow::Result<Verdict> {
    let (_upstream, model) = stalling_model().await?;
    let strategy = SummarisationCompaction { model, turns_to_summarise: 4 };
    let result = strategy.compact(events(8), 4096).await;
    Ok(verdict(result.err()))
}
