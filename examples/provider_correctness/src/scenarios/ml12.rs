//! ML12: generation config fields (stop sequences, seed, penalties, log probabilities) reach
//! each provider that supports them, and a field a provider cannot send fails the request with
//! an error naming it instead of being dropped.

use std::sync::Arc;

use adk_core::{GenerateContentConfig, Llm};
use adk_model::anthropic::AnthropicConfig;
use adk_model::azure_ai::{AzureAIClient, AzureAIConfig};
use adk_model::deepseek::{DeepSeekClient, DeepSeekConfig};
use adk_model::groq::{GroqClient, GroqConfig};
use adk_model::openai::{OpenAIResponsesClient, OpenAIResponsesConfig};
use adk_model::{AnthropicClient, OpenAIClient, OpenAIConfig};
use serde_json::{Value, json};

use crate::common::{AgentSpec, Harness, Keys, Mode, Outcome, Verdict, fail, pass, short};
use crate::upstream::{Proxy, Reply, Upstream, chat};

const FINDING: &str = "ML12";
const COUNT: &str = "Count from 1 to 12, separated by commas. Output nothing else.";

fn stop_config() -> GenerateContentConfig {
    GenerateContentConfig { stop_sequences: vec!["7".to_string()], ..Default::default() }
}

/// Runs the counting prompt and checks the stop sequence cut the answer before 8.
async fn honours_stop(
    model: Arc<dyn Llm>,
    config: GenerateContentConfig,
    sent: impl Fn() -> Option<Value>,
    expected: &[(&str, Value)],
) -> anyhow::Result<Verdict> {
    let harness =
        Harness::new("ml12", model, &AgentSpec::new("Follow instructions exactly.").config(config))
            .await?;
    let turn = harness.turn(COUNT).await;
    if let Some(error) = &turn.error {
        if error.contains("402") || error.contains("Insufficient Balance") {
            return Ok(Verdict::Skip(format!(
                "account cannot pay for calls: {}",
                short(error, 120)
            )));
        }
        return Ok(fail(format!("run failed: {}", short(error, 200))));
    }
    let Some(body) = sent() else {
        return Ok(fail("no request was captured"));
    };
    let missing: Vec<String> = expected
        .iter()
        .filter(|(pointer, value)| !same(body.pointer(pointer), value))
        .map(|(pointer, value)| format!("{pointer}={value} (sent {:?})", body.pointer(pointer)))
        .collect();
    if !missing.is_empty() {
        return Ok(fail(format!("request lacks {}", missing.join(", "))));
    }
    let answer = turn.answer();
    if answer.contains('8') || answer.contains("12") {
        return Ok(fail(format!("stop sequence ignored: {}", short(&answer, 60))));
    }
    Ok(pass(format!("answer stopped at the sequence: {:?}", short(&answer, 40))))
}

/// Runs one turn with `config` and expects an error naming `field` before any request.
async fn rejects(
    model: Arc<dyn Llm>,
    config: GenerateContentConfig,
    field: &str,
) -> anyhow::Result<Verdict> {
    let harness =
        Harness::new("ml12", model, &AgentSpec::new("Follow instructions exactly.").config(config))
            .await?;
    let turn = harness.turn("Say hello.").await;
    Ok(match turn.error {
        Some(error) if error.contains(field) => pass(format!("rejected: {}", short(&error, 110))),
        Some(error) => fail(format!("failed without naming `{field}`: {}", short(&error, 150))),
        None => fail(format!(
            "`{field}` was dropped silently; the run answered {:?}",
            short(&turn.answer(), 50)
        )),
    })
}

/// Equality that tolerates the f32-to-f64 widening of numbers in request bodies.
fn same(sent: Option<&Value>, expected: &Value) -> bool {
    match (sent, expected) {
        (Some(Value::Number(sent)), Value::Number(expected)) => {
            (sent.as_f64().unwrap_or(f64::NAN) - expected.as_f64().unwrap_or(f64::NAN)).abs() < 1e-6
        }
        (Some(sent), expected) => sent == expected,
        (None, _) => false,
    }
}

fn last_body(proxy: &Proxy) -> impl Fn() -> Option<Value> + '_ {
    move || proxy.captured().last().map(|request| request.body.clone())
}

