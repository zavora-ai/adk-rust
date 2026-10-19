//! Item 1: `GeminiModel` sends stop sequences, seed, penalties, and log probabilities.

use std::sync::Arc;

use adk_core::{ErrorCategory, GenerateContentConfig, Llm};
use adk_model::GeminiModel;
use serde_json::{Value, json};

use crate::common::{Mode, Report, Verdict, agent_turn, check, describe, fail, pass, short};
use crate::upstream::{Reply, Upstream, sse};

const LIVE_MODEL: &str = "gemini-3.5-flash-lite";

fn every_field() -> GenerateContentConfig {
    GenerateContentConfig {
        stop_sequences: vec!["END".to_string()],
        seed: Some(7),
        presence_penalty: Some(0.25),
        frequency_penalty: Some(0.5),
        top_logprobs: Some(2),
        ..Default::default()
    }
}

pub async fn run(gemini_key: Option<&str>, report: &mut Report) {
    report.record_result("gemini-config-fields", 1, Mode::Scripted, config_fields().await);
    report.record_result(
        "gemini-interactions-unsupported",
        1,
        Mode::Scripted,
        interactions_unsupported().await,
    );
    let Some(key) = gemini_key else {
        for id in ["gemini-live-stop", "gemini-live-seed", "gemini-live-penalty"] {
            report.record(id, 1, Mode::Live, Verdict::Skip("GEMINI_API_KEY not set".into()));
        }
        return;
    };
    report.record_result("gemini-live-stop", 1, Mode::Live, live_stop(key).await);
    report.record_result("gemini-live-seed", 1, Mode::Live, live_seed(key).await);
    report.record_result("gemini-live-penalty", 1, Mode::Live, live_penalty(key).await);
}

/// Every field reaches `generationConfig`, and returned log probabilities reach the event.
async fn config_fields() -> anyhow::Result<Verdict> {
    let upstream = Upstream::start().await?;
    upstream.push(Reply::Sse(vec![sse(&json!({
        "candidates": [{
            "content": {"role": "model", "parts": [{"text": "Hi"}]},
            "finishReason": "STOP",
            "logprobsResult": {
                "topCandidates": [{"candidates": [
                    {"token": "Hi", "tokenId": 17, "logProbability": -0.01},
                    {"token": "Hello", "tokenId": 18, "logProbability": -4.6}
                ]}],
                "chosenCandidates": [{"token": "Hi", "tokenId": 17, "logProbability": -0.01}],
                "logProbabilitySum": -0.01
            },
            "avgLogprobs": -0.01
        }],
        "usageMetadata": {"promptTokenCount": 5, "candidatesTokenCount": 1, "totalTokenCount": 6},
        "modelVersion": LIVE_MODEL
    }))]));
    let model = GeminiModel::new_with_base_url(
        "test-key",
        LIVE_MODEL,
        format!("{}/v1beta/", upstream.base),
    )?;
    let turn = agent_turn(Arc::new(model), Some(every_field()), "Say hi.").await?;
    if let Some(error) = &turn.error {
        return Ok(fail(format!("turn failed: {}", describe(error))));
    }
    let captured = upstream.captured();
    let Some(config) = captured.first().map(|request| request.body["generationConfig"].clone())
    else {
        return Ok(fail("no request reached the upstream"));
    };
    let expected = [
        ("stopSequences", json!(["END"])),
        ("seed", json!(7)),
        ("presencePenalty", json!(0.25)),
        ("frequencyPenalty", json!(0.5)),
        ("responseLogprobs", json!(true)),
        ("logprobs", json!(2)),
    ];
    let missing: Vec<&str> = expected
        .iter()
        .filter(|(field, value)| config.get(field) != Some(value))
        .map(|(field, _)| *field)
        .collect();
    let logprobs = turn
        .final_response()
        .and_then(|response| response.provider_metadata.as_ref())
        .and_then(|metadata| metadata.get("logprobs"))
        .cloned();
    let returned = logprobs
        .as_ref()
        .and_then(|logprobs| logprobs.pointer("/chosenCandidates/0/token"))
        .and_then(Value::as_str)
        == Some("Hi");
    Ok(check(
        missing.is_empty() && returned,
        format!(
            "generationConfig {} ; missing {missing:?} ; provider_metadata.logprobs returned: {returned}",
            short(&config.to_string(), 160)
        ),
    ))
}

