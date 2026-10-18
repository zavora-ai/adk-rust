//! ML18: a stream that stalls after its first bytes must fail the run after the configured
//! idle timeout instead of hanging it.
//!
//! The stalled stream is scripted; a live call per client shows a client with timeouts
//! configured still completes a normal turn.

use std::sync::Arc;
use std::time::{Duration, Instant};

use adk_core::Llm;
use adk_model::azure_ai::{AzureAIClient, AzureAIConfig};
use adk_model::deepseek::{DeepSeekClient, DeepSeekConfig};
use adk_model::groq::{GroqClient, GroqConfig};
use adk_model::ollama::{OllamaConfig, OllamaModel};
use adk_model::openai::{OpenAIResponsesClient, OpenAIResponsesConfig};
use adk_model::{OpenAIClient, OpenAIConfig};
use serde_json::json;

use crate::common::{AgentSpec, Harness, Keys, Mode, Outcome, Verdict, fail, pass, short};
use crate::upstream::{Reply, Upstream, chat};

const FINDING: &str = "ML18";
/// How long the scenario waits before calling a stalled run hung.
const HANG: Duration = Duration::from_secs(10);

#[cfg(feature = "fixed")]
fn timeouts(idle: Duration) -> adk_model::HttpTimeouts {
    adk_model::HttpTimeouts::new(Duration::from_secs(5), idle)
}

/// Applies the timeouts on a build with the fix; returns the client unchanged otherwise.
macro_rules! with_timeouts {
    ($client:expr, $idle:expr) => {{
        #[cfg(feature = "fixed")]
        let client = $client.with_timeouts(timeouts($idle))?;
        #[cfg(not(feature = "fixed"))]
        let client = {
            let _ = $idle;
            $client
        };
        client
    }};
}

async fn stalled(
    id: &'static str,
    first: Reply,
    build: impl FnOnce(&str) -> anyhow::Result<Arc<dyn Llm>>,
) -> Outcome {
    let verdict = async {
        let upstream = Upstream::start().await?;
        upstream.push(first);
        let harness =
            Harness::new("ml18", build(&upstream.base)?, &AgentSpec::new("Be brief.")).await?;
        let started = Instant::now();
        let Ok(turn) = tokio::time::timeout(HANG, harness.turn("Hello.")).await else {
            return Ok(fail(format!("the stalled stream hung the run for {}s", HANG.as_secs())));
        };
        let elapsed = started.elapsed();
        Ok::<_, anyhow::Error>(match turn.error {
            None => fail("the run ended without an error"),
            Some(error) if elapsed < Duration::from_millis(1500) => fail(format!(
                "failed before the idle timeout ({elapsed:?}): {}",
                short(&error, 120)
            )),
            Some(error) => {
                pass(format!("failed after {:.1}s: {}", elapsed.as_secs_f32(), short(&error, 110)))
            }
        })
    }
    .await
    .unwrap_or_else(|error| fail(format!("setup: {error}")));
    Outcome { id, finding: FINDING, mode: Mode::Scripted, verdict }
}

fn chat_stall() -> Reply {
    Reply::Stall { content_type: "text/event-stream", first: chat::first_chunk() }
}

async fn happy(model: anyhow::Result<Arc<dyn Llm>>) -> Verdict {
    let result = async {
        let harness = Harness::new("ml18", model?, &AgentSpec::new("Be brief.")).await?;
        let turn = harness.turn("Say hello in one word.").await;
        Ok::<_, anyhow::Error>(match turn.error {
            Some(error) => fail(short(&error, 160)),
            None if turn.answer().trim().is_empty() => fail("empty answer"),
            None => pass(format!("answered {:?}", short(&turn.answer(), 30))),
        })
    }
    .await;
    result.unwrap_or_else(|error| fail(format!("setup: {error}")))
}

