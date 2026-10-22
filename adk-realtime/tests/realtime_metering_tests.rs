//! Realtime usage metered in the run's spend ledger (ROADMAP "Realtime usage is not
//! reported, so realtime spend is unmetered").
//!
//! On `main` a `ResponseDone` carried its usage past every runner untouched: the ledger
//! stayed at zero whatever a session cost. These tests fail there: nothing reads the usage
//! and nothing reserves or commits.

#![cfg(all(feature = "integration", feature = "scripted"))]

use std::sync::Arc;
use std::time::Duration;

use adk_core::{
    InMemorySpendLedger, LlmSpendEstimate, RunConfig, SpendKey, SpendLedger, SpendLimits,
    SpendPeriod,
};
use adk_realtime::integration::IntegratedRealtimeRunner;
use adk_realtime::metering::RealtimePricing;
use adk_realtime::scripted::{self, ScriptedRealtimeModel, ScriptedReply};
use adk_realtime::{RealtimeError, RealtimeRunner, ServerEvent};
use serde_json::json;

const APP: &str = "voice";

fn openai_done(input: u64, output: u64) -> ServerEvent {
    scripted::response_done(json!({ "response": { "usage": {
        "input_tokens": input,
        "output_tokens": output,
        "total_tokens": input + output,
        "input_token_details": { "text_tokens": input, "audio_tokens": 0, "cached_tokens": 0 },
        "output_token_details": { "text_tokens": 0, "audio_tokens": output }
    } } }))
}

fn pricing() -> RealtimePricing {
    RealtimePricing {
        input_text_micro_usd_per_million: 1_000_000,
        output_audio_micro_usd_per_million: 2_000_000,
        ..RealtimePricing::default()
    }
}

async fn drain(runner: &IntegratedRealtimeRunner) -> Vec<adk_realtime::Result<ServerEvent>> {
    let mut events = Vec::new();
    while let Some(event) = runner.next_event().await {
        events.push(event);
    }
    events
}

#[tokio::test]
async fn an_openai_response_is_priced_and_committed() {
    let model = ScriptedRealtimeModel::new(vec![
        ScriptedReply::new(vec![
            scripted::response_created(),
            scripted::transcript_delta("hello"),
            openai_done(1_000_000, 500_000),
        ])
        .on_text(),
    ])
    .with_model_id("gpt-realtime")
    .with_idle_timeout(Duration::from_millis(50));
    let ledger = Arc::new(InMemorySpendLedger::new(SpendLimits::new()));
    let runner = IntegratedRealtimeRunner::builder()
        .model(Arc::new(model))
        .identity(APP, "ana", "s1")
        .tool_governance(RunConfig::builder().spend_ledger(ledger.clone()).build())
        .realtime_pricing(pricing())
        .build()
        .unwrap();
    runner.connect().await.unwrap();
    runner.send_text("hi").await.unwrap();
    let events = drain(&runner).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");

    // 1M input text tokens at $1/M plus 0.5M output audio tokens at $2/M.
    let openai = SpendKey::org(APP).with_vendor("openai");
    assert_eq!(ledger.spent(&openai, SpendPeriod::Day).await.unwrap(), 2_000_000);
    let _ = runner.close().await;
}

#[tokio::test]
async fn a_gemini_usage_report_without_a_start_event_is_metered() {
    let model = ScriptedRealtimeModel::new(vec![
        ScriptedReply::new(vec![
            scripted::transcript_delta("hello"),
            scripted::response_done(json!({
                "serverContent": { "turnComplete": true },
                "usageMetadata": {
                    "promptTokenCount": 2_000_000,
                    "responseTokenCount": 1_000_000,
                    "totalTokenCount": 3_000_000,
                    "responseTokensDetails": [{ "modality": "AUDIO", "tokenCount": 1_000_000 }]
                }
            })),
        ])
        .on_text(),
    ])
    .with_model_id("gemini-3.1-flash-live-preview")
    .with_idle_timeout(Duration::from_millis(50));
    let ledger = Arc::new(InMemorySpendLedger::new(SpendLimits::new()));
    let runner = IntegratedRealtimeRunner::builder()
        .model(Arc::new(model))
        .identity(APP, "ana", "s1")
        .tool_governance(RunConfig::builder().spend_ledger(ledger.clone()).build())
        .realtime_pricing(pricing())
        .build()
        .unwrap();
    runner.connect().await.unwrap();
    runner.send_text("hi").await.unwrap();
    let events = drain(&runner).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");

    let gemini = SpendKey::org(APP).with_vendor("gemini");
    assert_eq!(ledger.spent(&gemini, SpendPeriod::Day).await.unwrap(), 2_000_000 + 2_000_000);
    let _ = runner.close().await;
}