fn merge(verdicts: Vec<(&str, Verdict)>) -> Verdict {
    let mut details = Vec::new();
    let mut failed = false;
    let mut all_skipped = true;
    for (label, verdict) in verdicts {
        match verdict {
            Verdict::Pass(detail) => {
                all_skipped = false;
                details.push(format!("{label}: {detail}"));
            }
            Verdict::Fail(detail) => {
                all_skipped = false;
                failed = true;
                details.push(format!("{label}: FAIL {detail}"));
            }
            Verdict::Skip(detail) => details.push(format!("{label}: skipped ({detail})")),
        }
    }
    let detail = details.join("; ");
    if failed {
        Verdict::Fail(detail)
    } else if all_skipped {
        Verdict::Skip(detail)
    } else {
        Verdict::Pass(detail)
    }
}

fn setup(result: anyhow::Result<Verdict>) -> Verdict {
    result.unwrap_or_else(|error| fail(format!("setup: {error}")))
}

async fn openai_chat(key: &str) -> Verdict {
    // GPT-5 models reject `stop`; gpt-4.1-mini accepts every field.
    let honoured = async {
        let proxy = Proxy::start("https://api.openai.com").await?;
        let mut config = OpenAIConfig::new(key, "gpt-4.1-mini");
        config.base_url = Some(format!("{}/v1", proxy.base));
        let generation = GenerateContentConfig {
            seed: Some(42),
            presence_penalty: Some(0.1),
            frequency_penalty: Some(0.1),
            top_logprobs: Some(2),
            ..stop_config()
        };
        honours_stop(
            Arc::new(OpenAIClient::new(config)?),
            generation,
            last_body(&proxy),
            &[
                ("/stop", json!(["7"])),
                ("/seed", json!(42)),
                ("/presence_penalty", json!(0.1)),
                ("/frequency_penalty", json!(0.1)),
                ("/logprobs", json!(true)),
                ("/top_logprobs", json!(2)),
            ],
        )
        .await
    }
    .await;
    setup(honoured)
}

async fn responses(key: &str) -> Verdict {
    let client = || -> anyhow::Result<Arc<dyn Llm>> {
        Ok(Arc::new(OpenAIResponsesClient::new(OpenAIResponsesConfig::new(key, "gpt-5.6-terra"))?))
    };
    let stop = async { rejects(client()?, stop_config(), "stop_sequences").await }.await;
    let seed = async {
        rejects(client()?, GenerateContentConfig { seed: Some(7), ..Default::default() }, "seed")
            .await
    }
    .await;
    merge(vec![("stop_sequences", setup(stop)), ("seed", setup(seed))])
}

async fn anthropic(key: &str) -> Verdict {
    let honoured = async {
        let proxy = Proxy::start("https://api.anthropic.com").await?;
        let mut config = AnthropicConfig::new(key, "claude-sonnet-4-6");
        config.base_url = Some(proxy.base.clone());
        honours_stop(
            Arc::new(AnthropicClient::new(config)?),
            stop_config(),
            last_body(&proxy),
            &[("/stop_sequences", json!(["7"]))],
        )
        .await
    }
    .await;
    let seed = async {
        let client = AnthropicClient::new(AnthropicConfig::new(key, "claude-sonnet-4-6"))?;
        rejects(
            Arc::new(client),
            GenerateContentConfig { seed: Some(7), ..Default::default() },
            "seed",
        )
        .await
    }
    .await;
    merge(vec![("stop_sequences", setup(honoured)), ("seed", setup(seed))])
}

async fn groq(key: &str) -> Verdict {
    let honoured = async {
        let proxy = Proxy::start("https://api.groq.com").await?;
        let config = GroqConfig::new(key, "qwen/qwen3.8-27b")
            .with_max_tokens(300)
            .with_base_url(format!("{}/openai/v1", proxy.base));
        let generation = GenerateContentConfig {
            seed: Some(42),
            presence_penalty: Some(0.1),
            frequency_penalty: Some(0.1),
            ..stop_config()
        };
        honours_stop(
            Arc::new(GroqClient::new(config)?),
            generation,
            last_body(&proxy),
            &[
                ("/stop", json!(["7"])),
                ("/seed", json!(42)),
                ("/presence_penalty", json!(0.1)),
                ("/frequency_penalty", json!(0.1)),
            ],
        )
        .await
    }
    .await;
    let logprobs = async {
        let client = GroqClient::new(GroqConfig::new(key, "qwen/qwen3.8-27b"))?;
        rejects(
            Arc::new(client),
            GenerateContentConfig { top_logprobs: Some(2), ..Default::default() },
            "top_logprobs",
        )
        .await
    }
    .await;
    merge(vec![("stop/seed/penalties", setup(honoured)), ("top_logprobs", setup(logprobs))])
}

