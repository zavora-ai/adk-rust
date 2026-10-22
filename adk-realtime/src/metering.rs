//! Realtime usage and spend.
//!
//! A provider reports what one response cost in tokens when the response ends: OpenAI in
//! `response.done` under `response.usage`, Gemini Live in the server message's
//! `usageMetadata`. [`RealtimeUsage::from_response`] reads either form from the
//! [`ResponseDone`](crate::events::ServerEvent::ResponseDone) payload,
//! [`RealtimeUsage::to_usage_metadata`] renders it as the [`UsageMetadata`] the rest of
//! ADK reports, and [`RealtimePricing`] prices it.
//!
//! The runners and `RealtimeAgent` meter every response in the run's
//! [`adk_core::SpendLedger`] the way `LlmAgent` meters a model call: a hold
//! for the run's [`LlmSpendEstimate`](adk_core::LlmSpendEstimate) is reserved when the
//! response starts, and the priced cost — or the estimate, without pricing — is committed
//! when it ends. A refused hold ends the session with
//! [`crate::error::RealtimeError::SpendRefused`].

use std::sync::Arc;

use adk_core::{
    LlmResponse, ModelCallScope, ModelSpendHold, RunConfig, SpendKey, SpendLedger, UsageMetadata,
    vendor_for_model,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{RealtimeError, Result};

/// Token usage one realtime response reported.
///
/// # Example
///
/// ```rust
/// use adk_realtime::metering::RealtimeUsage;
/// use serde_json::json;
///
/// let done = json!({ "response": { "usage": {
///     "total_tokens": 30, "input_tokens": 20, "output_tokens": 10,
///     "input_token_details": { "text_tokens": 12, "audio_tokens": 8, "cached_tokens": 0 },
///     "output_token_details": { "text_tokens": 4, "audio_tokens": 6 }
/// } } });
/// let usage = RealtimeUsage::from_response(&done).unwrap();
/// assert_eq!((usage.input_tokens, usage.output_tokens, usage.total_tokens), (20, 10, 30));
/// assert_eq!(usage.output_audio_tokens, Some(6));
///
/// let gemini = json!({ "usageMetadata": {
///     "promptTokenCount": 7, "responseTokenCount": 3, "totalTokenCount": 10,
///     "responseTokensDetails": [{ "modality": "AUDIO", "tokenCount": 3 }]
/// } });
/// let usage = RealtimeUsage::from_response(&gemini).unwrap();
/// assert_eq!((usage.input_tokens, usage.output_tokens, usage.total_tokens), (7, 3, 10));
/// assert_eq!(usage.output_audio_tokens, Some(3));
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RealtimeUsage {
    /// Tokens of the input the response was generated from.
    pub input_tokens: u32,
    /// Tokens the response generated.
    pub output_tokens: u32,
    /// Input and output together.
    pub total_tokens: u32,
    /// Input text tokens, when the provider breaks input down by modality.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_text_tokens: Option<u32>,
    /// Input audio tokens, when the provider breaks input down by modality.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_audio_tokens: Option<u32>,
    /// Input tokens served from the provider's cache.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_input_tokens: Option<u32>,
    /// Output text tokens, when the provider breaks output down by modality.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_text_tokens: Option<u32>,
    /// Output audio tokens, when the provider breaks output down by modality.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_audio_tokens: Option<u32>,
}

fn count(value: Option<&Value>) -> Option<u32> {
    value.and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok())
}

impl RealtimeUsage {
    /// Reads the usage a `ResponseDone` payload carries: OpenAI's `response.usage` (or a
    /// bare `usage`), or Gemini's `usageMetadata`. `None` when the payload reports none.
    pub fn from_response(response: &Value) -> Option<Self> {
        if let Some(usage) =
            response.get("response").and_then(|r| r.get("usage")).or_else(|| response.get("usage"))
        {
            return Self::from_openai(usage);
        }
        response.get("usageMetadata").and_then(Self::from_gemini)
    }

