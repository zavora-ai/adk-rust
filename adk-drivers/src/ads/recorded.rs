//! Recorded advertising responses (spec 3.5 §7.13: "no live spend in tests").

use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{AdsDriver, Campaign, CampaignDraft, CampaignPerformance};
use crate::error::{DriverError, Result};

/// One call to an advertising account and what it answered (spec 3.5 §7.13).
///
/// `request` is `{"op": ..., "args": ...}`; `response` is `{"ok": <value>}` or
/// `{"error": {"code": "drivers.*", "message": ...}}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct AdsInteraction {
    /// The call.
    pub request: Value,
    /// Its answer.
    pub response: Value,
}

impl AdsInteraction {
    /// The interaction of `op` with `args` answering `ok`.
    pub fn ok(op: &str, args: Value, ok: Value) -> Self {
        Self { request: request(op, args), response: json!({ "ok": ok }) }
    }
}

fn request(op: &str, args: Value) -> Value {
    json!({ "op": op, "args": args })
}

/// The error a recorded `{"code", "message"}` stands for.
fn recorded_error(error: &Value) -> DriverError {
    let message =
        error.get("message").and_then(Value::as_str).unwrap_or("recorded error").to_string();
    match error.get("code").and_then(Value::as_str).unwrap_or_default() {
        "drivers.invalid" => DriverError::Invalid { reason: message },
        "drivers.refused" => DriverError::Refused { reason: message },
        "drivers.not_found" => DriverError::NotFound { what: message },
        "drivers.unauthorized" => {
            DriverError::Unauthorized { service: "ads".into(), reason: message }
        }
        _ => DriverError::unavailable("ads", message),
    }
}

/// An advertising account replayed from recorded responses (spec 3.5 §7.13): each call is
/// answered by the first unused interaction whose request equals it, so tests run the
/// advertising contract against a provider's real answers without spending anything.
///
/// # Example
///
/// ```rust
/// use adk_drivers::ads::{AdsDriver, AdsInteraction, RecordedAds};
/// use serde_json::json;
///
/// # futures::executor::block_on(async {
/// let ads = RecordedAds::new(vec![AdsInteraction::ok(
///     "pause_campaign",
///     json!({"campaign": "c1"}),
///     json!({"id": "c1", "name": "Bread", "state": "paused", "dailyBudgetMinor": 2000, "bidMinor": 50, "currency": "USD"}),
/// )]);
/// assert_eq!(ads.pause_campaign("c1").await.unwrap().id, "c1");
/// assert!(ads.pause_campaign("c1").await.is_err());
/// # });
/// ```
#[derive(Debug, Default)]
pub struct RecordedAds {
    interactions: Mutex<Vec<(AdsInteraction, bool)>>,
}

impl RecordedAds {
    /// Replays `interactions`.
    pub fn new(interactions: Vec<AdsInteraction>) -> Self {
        Self {
            interactions: Mutex::new(
                interactions.into_iter().map(|interaction| (interaction, false)).collect(),
            ),
        }
    }

    /// Replays the JSON list of interactions `text`, as [`RecordingAds::to_json`] writes it.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError::Invalid`] for text that is not such a list.
    pub fn from_json(text: &str) -> Result<Self> {
        let interactions: Vec<AdsInteraction> = serde_json::from_str(text).map_err(|error| {
            DriverError::invalid(format!("not a list of recorded interactions: {error}"))
        })?;
        Ok(Self::new(interactions))
    }

    /// The interactions no call used yet.
    pub fn unused(&self) -> Vec<AdsInteraction> {
        self.interactions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|(_, used)| !used)
            .map(|(interaction, _)| interaction.clone())
            .collect()
    }

    fn answer<T: DeserializeOwned>(&self, op: &str, args: Value) -> Result<T> {
        let wanted = request(op, args);
        let mut interactions = self.interactions.lock().unwrap_or_else(PoisonError::into_inner);
        let Some((interaction, used)) = interactions
            .iter_mut()
            .find(|(interaction, used)| !*used && interaction.request == wanted)
        else {
            return Err(DriverError::NoRecording { request: wanted.to_string() });
        };
        *used = true;
        if let Some(error) = interaction.response.get("error") {
            return Err(recorded_error(error));
        }
        let ok = interaction.response.get("ok").cloned().unwrap_or(Value::Null);
        serde_json::from_value(ok)
            .map_err(|error| DriverError::protocol("ads", format!("recorded answer: {error}")))
    }
}

