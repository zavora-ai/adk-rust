//! Muse image and transcription calls are metered against the current run: the spend hold
//! covers the call before it is sent, and the units it used are committed at the catalog's
//! per-unit price.
#![cfg(feature = "muse")]

use std::sync::Arc;

use adk_core::{
    BudgetTracker, InMemorySpendLedger, ModelCallScope, RunBudget, SpendKey, SpendLedger,
    SpendLimits, SpendPeriod, UsageUnit, UsageVector,
};
use adk_model::muse::{ImageOptions, MuseAsrClient, MuseConfig, MuseImagesClient};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ledger(daily_cap_micro_usd: u64) -> Arc<InMemorySpendLedger> {
    let key = SpendKey::org("studio").per(SpendPeriod::Day);
    Arc::new(InMemorySpendLedger::new(SpendLimits::new().limit(key, daily_cap_micro_usd)))
}

async fn muse_spent(ledger: &InMemorySpendLedger) -> u64 {
    ledger.spent(&SpendKey::org("studio").with_vendor("muse"), SpendPeriod::Day).await.unwrap()
}

async fn images_server(returned: usize) -> MockServer {
    let server = MockServer::start().await;
    let data: Vec<_> = (0..returned).map(|_| json!({ "b64_json": "aGVsbG8=" })).collect();
    Mock::given(method("POST"))
        .and(path("/images/generations"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "created": 1, "data": data })),
        )
        .mount(&server)
        .await;
    server
}

fn images_client(server: &MockServer) -> MuseImagesClient {
    MuseImagesClient::new(MuseConfig::new("test-key", "muse-image-1.0").with_base_url(server.uri()))
        .unwrap()
}

#[tokio::test]
async fn an_image_call_over_the_daily_cap_is_refused_before_it_is_sent() {
    let server = images_server(1).await;
    let ledger = ledger(5_000);
    let scope = ModelCallScope::new().with_spend_ledger(ledger.clone(), "studio");
    let refused = scope
        .within(images_client(&server).generate("a red barn", ImageOptions::default()))
        .await
        .unwrap_err();
    assert_eq!(refused.code, adk_core::SPEND_LIMIT_EXCEEDED_CODE);
    assert!(server.received_requests().await.unwrap().is_empty());
    assert_eq!(muse_spent(&ledger).await, 0);
}

#[tokio::test]
async fn an_image_call_commits_the_images_returned() {
    let server = images_server(2).await;
    let ledger = ledger(1_000_000);
    let tracker = Arc::new(BudgetTracker::new(RunBudget::new().max_cost_usd(1.0)));
    let scope = ModelCallScope::new()
        .with_budget(tracker.clone())
        .with_spend_ledger(ledger.clone(), "studio");
    let options = ImageOptions { n: Some(4), ..ImageOptions::default() };
    let response =
        scope.within(images_client(&server).generate("a red barn", options)).await.unwrap();
    assert_eq!(response.data.len(), 2);
    // Four were requested and held; the two returned are billed.
    assert_eq!(muse_spent(&ledger).await, 20_000);
    assert_eq!(tracker.usage().cost_micro_usd, 20_000);
    assert_eq!(tracker.units(), UsageVector::new().with(UsageUnit::Images, 2));
}

#[tokio::test]
async fn a_rejected_image_call_costs_nothing() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/images/generations"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({ "error": "bad size" })))
        .mount(&server)
        .await;
    let ledger = ledger(1_000_000);
    let scope = ModelCallScope::new().with_spend_ledger(ledger.clone(), "studio");
    let error = scope
        .within(images_client(&server).generate("a red barn", ImageOptions::default()))
        .await
        .unwrap_err();
    assert_eq!(error.code, "model.muse.api_error");
    assert_eq!(muse_spent(&ledger).await, 0);
}

#[tokio::test]
async fn an_image_call_outside_a_run_is_unmetered() {
    let server = images_server(1).await;
    let response =
        images_client(&server).generate("a red barn", ImageOptions::default()).await.unwrap();
    assert_eq!(response.data.len(), 1);
}

#[tokio::test]
async fn an_unpriced_image_model_is_refused_under_a_cost_cap() {
    let server = images_server(1).await;
    let tracker = Arc::new(BudgetTracker::new(RunBudget::new().max_cost_usd(1.0)));
    let scope = ModelCallScope::new().with_budget(tracker);
    let client = MuseImagesClient::new(
        MuseConfig::new("test-key", "muse-image-9.0").with_base_url(server.uri()),
    )
    .unwrap();
    let refused =
        scope.within(client.generate("a red barn", ImageOptions::default())).await.unwrap_err();
    assert_eq!(refused.code, "budget.cost_unknown");
    assert!(server.received_requests().await.unwrap().is_empty());
}

fn wav_seconds(seconds: usize) -> Vec<u8> {
    let data = seconds * 16_000 * 2;
    let mut wav = Vec::with_capacity(44 + data);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&((36 + data) as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&16_000u32.to_le_bytes());
    wav.extend_from_slice(&32_000u32.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(data as u32).to_le_bytes());
    wav.resize(44 + data, 0);
    wav
}

async fn asr_server(duration_ms: u64) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/asr/transcribe"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "sessionId": "s1",
            "transcript": "hello",
            "audioDurationMs": duration_ms,
        })))
        .mount(&server)
        .await;
    server
}

fn asr_client(server: &MockServer) -> MuseAsrClient {
    MuseAsrClient::new(
        MuseConfig::new("test-key", "muse-voice-transcribe-1.0").with_base_url(server.uri()),
    )
    .unwrap()
}

#[tokio::test]
async fn a_transcription_commits_whole_seconds_at_the_hourly_rate() {
    let server = asr_server(90_400).await;
    let ledger = ledger(1_000_000);
    let tracker = Arc::new(BudgetTracker::new(RunBudget::new()));
    let scope = ModelCallScope::new()
        .with_budget(tracker.clone())
        .with_spend_ledger(ledger.clone(), "studio");
    let transcript = scope
        .within(asr_client(&server).transcribe(
            wav_seconds(91),
            "memo.wav",
            "audio/wav",
            Default::default(),
        ))
        .await
        .unwrap();
    assert_eq!(transcript.text, "hello");
    // 90.4 s bills as 90 s at $0.18 per hour.
    assert_eq!(muse_spent(&ledger).await, 4_500);
    assert_eq!(tracker.units(), UsageVector::new().with(UsageUnit::AudioMillis, 90_400));
}

#[tokio::test]
async fn a_transcription_longer_than_the_cap_allows_is_refused_before_it_is_sent() {
    let server = asr_server(90_000).await;
    // 90 s costs 4,500 micro-USD; the cap leaves 4,000.
    let ledger = ledger(4_000);
    let scope = ModelCallScope::new().with_spend_ledger(ledger.clone(), "studio");
    let refused = scope
        .clone()
        .within(asr_client(&server).transcribe(
            wav_seconds(90),
            "memo.wav",
            "audio/wav",
            Default::default(),
        ))
        .await
        .unwrap_err();
    assert_eq!(refused.code, adk_core::SPEND_LIMIT_EXCEEDED_CODE);
    assert!(server.received_requests().await.unwrap().is_empty());

    // A short memo fits.
    let short = asr_server(10_000).await;
    scope
        .within(asr_client(&short).transcribe(
            wav_seconds(10),
            "memo.wav",
            "audio/wav",
            Default::default(),
        ))
        .await
        .unwrap();
    assert_eq!(muse_spent(&ledger).await, 500);
}
