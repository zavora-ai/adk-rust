//! Records model spend against [`RunConfig::spend_ledger`](adk_core::RunConfig::spend_ledger).
//!
//! When a run's config carries a ledger, the runner installs an [`LlmSpendRecorder`] as the
//! first [`InvocationHooks`] entry. Before each model call it reserves an estimate; once the
//! call's final chunk arrives it commits the reported `UsageMetadata::cost`, attributed to the
//! provider the response names (`LlmResponse::provider`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use adk_core::{
    BeforeModelResult, CallbackContext, Content, InvocationHooks, LlmRequest, LlmResponse,
    ReservationId, Result, SpendKey, SpendLedger, async_trait, usd_to_micro_usd,
};
use futures::{Stream, StreamExt};

/// How much the runner reserves before each model call.
///
/// The reservation is a hold, not a charge: the call commits its reported cost once it
/// completes. When the request caps output tokens and an output price is set, the hold is
/// `max_output_tokens × price`; otherwise it is `per_call_micro_usd`.
///
/// # Example
///
/// ```rust
/// use adk_core::LlmRequest;
/// use adk_runner::LlmSpendEstimate;
///
/// let estimate = LlmSpendEstimate::per_call(20_000).with_output_price(10_000_000);
/// let mut request = LlmRequest::new("gemini-3.7-flash", vec![]);
/// assert_eq!(estimate.estimate(&request), 20_000);
///
/// request.config = Some(adk_core::GenerateContentConfig {
///     max_output_tokens: Some(4_096),
///     ..Default::default()
/// });
/// assert_eq!(estimate.estimate(&request), 40_960);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LlmSpendEstimate {
    /// Reserved when the request has no output cap or no output price is set.
    pub per_call_micro_usd: u64,
    /// Output price in micro-USD per million tokens, applied to `max_output_tokens`.
    pub output_micro_usd_per_million_tokens: Option<u64>,
}

impl LlmSpendEstimate {
    /// Default per-call hold: 0.05 USD.
    pub const DEFAULT_PER_CALL_MICRO_USD: u64 = 50_000;

    /// Reserves a fixed amount before every call.
    pub fn per_call(micro_usd: u64) -> Self {
        Self { per_call_micro_usd: micro_usd, output_micro_usd_per_million_tokens: None }
    }

    /// Sizes the hold from the request's `max_output_tokens` at this output price.
    pub fn with_output_price(mut self, micro_usd_per_million_tokens: u64) -> Self {
        self.output_micro_usd_per_million_tokens = Some(micro_usd_per_million_tokens);
        self
    }

    /// Returns the amount to reserve for `request`.
    pub fn estimate(&self, request: &LlmRequest) -> u64 {
        let max_output_tokens = request
            .config
            .as_ref()
            .and_then(|config| config.max_output_tokens)
            .and_then(|tokens| u64::try_from(tokens).ok());
        match (max_output_tokens, self.output_micro_usd_per_million_tokens) {
            (Some(tokens), Some(price)) => tokens.saturating_mul(price).div_ceil(1_000_000),
            _ => self.per_call_micro_usd,
        }
    }
}

impl Default for LlmSpendEstimate {
    fn default() -> Self {
        Self::per_call(Self::DEFAULT_PER_CALL_MICRO_USD)
    }
}

/// Vendor recorded for a model id, read from its family prefix.
///
/// The vendor must be known before the call so a vendor cap can refuse it. Ids routed
/// through an aggregator name the model's maker, not the aggregator.
fn vendor_for_model(model: &str) -> &'static str {
    let name = model.rsplit('/').next().unwrap_or(model).to_ascii_lowercase();
    let starts = |prefixes: &[&str]| prefixes.iter().any(|prefix| name.starts_with(prefix));
    if starts(&["gemini"]) {
        "gemini"
    } else if starts(&["claude"]) {
        "anthropic"
    } else if starts(&["gpt-", "chatgpt", "o1", "o3", "o4", "codex"]) {
        "openai"
    } else if starts(&["deepseek"]) {
        "deepseek"
    } else if starts(&["grok"]) {
        "xai"
    } else if starts(&["mistral", "magistral", "codestral", "devstral", "ministral", "pixtral"]) {
        "mistral"
    } else {
        "unknown"
    }
}

/// Identifies one agent's in-flight model call within an invocation.
type CallKey = (String, String, String);

fn call_key(ctx: &dyn CallbackContext) -> CallKey {
    (ctx.invocation_id().to_string(), ctx.branch().to_string(), ctx.agent_name().to_string())
}

#[derive(Debug)]
struct PendingCall {
    reservation: ReservationId,
    estimate: u64,
    key: SpendKey,
    model: String,
    saw_chunk: bool,
    cost: Option<f64>,
    provider: Option<String>,
}