fn window(from: DateTime<Utc>, to: DateTime<Utc>) -> Value {
    json!({ "from": from.to_rfc3339(), "to": to.to_rfc3339() })
}

#[async_trait]
impl AdsDriver for RecordedAds {
    fn name(&self) -> &str {
        "recorded"
    }

    async fn performance(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<CampaignPerformance>> {
        if to <= from {
            return Err(DriverError::invalid("a window ends after it starts"));
        }
        self.answer("performance", window(from, to))
    }

    async fn launch_campaign(&self, draft: &CampaignDraft) -> Result<Campaign> {
        draft.validate()?;
        self.answer("launch_campaign", serde_json::to_value(draft).unwrap_or_default())
    }

    async fn set_bid(&self, campaign: &str, bid_minor: u64, currency: &str) -> Result<Campaign> {
        self.answer(
            "set_bid",
            json!({ "campaign": campaign, "bidMinor": bid_minor, "currency": currency }),
        )
    }

    async fn pause_campaign(&self, campaign: &str) -> Result<Campaign> {
        self.answer("pause_campaign", json!({ "campaign": campaign }))
    }
}

/// Records the calls to an advertising account and its answers, as [`RecordedAds`]
/// replays them: run it against a provider's sandbox account once, keep the JSON, and
/// tests replay it.
pub struct RecordingAds {
    inner: Arc<dyn AdsDriver>,
    recorded: Mutex<Vec<AdsInteraction>>,
}

impl std::fmt::Debug for RecordingAds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecordingAds").field("inner", &self.inner.name()).finish_non_exhaustive()
    }
}

impl RecordingAds {
    /// Records the calls to `inner`.
    pub fn new(inner: Arc<dyn AdsDriver>) -> Self {
        Self { inner, recorded: Mutex::default() }
    }

    /// The interactions so far.
    pub fn interactions(&self) -> Vec<AdsInteraction> {
        self.recorded.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// The interactions as JSON.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(&self.interactions()).unwrap_or_default()
    }

    fn record<T: Serialize>(&self, op: &str, args: Value, result: &Result<T>) {
        let response = match result {
            Ok(value) => json!({ "ok": value }),
            Err(error) => {
                json!({ "error": { "code": error.code(), "message": error.to_string() } })
            }
        };
        self.recorded
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(AdsInteraction { request: request(op, args), response });
    }
}

#[async_trait]
impl AdsDriver for RecordingAds {
    fn name(&self) -> &str {
        self.inner.name()
    }

    async fn performance(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<CampaignPerformance>> {
        let result = self.inner.performance(from, to).await;
        self.record("performance", window(from, to), &result);
        result
    }

    async fn launch_campaign(&self, draft: &CampaignDraft) -> Result<Campaign> {
        let result = self.inner.launch_campaign(draft).await;
        self.record("launch_campaign", serde_json::to_value(draft).unwrap_or_default(), &result);
        result
    }

    async fn set_bid(&self, campaign: &str, bid_minor: u64, currency: &str) -> Result<Campaign> {
        let result = self.inner.set_bid(campaign, bid_minor, currency).await;
        self.record(
            "set_bid",
            json!({ "campaign": campaign, "bidMinor": bid_minor, "currency": currency }),
            &result,
        );
        result
    }

    async fn pause_campaign(&self, campaign: &str) -> Result<Campaign> {
        let result = self.inner.pause_campaign(campaign).await;
        self.record("pause_campaign", json!({ "campaign": campaign }), &result);
        result
    }
}