async fn deepseek(key: &str) -> Verdict {
    let honoured = async {
        let proxy = Proxy::start("https://api.deepseek.com").await?;
        let config = DeepSeekConfig::v4_flash(key).with_base_url(proxy.base.clone());
        honours_stop(
            Arc::new(DeepSeekClient::new(config)?),
            GenerateContentConfig { presence_penalty: Some(0.1), ..stop_config() },
            last_body(&proxy),
            &[("/stop", json!(["7"])), ("/presence_penalty", json!(0.1))],
        )
        .await
    }
    .await;
    let seed = async {
        let client = DeepSeekClient::new(DeepSeekConfig::v4_flash(key))?;
        rejects(
            Arc::new(client),
            GenerateContentConfig { seed: Some(7), ..Default::default() },
            "seed",
        )
        .await
    }
    .await;
    merge(vec![("stop/penalty", setup(honoured)), ("seed", setup(seed))])
}

async fn azure_ai_scripted() -> Verdict {
    let honoured = async {
        let upstream = Upstream::start().await?;
        upstream.push(Reply::Sse(chat::turn(Some("1, 2, 3, 4, 5, 6, "), &[])));
        let client =
            AzureAIClient::new(AzureAIConfig::new(&upstream.base, "test-key", "scripted"))?;
        let generation = GenerateContentConfig {
            seed: Some(42),
            presence_penalty: Some(0.1),
            frequency_penalty: Some(0.1),
            ..stop_config()
        };
        honours_stop(
            Arc::new(client),
            generation,
            || upstream.captured().last().map(|request| request.body.clone()),
            &[
                ("/stop", json!(["7"])),
                ("/seed", json!(42)),
                ("/presence_penalty", json!(0.1)),
                ("/frequency_penalty", json!(0.1)),
            ],
        )
        .await
    }
    .await;
    let logprobs = async {
        let upstream = Upstream::start().await?;
        upstream.push(Reply::Sse(chat::turn(Some("Hello."), &[])));
        let client =
            AzureAIClient::new(AzureAIConfig::new(&upstream.base, "test-key", "scripted"))?;
        rejects(
            Arc::new(client),
            GenerateContentConfig { top_logprobs: Some(2), ..Default::default() },
            "top_logprobs",
        )
        .await
    }
    .await;
    merge(vec![("stop/seed/penalties", setup(honoured)), ("top_logprobs", setup(logprobs))])
}

pub async fn run(keys: &Keys, outcomes: &mut Vec<Outcome>) {
    let skip = |name: &str| Verdict::Skip(format!("{name} not set"));
    let verdict = match &keys.openai {
        Some(key) => openai_chat(key).await,
        None => skip("OPENAI_API_KEY"),
    };
    outcomes.push(Outcome { id: "ml12-openai-chat", finding: FINDING, mode: Mode::Live, verdict });
    let verdict = match &keys.openai {
        Some(key) => responses(key).await,
        None => skip("OPENAI_API_KEY"),
    };
    outcomes.push(Outcome { id: "ml12-responses", finding: FINDING, mode: Mode::Live, verdict });
    let verdict = match &keys.anthropic {
        Some(key) => anthropic(key).await,
        None => skip("ANTHROPIC_API_KEY"),
    };
    outcomes.push(Outcome { id: "ml12-anthropic", finding: FINDING, mode: Mode::Live, verdict });
    let verdict = match &keys.groq {
        Some(key) => groq(key).await,
        None => skip("GROQ_API_KEY"),
    };
    outcomes.push(Outcome { id: "ml12-groq", finding: FINDING, mode: Mode::Live, verdict });
    let verdict = match &keys.deepseek {
        Some(key) => deepseek(key).await,
        None => skip("DEEPSEEK_API_KEY"),
    };
    outcomes.push(Outcome { id: "ml12-deepseek", finding: FINDING, mode: Mode::Live, verdict });
    outcomes.push(Outcome {
        id: "ml12-azure-ai",
        finding: FINDING,
        mode: Mode::Scripted,
        verdict: azure_ai_scripted().await,
    });
}
