//! Item 2: Ollama and OpenRouter Responses send or refuse every generation config field.

use std::sync::Arc;

use adk_core::GenerateContentConfig;
use adk_model::{OllamaConfig, OllamaModel, OpenRouterApiMode, OpenRouterClient, OpenRouterConfig};
use serde_json::{Value, json};

use crate::common::{Mode, Report, Verdict, agent_turn, check, describe, fail, pass, short};
use crate::upstream::{Reply, Upstream};

pub async fn run(report: &mut Report) {
    report.record_result("ollama-config-fields", 2, Mode::Scripted, ollama_fields().await);
    report.record_result("openrouter-responses-seed", 2, Mode::Scripted, openrouter_seed().await);
    match std::env::var("OLLAMA_MODEL").ok().filter(|model| !model.trim().is_empty()) {
        Some(model) => {
            report.record_result("ollama-live", 2, Mode::Live, ollama_live(&model).await)
        }
        None => report.record(
            "ollama-live",
            2,
            Mode::Live,
            Verdict::Skip("OLLAMA_MODEL not set".into()),
        ),
    }
}

/// Against a local Ollama: the same seed gives the same answer, a stop sequence cuts it, and
/// log probabilities come back.
async fn ollama_live(model_name: &str) -> anyhow::Result<Verdict> {
    let host =
        std::env::var("OLLAMA_HOST").unwrap_or_else(|_| "http://127.0.0.1:11434".to_string());
    let config = |stop: Vec<String>| GenerateContentConfig {
        seed: Some(42),
        temperature: Some(1.5),
        max_output_tokens: Some(60),
        top_logprobs: Some(2),
        stop_sequences: stop,
        ..Default::default()
    };
    let prompt = "Describe a fox in one sentence.";
    let model = || -> anyhow::Result<Arc<OllamaModel>> {
        Ok(Arc::new(OllamaModel::new(OllamaConfig::with_host(host.clone(), model_name))?))
    };
    let first = agent_turn(model()?, Some(config(Vec::new())), prompt).await?;
    let second = agent_turn(model()?, Some(config(Vec::new())), prompt).await?;
    if let Some(error) = first.error.as_ref().or(second.error.as_ref()) {
        return Ok(fail(format!("turn failed: {}", describe(error))));
    }
    let (first_answer, second_answer) = (first.answer(), second.answer());
    let tokens = first
        .final_response()
        .and_then(|response| response.provider_metadata.as_ref())
        .and_then(|metadata| metadata.pointer("/logprobs/content"))
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    // Stop on the answer's second word: the seeded run repeats the answer up to it.
    let Some(stop) = first_answer
        .split_whitespace()
        .nth(1)
        .map(|word| word.trim_matches(|c: char| !c.is_alphanumeric()).to_string())
        .filter(|word| word.len() >= 2)
    else {
        return Ok(fail(format!("answer too short to pick a stop word: {first_answer:?}")));
    };
    let stopped = agent_turn(model()?, Some(config(vec![stop.clone()])), prompt).await?;
    if let Some(error) = &stopped.error {
        return Ok(fail(format!("turn failed: {}", describe(error))));
    }
    let cut = stopped.answer();
    let seeded = !first_answer.is_empty() && first_answer == second_answer;
    let cut_short = !cut.contains(&stop) && first_answer.starts_with(cut.trim_end());
    Ok(check(
        seeded && cut_short && tokens > 0,
        format!(
            "seed 42 twice equal: {seeded}; stop {stop:?} -> {:?} (cut: {cut_short}); logprob tokens: {tokens}",
            short(&cut, 40)
        ),
    ))
}

/// Stop sequences, seed, and penalties reach `options`; log probabilities are requested and
/// returned in `provider_metadata`.
async fn ollama_fields() -> anyhow::Result<Verdict> {
    let upstream = Upstream::start().await?;
    let logprob = json!({"token": "Hi", "logprob": -0.01, "bytes": [72, 105], "top_logprobs": [
        {"token": "Hi", "logprob": -0.01, "bytes": [72, 105]},
        {"token": "Hello", "logprob": -4.6, "bytes": [72, 101, 108, 108, 111]}
    ]});
    upstream.push(Reply::Ndjson(vec![
        json!({"model": "scripted", "created_at": "2026-10-19T00:00:00Z",
               "message": {"role": "assistant", "content": "Hi"}, "done": false,
               "logprobs": [logprob]}),
        json!({"model": "scripted", "created_at": "2026-10-19T00:00:00Z",
               "message": {"role": "assistant", "content": ""}, "done": true, "done_reason": "stop",
               "prompt_eval_count": 5, "eval_count": 1}),
    ]));
    let model = OllamaModel::new(OllamaConfig::with_host(upstream.base.clone(), "scripted"))?;
    let config = GenerateContentConfig {
        stop_sequences: vec!["END".to_string()],
        seed: Some(7),
        presence_penalty: Some(0.25),
        frequency_penalty: Some(0.5),
        top_logprobs: Some(2),
        ..Default::default()
    };
    let turn = agent_turn(Arc::new(model), Some(config), "Say hi.").await?;
    if let Some(error) = &turn.error {
        return Ok(fail(format!("turn failed: {}", describe(error))));
    }
    let Some(body) = upstream.captured().first().map(|request| request.body.clone()) else {
        return Ok(fail("no request reached the upstream"));
    };
    let expected = [
        ("/options/stop", json!(["END"])),
        ("/options/seed", json!(7)),
        ("/options/presence_penalty", json!(0.25)),
        ("/options/frequency_penalty", json!(0.5)),
        ("/logprobs", json!(true)),
        ("/top_logprobs", json!(2)),
    ];
    let missing: Vec<&str> = expected
        .iter()
        .filter(|(pointer, value)| body.pointer(pointer) != Some(value))
        .map(|(pointer, _)| *pointer)
        .collect();
    let returned = turn
        .final_response()
        .and_then(|response| response.provider_metadata.as_ref())
        .and_then(|metadata| metadata.pointer("/logprobs/content/0/token"))
        .and_then(Value::as_str)
        == Some("Hi");
    Ok(check(
        missing.is_empty() && returned,
        format!(
            "options {} ; missing {missing:?} ; provider_metadata.logprobs returned: {returned}",
            short(&body["options"].to_string(), 140)
        ),
    ))
}

/// OpenRouter's Responses API has no `seed`, so the request fails naming it and is not sent.
async fn openrouter_seed() -> anyhow::Result<Verdict> {
    let upstream = Upstream::start().await?;
    let client = OpenRouterClient::new(
        OpenRouterConfig::new("test-key", "openai/gpt-5.6-terra")
            .with_base_url(format!("{}/api/v1", upstream.base))
            .with_default_api_mode(OpenRouterApiMode::Responses),
    )?;
    let config = GenerateContentConfig { seed: Some(7), ..Default::default() };
    let turn = agent_turn(Arc::new(client), Some(config), "Say hi.").await?;
    let sent = upstream.captured().len();
    Ok(match &turn.error {
        Some(error) if error.code == "model.unsupported_config" && sent == 0 => {
            pass(format!("{} ; requests sent: {sent}", describe(error)))
        }
        Some(error) => fail(format!("{} ; requests sent: {sent}", describe(error))),
        None => fail(format!("the turn succeeded; requests sent: {sent}")),
    })
}
