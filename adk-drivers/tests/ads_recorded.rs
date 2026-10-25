//! The advertising contract against recorded responses (spec 3.5 §7.13, §11: no live spend
//! in tests), and a recording made by `RecordingAds` replayed by `RecordedAds`.

use std::sync::Arc;

use adk_drivers::KpiSource;
use adk_drivers::ads::{
    AdsDriver, AdsMetrics, CampaignDraft, InMemoryAds, RecordedAds, RecordingAds,
};
use adk_drivers::conformance::{AdsHarness, ads_driver_contract};
use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};

fn draft() -> CampaignDraft {
    serde_json::from_value(serde_json::json!({
        "name": "Sourdough week",
        "adText": "Fresh sourdough every morning",
        "landingPage": "https://bakery.example/sourdough",
        "dailyBudgetMinor": 2000,
        "bidMinor": 60,
        "currency": "USD"
    }))
    .unwrap()
}

fn window() -> (DateTime<Utc>, DateTime<Utc>) {
    (
        Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap(),
        Utc.with_ymd_and_hms(2026, 10, 8, 0, 0, 0).unwrap(),
    )
}

/// Replays `recording`.
struct Replay(String);

#[async_trait]
impl AdsHarness for Replay {
    async fn driver(&self) -> Arc<dyn AdsDriver> {
        Arc::new(RecordedAds::from_json(&self.0).unwrap())
    }

    fn draft(&self) -> CampaignDraft {
        draft()
    }

    fn window(&self) -> (DateTime<Utc>, DateTime<Utc>) {
        window()
    }
}

#[tokio::test]
async fn recorded_responses_keep_the_ads_contract_and_spend_nothing() {
    let recording = std::fs::read_to_string(format!(
        "{}/tests/fixtures/ads_recorded.json",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    let harness = Replay(recording.clone());
    ads_driver_contract(&harness).await;

    // The recorded spend reads through the metric source; a call outside the recording is refused.
    let ads = RecordedAds::from_json(&recording).unwrap();
    let (from, to) = window();
    let spend = AdsMetrics::new(Arc::new(ads))
        .read(uuid::Uuid::nil(), "ads.spend", from, to)
        .await
        .unwrap();
    assert_eq!(spend, Some(1840.0));
    let ads = RecordedAds::from_json(&recording).unwrap();
    let error = ads.pause_campaign("cmp-other").await.unwrap_err();
    assert!(matches!(error, adk_drivers::DriverError::NoRecording { .. }), "{error:?}");
}

/// Records the suite against the in-memory account.
struct Record(Arc<RecordingAds>);

#[async_trait]
impl AdsHarness for Record {
    async fn driver(&self) -> Arc<dyn AdsDriver> {
        self.0.clone()
    }

    fn draft(&self) -> CampaignDraft {
        draft()
    }

    fn window(&self) -> (DateTime<Utc>, DateTime<Utc>) {
        window()
    }
}

#[tokio::test]
async fn a_recording_replays_through_the_same_contract() {
    let recording = Arc::new(RecordingAds::new(Arc::new(InMemoryAds::new())));
    ads_driver_contract(&Record(Arc::clone(&recording))).await;
    let json = recording.to_json();
    let replay = RecordedAds::from_json(&json).unwrap();
    let replayed = Replay(json);
    ads_driver_contract(&replayed).await;
    assert!(!replay.unused().is_empty(), "the recording holds the suite's calls");
}