    fn from_openai(usage: &Value) -> Option<Self> {
        let input_tokens = count(usage.get("input_tokens"));
        let output_tokens = count(usage.get("output_tokens"));
        let total_tokens = count(usage.get("total_tokens"));
        if input_tokens.is_none() && output_tokens.is_none() && total_tokens.is_none() {
            return None;
        }
        let input = usage.get("input_token_details");
        let output = usage.get("output_token_details");
        let input_tokens = input_tokens.unwrap_or(0);
        let output_tokens = output_tokens.unwrap_or(0);
        Some(Self {
            input_tokens,
            output_tokens,
            total_tokens: total_tokens.unwrap_or(input_tokens + output_tokens),
            input_text_tokens: count(input.and_then(|d| d.get("text_tokens"))),
            input_audio_tokens: count(input.and_then(|d| d.get("audio_tokens"))),
            cached_input_tokens: count(input.and_then(|d| d.get("cached_tokens"))),
            output_text_tokens: count(output.and_then(|d| d.get("text_tokens"))),
            output_audio_tokens: count(output.and_then(|d| d.get("audio_tokens"))),
        })
    }

    fn from_gemini(usage: &Value) -> Option<Self> {
        let input_tokens = count(usage.get("promptTokenCount"));
        let output_tokens = count(usage.get("responseTokenCount"))
            .or_else(|| count(usage.get("candidatesTokenCount")));
        let total_tokens = count(usage.get("totalTokenCount"));
        if input_tokens.is_none() && output_tokens.is_none() && total_tokens.is_none() {
            return None;
        }
        let modality = |details: Option<&Value>, modality: &str| -> Option<u32> {
            details.and_then(Value::as_array).and_then(|entries| {
                entries
                    .iter()
                    .find(|entry| entry.get("modality").and_then(Value::as_str) == Some(modality))
                    .and_then(|entry| count(entry.get("tokenCount")))
            })
        };
        let input = usage.get("promptTokensDetails");
        let output = usage.get("responseTokensDetails");
        let input_tokens = input_tokens.unwrap_or(0);
        let output_tokens = output_tokens.unwrap_or(0);
        Some(Self {
            input_tokens,
            output_tokens,
            total_tokens: total_tokens.unwrap_or(input_tokens + output_tokens),
            input_text_tokens: modality(input, "TEXT"),
            input_audio_tokens: modality(input, "AUDIO"),
            cached_input_tokens: count(usage.get("cachedContentTokenCount")),
            output_text_tokens: modality(output, "TEXT"),
            output_audio_tokens: modality(output, "AUDIO"),
        })
    }

    /// The usage as the [`UsageMetadata`] ADK events carry, priced at `cost_usd` when known.
    pub fn to_usage_metadata(&self, cost_usd: Option<f64>) -> UsageMetadata {
        let signed = |n: u32| i32::try_from(n).unwrap_or(i32::MAX);
        UsageMetadata {
            prompt_token_count: signed(self.input_tokens),
            candidates_token_count: signed(self.output_tokens),
            total_token_count: signed(self.total_tokens),
            cache_read_input_token_count: self.cached_input_tokens.map(signed),
            audio_input_token_count: self.input_audio_tokens.map(signed),
            audio_output_token_count: self.output_audio_tokens.map(signed),
            cost: cost_usd,
            provider_usage: serde_json::to_value(self).ok(),
            ..UsageMetadata::default()
        }
    }
}

/// Prices for realtime tokens, in micro-USD per million tokens of each kind.
///
/// Without a modality breakdown, input tokens are priced as text input and output tokens
/// as text output.
///
/// # Example
///
/// ```rust
/// use adk_realtime::metering::{RealtimePricing, RealtimeUsage};
///
/// let pricing = RealtimePricing {
///     input_text_micro_usd_per_million: 4_000_000,
///     output_text_micro_usd_per_million: 16_000_000,
///     ..RealtimePricing::default()
/// };
/// let usage = RealtimeUsage { input_tokens: 1_000_000, output_tokens: 500_000, ..Default::default() };
/// assert_eq!(pricing.cost_micro_usd(&usage), 12_000_000);
/// assert_eq!(pricing.cost_usd(&usage), 12.0);
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RealtimePricing {
    /// Price of input text tokens.
    pub input_text_micro_usd_per_million: u64,
    /// Price of input audio tokens.
    pub input_audio_micro_usd_per_million: u64,
    /// Price of cached input tokens.
    pub cached_input_micro_usd_per_million: u64,
    /// Price of output text tokens.
    pub output_text_micro_usd_per_million: u64,
    /// Price of output audio tokens.
    pub output_audio_micro_usd_per_million: u64,
}