#[tokio::test]
async fn without_pricing_the_run_estimate_is_committed() {
    let model = ScriptedRealtimeModel::new(vec![
        ScriptedReply::new(vec![scripted::response_created(), openai_done(10, 10)]).on_text(),
    ])
    .with_model_id("gpt-realtime")
    .with_idle_timeout(Duration::from_millis(50));
    let ledger = Arc::new(InMemorySpendLedger::new(SpendLimits::new()));
    let config = RunConfig::builder()
        .spend_ledger(ledger.clone())
        .llm_spend_estimate(LlmSpendEstimate::per_call(7_000))
        .build();
    let runner = IntegratedRealtimeRunner::builder()
        .model(Arc::new(model))
        .identity(APP, "ana", "s1")
        .tool_governance(config)
        .build()
        .unwrap();
    runner.connect().await.unwrap();
    runner.send_text("hi").await.unwrap();
    drain(&runner).await;
    assert_eq!(ledger.spent(&SpendKey::org(APP), SpendPeriod::Day).await.unwrap(), 7_000);
    let _ = runner.close().await;
}

#[tokio::test]
async fn a_spend_cap_refuses_the_response_and_ends_the_session() {
    let model = ScriptedRealtimeModel::new(vec![
        ScriptedReply::new(vec![scripted::response_created(), openai_done(10, 10)]).on_text(),
    ])
    .with_model_id("gpt-realtime")
    .with_idle_timeout(Duration::from_millis(50));
    let limits = SpendLimits::new().limit(SpendKey::org(APP), 1_000);
    let ledger = Arc::new(InMemorySpendLedger::new(limits));
    let runner = IntegratedRealtimeRunner::builder()
        .model(Arc::new(model))
        .identity(APP, "ana", "s1")
        .tool_governance(RunConfig::builder().spend_ledger(ledger.clone()).build())
        .build()
        .unwrap();
    runner.connect().await.unwrap();
    runner.send_text("hi").await.unwrap();
    let first = runner.next_event().await.expect("an event");
    assert!(matches!(first, Err(RealtimeError::SpendRefused(_))), "{first:?}");
    assert_eq!(ledger.spent(&SpendKey::org(APP), SpendPeriod::Day).await.unwrap(), 0);
    let _ = runner.close().await;
}

#[tokio::test]
async fn the_plain_runner_meters_its_responses_too() {
    let model = ScriptedRealtimeModel::new(vec![
        ScriptedReply::new(vec![scripted::response_created(), openai_done(1_000_000, 0)]).on_text(),
    ])
    .with_model_id("gpt-realtime")
    .with_idle_timeout(Duration::from_millis(50));
    let ledger = Arc::new(InMemorySpendLedger::new(SpendLimits::new()));
    let runner = RealtimeRunner::builder()
        .model(Arc::new(model))
        .tool_governance(
            RunConfig::builder().spend_ledger(ledger.clone()).build(),
            adk_core::ApprovalScope::new(APP, "ana", "s1"),
        )
        .realtime_pricing(pricing())
        .build()
        .unwrap();
    runner.connect().await.unwrap();
    runner.send_text("hi").await.unwrap();
    while let Some(event) = runner.next_event().await {
        event.unwrap();
    }
    assert_eq!(ledger.spent(&SpendKey::org(APP), SpendPeriod::Day).await.unwrap(), 1_000_000);
    let _ = runner.close().await;
}