/// [`InvocationHooks`] that reserves budget before each model call and commits its cost.
///
/// Spend is attributed to `org` (the runner passes its app name), the calling agent, and
/// the vendor. The reservation names the vendor read from the request's model id, since it
/// precedes the response. When the response names a different provider
/// (`LlmResponse::provider`, for example `bedrock` serving a Claude model), the cost is
/// committed under that provider and the recorder reserves under it for the model's later
/// calls. If a limit refuses even a zero-amount reservation under the reported provider, the
/// cost is committed under the reserved vendor instead and a warning is logged; org and agent
/// attribution are unaffected. A reservation that the ledger refuses fails the model call
/// with the ledger's error, so a capped run stops before it spends.
///
/// A call is settled when its final chunk arrives, when the same agent starts its next
/// call, when the agent finishes, or when the run's event stream ends:
///
/// | What the recorder saw | Ledger action |
/// |-----------------------|---------------|
/// | A chunk reporting `usage_metadata.cost` | Commit that cost |
/// | Chunks, but no cost | Commit the estimate and log a warning |
/// | No chunk (the call failed or a later callback skipped it) | Release |
///
/// # Example
///
/// ```rust
/// use adk_core::{InMemorySpendLedger, RunConfig};
/// use adk_runner::{LlmSpendEstimate, LlmSpendRecorder};
/// use std::sync::Arc;
///
/// let ledger = Arc::new(InMemorySpendLedger::default());
/// let recorder = Arc::new(LlmSpendRecorder::new(ledger, "my-app", LlmSpendEstimate::default()));
/// let config = RunConfig::builder().invocation_hook(recorder).build();
/// # let _ = config;
/// ```
#[derive(Debug)]
pub struct LlmSpendRecorder {
    ledger: Arc<dyn SpendLedger>,
    org: String,
    estimate: LlmSpendEstimate,
    pending: Mutex<HashMap<CallKey, PendingCall>>,
    /// Provider each model's responses reported, used for its later reservations.
    providers: Mutex<HashMap<String, String>>,
}

impl LlmSpendRecorder {
    /// Creates a recorder that attributes spend to `org`.
    pub fn new(
        ledger: Arc<dyn SpendLedger>,
        org: impl Into<String>,
        estimate: LlmSpendEstimate,
    ) -> Self {
        Self {
            ledger,
            org: org.into(),
            estimate,
            pending: Mutex::new(HashMap::new()),
            providers: Mutex::new(HashMap::new()),
        }
    }

    fn vendor_for(&self, model: &str) -> String {
        self.providers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(model)
            .cloned()
            .unwrap_or_else(|| vendor_for_model(model).to_string())
    }

    fn take(&self, key: &CallKey) -> Option<PendingCall> {
        self.pending.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).remove(key)
    }

    /// Settles every call that is still open. The runner calls this when a run ends.
    pub async fn settle_all(&self) {
        let open: Vec<PendingCall> = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .drain()
            .map(|(_, call)| call)
            .collect();
        for call in open {
            self.settle(call).await;
        }
    }

    fn has_open_calls(&self) -> bool {
        !self.pending.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).is_empty()
    }

    async fn settle(&self, call: PendingCall) {
        let PendingCall { reservation, estimate, key, model, saw_chunk, cost, provider } = call;
        if !saw_chunk {
            if let Err(error) = self.ledger.release(reservation).await {
                tracing::warn!(error = %error, spend.key = %key, "failed to release model spend reservation");
            }
            return;
        }
        let amount = match cost.and_then(usd_to_micro_usd) {
            Some(amount) => amount,
            None => {
                tracing::warn!(
                    spend.key = %key,
                    spend.estimate_micro_usd = estimate,
                    "model response reported no cost; committing the reserved estimate"
                );
                estimate
            }
        };
        let reported = provider.filter(|provider| key.vendor.as_deref() != Some(provider));
        if let Some(provider) = reported {
            self.providers
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(model, provider.clone());
            let corrected = key.clone().with_vendor(provider);
            // A zero hold places the committed cost under the reported provider; the
            // estimate's hold is released only once the cost is recorded there.
            match self.ledger.reserve(&corrected, 0).await {
                Ok(corrected_reservation) => {
                    match self.ledger.commit(corrected_reservation, amount).await {
                        Ok(()) => {
                            if let Err(error) = self.ledger.release(reservation).await {
                                tracing::warn!(error = %error, spend.key = %key, "failed to release model spend reservation");
                            }
                            return;
                        }
                        Err(error) => tracing::warn!(
                            error = %error,
                            spend.key = %corrected,
                            "failed to commit model spend under the reported provider; committing under the reserved vendor"
                        ),
                    }
                }
                Err(error) => tracing::warn!(
                    error = %error,
                    spend.key = %corrected,
                    "spend ledger refused the reported provider; committing under the reserved vendor"
                ),
            }
        }
        if let Err(error) = self.ledger.commit(reservation, amount).await {
            tracing::error!(
                error = %error,
                spend.key = %key,
                spend.amount_micro_usd = amount,
                "failed to commit model spend"
            );
        }
    }
}