impl RealtimePricing {
    /// The cost of `usage` in micro-USD, rounded up per token kind.
    pub fn cost_micro_usd(&self, usage: &RealtimeUsage) -> u64 {
        let priced =
            |tokens: u32, price: u64| u64::from(tokens).saturating_mul(price).div_ceil(1_000_000);
        let cached = usage.cached_input_tokens.unwrap_or(0);
        let (input_text, input_audio) = match (usage.input_text_tokens, usage.input_audio_tokens) {
            (None, None) => (usage.input_tokens.saturating_sub(cached), 0),
            (text, audio) => (text.unwrap_or(0).saturating_sub(cached), audio.unwrap_or(0)),
        };
        let (output_text, output_audio) =
            match (usage.output_text_tokens, usage.output_audio_tokens) {
                (None, None) => (usage.output_tokens, 0),
                (text, audio) => (text.unwrap_or(0), audio.unwrap_or(0)),
            };
        priced(input_text, self.input_text_micro_usd_per_million)
            .saturating_add(priced(input_audio, self.input_audio_micro_usd_per_million))
            .saturating_add(priced(cached, self.cached_input_micro_usd_per_million))
            .saturating_add(priced(output_text, self.output_text_micro_usd_per_million))
            .saturating_add(priced(output_audio, self.output_audio_micro_usd_per_million))
    }

    /// The cost of `usage` in USD.
    pub fn cost_usd(&self, usage: &RealtimeUsage) -> f64 {
        self.cost_micro_usd(usage) as f64 / 1_000_000.0
    }
}

/// Meters the responses of one session in the run's spend ledger.
pub(crate) struct ResponseMeter {
    ledger: Arc<dyn SpendLedger>,
    key: SpendKey,
    estimate_micro_usd: u64,
    pricing: Option<RealtimePricing>,
    provider: String,
    hold: tokio::sync::Mutex<Option<ModelSpendHold>>,
}

impl std::fmt::Debug for ResponseMeter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResponseMeter")
            .field("key", &self.key)
            .field("estimate_micro_usd", &self.estimate_micro_usd)
            .field("pricing", &self.pricing)
            .finish_non_exhaustive()
    }
}

impl ResponseMeter {
    /// A meter for responses of `model_id` made on behalf of the run `config` describes,
    /// attributed to `org` and the agent named `agent_name`. `None` when the run has no
    /// spend ledger.
    pub(crate) fn for_run(
        config: &RunConfig,
        org: &str,
        agent_name: &str,
        model_id: &str,
        pricing: Option<RealtimePricing>,
    ) -> Option<Self> {
        let ledger = config.spend_ledger.clone()?;
        let key = ModelCallScope::for_run(config, org, agent_name).spend_key(model_id)?;
        Some(Self {
            ledger,
            key,
            estimate_micro_usd: config.llm_spend_estimate.unwrap_or_default().per_call_micro_usd,
            pricing,
            provider: vendor_for_model(model_id).to_string(),
            hold: tokio::sync::Mutex::new(None),
        })
    }

    /// Reserves the hold for a response that is starting. A hold already open — a second
    /// start before the first end, as an interruption produces — is kept.
    ///
    /// # Errors
    ///
    /// [`RealtimeError::SpendRefused`] when the ledger refuses the reservation.
    pub(crate) async fn begin_response(&self) -> Result<()> {
        let mut slot = self.hold.lock().await;
        if slot.is_some() {
            return Ok(());
        }
        *slot = Some(self.reserve().await?);
        Ok(())
    }

    /// Settles the open hold with what `response` reports, opening one first when the
    /// provider sent no start event (Gemini Live). Returns the usage the response reported.
    ///
    /// # Errors
    ///
    /// [`RealtimeError::SpendRefused`] when a hold had to be opened here and the ledger
    /// refused it: the response already happened, so the caller ends the session.
    pub(crate) async fn end_response(&self, response: &Value) -> Result<Option<UsageMetadata>> {
        let usage = RealtimeUsage::from_response(response);
        let mut hold = match self.hold.lock().await.take() {
            Some(hold) => hold,
            None => self.reserve().await?,
        };
        let metadata = usage.map(|usage| {
            usage.to_usage_metadata(self.pricing.map(|pricing| pricing.cost_usd(&usage)))
        });
        let observed = LlmResponse {
            usage_metadata: metadata.clone(),
            provider: Some(self.provider.clone()),
            ..LlmResponse::default()
        };
        hold.observe(&observed);
        hold.settle().await;
        Ok(metadata)
    }

    /// Releases a hold left open when the session ends without a done event.
    pub(crate) async fn abandon(&self) {
        if let Some(hold) = self.hold.lock().await.take() {
            hold.settle().await;
        }
    }