/// The Interactions transport has no penalty or log-probability field, so it refuses them.
async fn interactions_unsupported() -> anyhow::Result<Verdict> {
    let upstream = Upstream::start().await?;
    let model = GeminiModel::new_with_base_url(
        "test-key",
        LIVE_MODEL,
        format!("{}/v1beta/", upstream.base),
    )?
    .use_interactions_api(true)?;
    let config = GenerateContentConfig {
        presence_penalty: Some(0.25),
        top_logprobs: Some(2),
        ..Default::default()
    };
    let turn = agent_turn(Arc::new(model), Some(config), "Say hi.").await?;
    let sent = upstream.captured().len();
    Ok(match &turn.error {
        Some(error) if error.code == "model.unsupported_config" && sent == 0 => {
            pass(format!("{} ; requests sent: {sent}", describe(error)))
        }
        Some(error) => fail(format!("{} ; requests sent: {sent}", describe(error))),
        None => fail(format!("the turn succeeded; requests sent: {sent}")),
    })
}

fn live_model(key: &str) -> anyhow::Result<Arc<dyn Llm>> {
    Ok(Arc::new(GeminiModel::new(key, LIVE_MODEL)?))
}

/// A stop sequence ends the answer where it appears.
async fn live_stop(key: &str) -> anyhow::Result<Verdict> {
    let config = GenerateContentConfig {
        stop_sequences: vec!["5".to_string()],
        max_output_tokens: Some(200),
        ..Default::default()
    };
    let turn = agent_turn(
        live_model(key)?,
        Some(config),
        "Count from 1 to 10, separated by commas. Output only the numbers.",
    )
    .await?;
    if let Some(error) = &turn.error {
        return Ok(fail(format!("turn failed: {}", describe(error))));
    }
    let answer = turn.answer();
    Ok(check(
        answer.contains('4') && !answer.contains('6'),
        format!("answer {:?} (stop sequence \"5\")", short(&answer, 80)),
    ))
}

/// The same seed at temperature 2 gives the same answer twice.
async fn live_seed(key: &str) -> anyhow::Result<Verdict> {
    let config = GenerateContentConfig {
        seed: Some(1234),
        temperature: Some(2.0),
        max_output_tokens: Some(200),
        ..Default::default()
    };
    let prompt = "Invent a name for a fictional fox and give it a one-sentence backstory.";
    let first = agent_turn(live_model(key)?, Some(config.clone()), prompt).await?;
    let second = agent_turn(live_model(key)?, Some(config), prompt).await?;
    if let Some(error) = first.error.as_ref().or(second.error.as_ref()) {
        return Ok(fail(format!("turn failed: {}", describe(error))));
    }
    let (first, second) = (first.answer(), second.answer());
    Ok(check(
        !first.is_empty() && first == second,
        format!("seed 1234: {:?} / {:?}", short(&first, 60), short(&second, 60)),
    ))
}

/// A presence penalty reaches Gemini, which answers for the model whether it supports one.
async fn live_penalty(key: &str) -> anyhow::Result<Verdict> {
    let config = GenerateContentConfig { presence_penalty: Some(0.5), ..Default::default() };
    let turn = agent_turn(live_model(key)?, Some(config), "Say hi.").await?;
    Ok(match &turn.error {
        Some(error)
            if error.category == ErrorCategory::InvalidInput
                && error.message.contains("Penalty") =>
        {
            pass(format!("Gemini refused the field: {}", describe(error)))
        }
        Some(error) => fail(format!("unexpected error: {}", describe(error))),
        None => fail(format!(
            "the turn succeeded, so the penalty never reached Gemini (it rejects penalties for {LIVE_MODEL}); answer {:?}",
            short(&turn.answer(), 60)
        )),
    })
}