#[async_trait]
impl InvocationHooks for LlmSpendRecorder {
    async fn before_model(
        &self,
        ctx: Arc<dyn CallbackContext>,
        request: LlmRequest,
    ) -> Result<BeforeModelResult> {
        let call = call_key(ctx.as_ref());
        if let Some(previous) = self.take(&call) {
            self.settle(previous).await;
        }
        let key = SpendKey::org(&self.org)
            .with_agent(ctx.agent_name())
            .with_vendor(self.vendor_for(&request.model));
        let estimate = self.estimate.estimate(&request);
        let reservation = self.ledger.reserve(&key, estimate).await.inspect_err(|error| {
            tracing::warn!(error = %error, spend.key = %key, "model call refused by spend ledger");
        })?;
        self.pending.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).insert(
            call,
            PendingCall {
                reservation,
                estimate,
                key,
                model: request.model.clone(),
                saw_chunk: false,
                cost: None,
                provider: None,
            },
        );
        Ok(BeforeModelResult::Continue(request))
    }

    async fn after_model(
        &self,
        ctx: Arc<dyn CallbackContext>,
        response: LlmResponse,
    ) -> Result<Option<LlmResponse>> {
        let call = call_key(ctx.as_ref());
        let finished = {
            let mut pending = self.pending.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(open) = pending.get_mut(&call) {
                open.saw_chunk = true;
                if let Some(cost) = response.usage_metadata.as_ref().and_then(|usage| usage.cost) {
                    open.cost = Some(cost);
                }
                if let Some(provider) = &response.provider {
                    open.provider = Some(provider.clone());
                }
            }
            if response.turn_complete { pending.remove(&call) } else { None }
        };
        if let Some(call) = finished {
            self.settle(call).await;
        }
        Ok(None)
    }

    async fn after_agent(&self, ctx: Arc<dyn CallbackContext>) -> Result<Option<Content>> {
        if let Some(call) = self.take(&call_key(ctx.as_ref())) {
            self.settle(call).await;
        }
        Ok(None)
    }
}

/// Settles any open call when a run's stream is dropped before it finishes.
struct SettleOnDrop(Arc<LlmSpendRecorder>);

impl Drop for SettleOnDrop {
    fn drop(&mut self) {
        if !self.0.has_open_calls() {
            return;
        }
        let recorder = self.0.clone();
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move { recorder.settle_all().await });
            }
            Err(_) => tracing::warn!(
                "run dropped outside a tokio runtime; open model spend reservations expire by TTL"
            ),
        }
    }
}

/// Forwards `events` and settles the recorder's open calls when the stream ends or is
/// dropped.
pub(crate) fn settle_when_done<S, T>(
    events: S,
    recorder: Arc<LlmSpendRecorder>,
) -> impl Stream<Item = T> + Send
where
    S: Stream<Item = T> + Send + 'static,
    T: Send + 'static,
{
    async_stream::stream! {
        let guard = SettleOnDrop(recorder);
        let mut events = std::pin::pin!(events);
        while let Some(event) = events.next().await {
            yield event;
        }
        guard.0.settle_all().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendor_is_read_from_the_model_family() {
        assert_eq!(vendor_for_model("gemini-3.7-flash"), "gemini");
        assert_eq!(vendor_for_model("models/gemini-3.1-pro-preview"), "gemini");
        assert_eq!(vendor_for_model("claude-opus-4-8"), "anthropic");
        assert_eq!(vendor_for_model("gpt-5.2"), "openai");
        assert_eq!(vendor_for_model("o3-mini"), "openai");
        assert_eq!(vendor_for_model("deepseek-v4"), "deepseek");
        assert_eq!(vendor_for_model("llama-4-scout"), "unknown");
    }

    #[test]
    fn the_estimate_without_an_output_price_is_the_per_call_amount() {
        let mut request = LlmRequest::new("gemini-3.7-flash", vec![]);
        request.config = Some(adk_core::GenerateContentConfig {
            max_output_tokens: Some(1_000),
            ..Default::default()
        });
        assert_eq!(LlmSpendEstimate::per_call(7).estimate(&request), 7);
        assert_eq!(LlmSpendEstimate::default().estimate(&request), 50_000);
    }
}