    async fn reserve(&self) -> Result<ModelSpendHold> {
        ModelSpendHold::reserve(self.ledger.clone(), self.key.clone(), self.estimate_micro_usd)
            .await
            .map_err(|error| RealtimeError::SpendRefused(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::{InMemorySpendLedger, SpendLimits, SpendPeriod};
    use serde_json::json;

    fn openai_done() -> Value {
        json!({ "response": { "usage": {
            "total_tokens": 30, "input_tokens": 20, "output_tokens": 10,
            "input_token_details": { "text_tokens": 12, "audio_tokens": 8, "cached_tokens": 2 },
            "output_token_details": { "text_tokens": 4, "audio_tokens": 6 }
        } } })
    }

    #[test]
    fn a_payload_without_usage_reports_none() {
        assert_eq!(RealtimeUsage::from_response(&json!({ "response": { "id": "r" } })), None);
        assert_eq!(RealtimeUsage::from_response(&json!({ "serverContent": {} })), None);
    }

    #[test]
    fn usage_metadata_carries_the_counts_and_the_breakdown() {
        let usage = RealtimeUsage::from_response(&openai_done()).unwrap();
        let metadata = usage.to_usage_metadata(Some(0.5));
        assert_eq!(metadata.prompt_token_count, 20);
        assert_eq!(metadata.candidates_token_count, 10);
        assert_eq!(metadata.total_token_count, 30);
        assert_eq!(metadata.cache_read_input_token_count, Some(2));
        assert_eq!(metadata.audio_input_token_count, Some(8));
        assert_eq!(metadata.audio_output_token_count, Some(6));
        assert_eq!(metadata.cost, Some(0.5));
    }

    #[test]
    fn pricing_uses_the_breakdown_when_present() {
        let usage = RealtimeUsage::from_response(&openai_done()).unwrap();
        let pricing = RealtimePricing {
            input_text_micro_usd_per_million: 1_000_000,
            input_audio_micro_usd_per_million: 2_000_000,
            cached_input_micro_usd_per_million: 500_000,
            output_text_micro_usd_per_million: 3_000_000,
            output_audio_micro_usd_per_million: 4_000_000,
        };
        // 10 text (12 minus 2 cached) + 8 audio + 2 cached + 4 text + 6 audio.
        assert_eq!(pricing.cost_micro_usd(&usage), 10 + 16 + 1 + 12 + 24);
    }

    #[tokio::test]
    async fn a_response_is_committed_under_the_run_key() {
        let ledger = Arc::new(InMemorySpendLedger::new(SpendLimits::new()));
        let config = RunConfig::builder().spend_ledger(ledger.clone()).build();
        let pricing = RealtimePricing {
            input_text_micro_usd_per_million: 1_000_000,
            output_text_micro_usd_per_million: 1_000_000,
            ..RealtimePricing::default()
        };
        let meter =
            ResponseMeter::for_run(&config, "voice", "agent", "gpt-realtime", Some(pricing))
                .unwrap();
        meter.begin_response().await.unwrap();
        let usage = meter
            .end_response(
                &json!({ "usage": { "input_tokens": 1_000_000, "output_tokens": 2_000_000 } }),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(usage.cost, Some(3.0));
        let key = SpendKey::org("voice").with_vendor("openai");
        assert_eq!(ledger.spent(&key, SpendPeriod::Day).await.unwrap(), 3_000_000);
    }

    #[tokio::test]
    async fn without_pricing_the_estimate_is_committed_and_a_cap_refuses() {
        let limits = SpendLimits::new().limit(SpendKey::org("voice"), 60_000);
        let ledger = Arc::new(InMemorySpendLedger::new(limits));
        let config = RunConfig::builder().spend_ledger(ledger.clone()).build();
        let meter = ResponseMeter::for_run(&config, "voice", "agent", "gemini-live", None).unwrap();
        // No start event: the hold opens and settles at the end, committing the estimate.
        meter.end_response(&json!({ "usageMetadata": { "totalTokenCount": 5 } })).await.unwrap();
        let key = SpendKey::org("voice").with_vendor("gemini");
        assert_eq!(ledger.spent(&key, SpendPeriod::Day).await.unwrap(), 50_000);
        let refused = meter.begin_response().await.unwrap_err();
        assert!(matches!(refused, RealtimeError::SpendRefused(_)), "{refused}");
    }
}