pub async fn run(keys: &Keys, outcomes: &mut Vec<Outcome>) {
    let idle = Duration::from_secs(2);
    outcomes.push(
        stalled("ml18-openai-chat", chat_stall(), |base| {
            let mut config = OpenAIConfig::new("test-key", "gpt-4.1-mini");
            config.base_url = Some(format!("{base}/v1"));
            Ok(Arc::new(with_timeouts!(OpenAIClient::new(config)?, idle)))
        })
        .await,
    );
    outcomes.push(
        stalled(
            "ml18-openai-responses",
            Reply::Stall { content_type: "text/event-stream", first: ": keep-alive\n\n".into() },
            |base| {
                let config = OpenAIResponsesConfig::new("test-key", "gpt-5.6-terra")
                    .with_base_url(format!("{base}/v1"));
                Ok(Arc::new(with_timeouts!(OpenAIResponsesClient::new(config)?, idle)))
            },
        )
        .await,
    );
    outcomes.push(
        stalled("ml18-groq", chat_stall(), |base| {
            let client =
                GroqClient::new(GroqConfig::new("test-key", "scripted").with_base_url(base))?;
            Ok(Arc::new(with_timeouts!(client, idle)))
        })
        .await,
    );
    outcomes.push(
        stalled("ml18-deepseek", chat_stall(), |base| {
            let client = DeepSeekClient::new(
                DeepSeekConfig::new("test-key", "deepseek-chat").with_base_url(base),
            )?;
            Ok(Arc::new(with_timeouts!(client, idle)))
        })
        .await,
    );
    outcomes.push(
        stalled("ml18-azure-ai", chat_stall(), |base| {
            let client = AzureAIClient::new(AzureAIConfig::new(base, "test-key", "scripted"))?;
            Ok(Arc::new(with_timeouts!(client, idle)))
        })
        .await,
    );
    outcomes.push(
        stalled(
            "ml18-ollama",
            Reply::Stall {
                content_type: "application/x-ndjson",
                first: format!(
                    "{}\n",
                    json!({"model": "scripted", "created_at": "2026-10-18T00:00:00Z",
                           "message": {"role": "assistant", "content": "Hel"}, "done": false})
                ),
            },
            |base| {
                let client = OllamaModel::new(OllamaConfig::with_host(base, "scripted"))?;
                Ok(Arc::new(with_timeouts!(client, idle)))
            },
        )
        .await,
    );

    // Live: clients configured with timeouts still complete a normal turn.
    let live_idle = Duration::from_secs(120);
    let mut verdicts = Vec::new();
    if let Some(key) = &keys.openai {
        let chat = (|| -> anyhow::Result<Arc<dyn Llm>> {
            Ok(Arc::new(with_timeouts!(
                OpenAIClient::new(OpenAIConfig::new(key, "gpt-5.6-terra"))?,
                live_idle
            )))
        })();
        verdicts.push(("openai-chat", happy(chat).await));
        let responses = (|| -> anyhow::Result<Arc<dyn Llm>> {
            let client =
                OpenAIResponsesClient::new(OpenAIResponsesConfig::new(key, "gpt-5.6-terra"))?;
            Ok(Arc::new(with_timeouts!(client, live_idle)))
        })();
        verdicts.push(("openai-responses", happy(responses).await));
    }
    if let Some(key) = &keys.groq {
        let groq = (|| -> anyhow::Result<Arc<dyn Llm>> {
            Ok(Arc::new(with_timeouts!(
                GroqClient::new(GroqConfig::new(key, "openai/gpt-oss-120b"))?,
                live_idle
            )))
        })();
        verdicts.push(("groq", happy(groq).await));
    }
    let verdict = if verdicts.is_empty() {
        Verdict::Skip("no OpenAI or Groq key".into())
    } else {
        let failed: Vec<String> = verdicts
            .iter()
            .filter_map(|(label, verdict)| match verdict {
                Verdict::Fail(detail) => Some(format!("{label}: {detail}")),
                Verdict::Pass(_) | Verdict::Skip(_) => None,
            })
            .collect();
        if failed.is_empty() {
            pass(verdicts.iter().map(|(label, _)| *label).collect::<Vec<_>>().join(", "))
        } else {
            fail(failed.join("; "))
        }
    };
    outcomes.push(Outcome {
        id: "ml18-live-happy-path",
        finding: FINDING,
        mode: Mode::Live,
        verdict,
    });
}
