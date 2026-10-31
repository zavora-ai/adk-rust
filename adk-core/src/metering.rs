//! Metering for every model call made on behalf of a run.
//!
//! `LlmAgent` and `CodeActAgent` turns, context compaction summaries, MCP sampling
//! and LLM routing all go through a [`ModelCallScope`], which applies the run's
//! [`BudgetTracker`] and spend ledger as two gates:
//!
//! | Step | Budget ([`RunConfig::budget_tracker`]) | Spend ledger ([`RunConfig::spend_ledger`]) |
//! |------|----------------------------------------|--------------------------------------------|
//! | Before the call | [`BudgetTracker::begin_model_call`] refuses a call over the limit | [`SpendLedger::reserve`] holds the call's upper-bound cost ([`Llm::max_cost_micro_usd`]) or refuses the call |
//! | After the call | the call's usage and cost are counted once | the reported cost is committed under `LlmResponse::provider` |
//! | Call failed | — | the hold is released |
//!
//! A refused call is never sent to the provider.
//!
//! # Hard caps
//!
//! The hold is an upper bound on the call's cost, so the committed cost never exceeds it
//! and a spend cap is never overshot. A call the model cannot bound (an unpriced model)
//! reserves [`UNBOUNDED_HOLD_MICRO_USD`]: any cap that covers the call refuses it, so it
//! runs only where no cap applies, and commits its reported cost or the
//! [`LlmSpendEstimate`] when it reports none.
//!
//! # Reaching the scope
//!
//! - [`ModelCallScope::generate`] meters one call explicitly.
//! - [`ModelCallScope::within`] makes the scope current for a future; inside it,
//!   [`generate_metered`] and [`MeteredLlm`] meter against it. The runner wraps
//!   its compaction calls this way, the MCP client wraps sampling requests, and
//!   `LlmAgent`, `CodeActAgent` and the realtime runtimes wrap every tool call in the
//!   run's scope ([`ModelCallScope::within_tool`]).
//!
//! # Calls billed in units
//!
//! Image generation, transcription and speech synthesis are billed per image, per second
//! of audio or per character, not per token. [`ModelCallScope::meter`] (or
//! [`meter_usage`], against the current scope) applies the same two gates to them:
//!
//! | Step | Budget | Spend ledger |
//! |------|--------|--------------|
//! | Before the call | refuses when a limit is reached, or when the call has no known price under a cost cap | reserves [`UsageCall::max_cost_micro_usd`], or [`UNBOUNDED_HOLD_MICRO_USD`] without a price |
//! | After the call | counts its cost, tokens and [`UsageVector`] | commits [`MeteredUsage::cost_micro_usd`] |
//! | Call failed | — | the hold is released |
//!
//! Units are counted in the budget ([`BudgetTracker::units`]) and never in the ledger,
//! whose amounts are micro-USD only. A unit call is not a model call.
//!
//! # Example
//!
//! ```
//! use std::sync::Arc;
//! use adk_core::{BudgetTracker, InMemorySpendLedger, ModelCallScope, RunBudget, RunConfig};
//!
//! let config = RunConfig::builder()
//!     .spend_ledger(Arc::new(InMemorySpendLedger::default()))
//!     .build();
//! let tracker = Arc::new(BudgetTracker::new(RunBudget::new().max_model_calls(10)));
//! let scope = ModelCallScope::for_run(&config, "my-app", "assistant").with_budget(tracker);
//! assert!(!scope.is_unmetered());
//! assert_eq!(scope.spend_key("gemini-3.7-flash").unwrap().vendor.as_deref(), Some("gemini"));
//! ```

use std::fmt;
use std::future::Future;
use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;

use crate::{
    BudgetTracker, Llm, LlmRequest, LlmResponse, LlmResponseStream, ReservationId, Result,
    RunConfig, SpendKey, SpendLedger, ToolContext, UsageVector, budget::meter_stream,
    schema_adapter::SchemaAdapter, usd_to_micro_usd,
};

/// Hold reserved for a model call whose cost cannot be bounded.
///
/// It exceeds every cap, so a ledger refuses it wherever a limit covers the call, and it
/// fits the signed 64-bit storage of the SQL ledgers.
pub const UNBOUNDED_HOLD_MICRO_USD: u64 = i64::MAX as u64;

/// What an unbounded model call commits when its response reports no cost.
///
/// Calls with a cost bound ([`Llm::max_cost_micro_usd`]) reserve and, when no cost is
/// reported, commit that bound. A call without one reserves [`UNBOUNDED_HOLD_MICRO_USD`],
/// which only an uncapped key accepts, and commits this estimate when its response
/// reports no cost: `max_output_tokens × price` when the request caps output tokens and an
/// output price is set, otherwise `per_call_micro_usd`.
///
/// # Example
///
/// ```rust
/// use adk_core::{LlmRequest, LlmSpendEstimate};
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

/// Vendor a model call is reserved under, read from the model id's family prefix.
///
/// The vendor must be known before the call so a vendor cap can refuse it. Ids routed
/// through an aggregator name the model's maker, not the aggregator; the committed cost
/// moves to the provider the response reports.
///
/// # Example
///
/// ```
/// use adk_core::vendor_for_model;
///
/// assert_eq!(vendor_for_model("models/gemini-3.7-flash"), "gemini");
/// assert_eq!(vendor_for_model("grok-4.6"), "xai");
/// assert_eq!(vendor_for_model("muse-spark-1.3"), "muse");
/// assert_eq!(vendor_for_model("my-fine-tune"), "unknown");
/// ```
pub fn vendor_for_model(model: &str) -> &'static str {
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
    } else if starts(&["muse-"]) {
        "muse"
    } else {
        "unknown"
    }
}

/// A spend-ledger hold for one model call, settled from the responses it observes.
///
/// | What the hold observed | Ledger action on [`settle`](Self::settle) |
/// |------------------------|--------------------------------------------|
/// | A response reporting `usage_metadata.cost` | Commit that cost |
/// | Responses, but no cost | Commit the estimate and log a warning |
/// | No response (the call failed) | Release |
///
/// When a response names a provider other than the reserved vendor (for example `bedrock`
/// serving a Claude model), the cost is committed under that provider. If a limit refuses
/// even a zero-amount reservation under the reported provider, the cost is committed under
/// the reserved vendor instead and a warning is logged.
///
/// A hold dropped unsettled settles on the current Tokio runtime; outside a runtime it
/// expires by the ledger's reservation TTL.
///
/// # Example
///
/// ```
/// use std::sync::Arc;
/// use adk_core::{InMemorySpendLedger, LlmResponse, ModelSpendHold, SpendKey, SpendLedger, SpendPeriod, UsageMetadata};
///
/// # async fn demo() -> adk_core::Result<()> {
/// let ledger = Arc::new(InMemorySpendLedger::default());
/// let key = SpendKey::org("acme").with_vendor("gemini");
/// let mut hold = ModelSpendHold::reserve(ledger.clone(), key, 50_000).await?;
///
/// let mut response = LlmResponse::default();
/// response.usage_metadata = Some(UsageMetadata { cost: Some(0.002), ..Default::default() });
/// hold.observe(&response);
/// hold.settle().await;
/// assert_eq!(ledger.spent(&SpendKey::org("acme"), SpendPeriod::Day).await?, 2_000);
/// # Ok(())
/// # }
/// ```
pub struct ModelSpendHold {
    inner: Option<HoldState>,
}

struct HoldState {
    ledger: Arc<dyn SpendLedger>,
    reservation: ReservationId,
    key: SpendKey,
    estimate: u64,
    saw_response: bool,
    cost_micro_usd: Option<u64>,
    provider: Option<String>,
}

impl fmt::Debug for ModelSpendHold {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut debug = f.debug_struct("ModelSpendHold");
        if let Some(state) = &self.inner {
            debug
                .field("reservation", &state.reservation)
                .field("key", &state.key)
                .field("estimate", &state.estimate)
                .field("cost_micro_usd", &state.cost_micro_usd);
        }
        debug.finish_non_exhaustive()
    }
}

impl ModelSpendHold {
    /// Reserves `estimate_micro_usd` under `key`.
    ///
    /// # Errors
    ///
    /// Returns the ledger's error, such as [`SPEND_LIMIT_EXCEEDED_CODE`](crate::SPEND_LIMIT_EXCEEDED_CODE),
    /// when the reservation is refused.
    pub async fn reserve(
        ledger: Arc<dyn SpendLedger>,
        key: SpendKey,
        estimate_micro_usd: u64,
    ) -> Result<Self> {
        let reservation = ledger.reserve(&key, estimate_micro_usd).await.inspect_err(|error| {
            tracing::warn!(error = %error, spend.key = %key, "model call refused by spend ledger");
        })?;
        Ok(Self {
            inner: Some(HoldState {
                ledger,
                reservation,
                key,
                estimate: estimate_micro_usd,
                saw_response: false,
                cost_micro_usd: None,
                provider: None,
            }),
        })
    }

    /// Commits `micro_usd`, instead of the reserved amount, when no response reports a cost.
    pub fn with_unpriced_commit(mut self, micro_usd: u64) -> Self {
        if let Some(state) = self.inner.as_mut() {
            state.estimate = micro_usd;
        }
        self
    }

    /// The key the hold was reserved under.
    pub fn key(&self) -> Option<&SpendKey> {
        self.inner.as_ref().map(|state| &state.key)
    }

    /// Records the cost and provider a response reports.
    pub fn observe(&mut self, response: &LlmResponse) {
        let Some(state) = self.inner.as_mut() else {
            return;
        };
        state.saw_response = true;
        if let Some(cost) = response.usage_metadata.as_ref().and_then(|usage| usage.cost) {
            state.cost_micro_usd = usd_to_micro_usd(cost);
        }
        if let Some(provider) = &response.provider {
            state.provider = Some(provider.clone());
        }
    }

    /// Records that the call completed at `cost_micro_usd`, or at an unknown cost.
    ///
    /// For calls that report no [`LlmResponse`], such as the unit-billed calls of
    /// [`ModelCallScope::meter`].
    ///
    /// # Example
    ///
    /// ```
    /// use std::sync::Arc;
    /// use adk_core::{InMemorySpendLedger, ModelSpendHold, SpendKey, SpendLedger, SpendPeriod};
    ///
    /// # async fn demo() -> adk_core::Result<()> {
    /// let ledger = Arc::new(InMemorySpendLedger::default());
    /// let mut hold = ModelSpendHold::reserve(ledger.clone(), SpendKey::org("acme"), 20_000).await?;
    /// hold.observe_cost(Some(10_000));
    /// hold.settle().await;
    /// assert_eq!(ledger.spent(&SpendKey::org("acme"), SpendPeriod::Day).await?, 10_000);
    /// # Ok(())
    /// # }
    /// ```
    pub fn observe_cost(&mut self, cost_micro_usd: Option<u64>) {
        let Some(state) = self.inner.as_mut() else {
            return;
        };
        state.saw_response = true;
        if cost_micro_usd.is_some() {
            state.cost_micro_usd = cost_micro_usd;
        }
    }

    /// Commits the observed cost, or releases the hold when no response arrived.
    ///
    /// Returns the provider the cost was reported under when it differs from the reserved
    /// vendor, so a caller can reserve later calls of the same model under it.
    pub async fn settle(mut self) -> Option<String> {
        let state = self.inner.take()?;
        state.settle().await
    }
}

impl HoldState {
    async fn settle(self) -> Option<String> {
        let Self { ledger, reservation, key, estimate, saw_response, cost_micro_usd, provider } =
            self;
        if !saw_response {
            if let Err(error) = ledger.release(reservation).await {
                tracing::warn!(error = %error, spend.key = %key, "failed to release model spend reservation");
            }
            return None;
        }
        let amount = match cost_micro_usd {
            Some(amount) => amount,
            None => {
                tracing::warn!(
                    spend.key = %key,
                    spend.estimate_micro_usd = estimate,
                    "model response reported no cost; committing the reserved bound or estimate"
                );
                estimate
            }
        };
        let reported = provider.filter(|provider| key.vendor.as_deref() != Some(provider));
        if let Some(provider) = &reported {
            let corrected = key.clone().with_vendor(provider.clone());
            // A zero hold places the committed cost under the reported provider; the
            // estimate's hold is released only once the cost is recorded there.
            match ledger.reserve(&corrected, 0).await {
                Ok(corrected_reservation) => {
                    match ledger.commit(corrected_reservation, amount).await {
                        Ok(()) => {
                            if let Err(error) = ledger.release(reservation).await {
                                tracing::warn!(error = %error, spend.key = %key, "failed to release model spend reservation");
                            }
                            return reported;
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
        if let Err(error) = ledger.commit(reservation, amount).await {
            tracing::error!(
                error = %error,
                spend.key = %key,
                spend.amount_micro_usd = amount,
                "failed to commit model spend"
            );
        }
        reported
    }
}

impl Drop for ModelSpendHold {
    fn drop(&mut self) {
        let Some(state) = self.inner.take() else {
            return;
        };
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    state.settle().await;
                });
            }
            Err(_) => tracing::warn!(
                spend.key = %state.key,
                "model spend hold dropped outside a tokio runtime; it expires by TTL"
            ),
        }
    }
}

/// Spend attribution for the model calls of a [`ModelCallScope`].
#[derive(Clone)]
struct SpendScope {
    ledger: Arc<dyn SpendLedger>,
    org: String,
    agent: Option<String>,
    estimate: LlmSpendEstimate,
}

/// The budget and spend ledger that model calls made on behalf of a run count against.
///
/// Built from a run's [`RunConfig`] with [`for_run`](Self::for_run), or assembled
/// explicitly for calls made outside any run. An empty scope meters nothing.
///
/// # Example
///
/// ```
/// use std::sync::Arc;
/// use adk_core::{BudgetTracker, InMemorySpendLedger, ModelCallScope, RunBudget};
///
/// let scope = ModelCallScope::new()
///     .with_budget(Arc::new(BudgetTracker::new(RunBudget::new().max_cost_usd(1.0))))
///     .with_spend_ledger(Arc::new(InMemorySpendLedger::default()), "acme")
///     .with_agent("mcp-sampling");
/// assert_eq!(scope.spend_key("claude-haiku-4-5").unwrap().agent.as_deref(), Some("mcp-sampling"));
/// ```
#[derive(Clone, Default)]
pub struct ModelCallScope {
    budget: Option<Arc<BudgetTracker>>,
    spend: Option<SpendScope>,
}

impl fmt::Debug for ModelCallScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ModelCallScope")
            .field("budget", &self.budget.as_ref().map(|tracker| tracker.budget()))
            .field("spend_org", &self.spend.as_ref().map(|spend| spend.org.as_str()))
            .field("spend_agent", &self.spend.as_ref().and_then(|spend| spend.agent.as_deref()))
            .finish()
    }
}

tokio::task_local! {
    static CURRENT_SCOPE: ModelCallScope;
}

impl ModelCallScope {
    /// Creates a scope that meters nothing.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates the scope for calls made on behalf of a run.
    ///
    /// Uses the run's [`budget_tracker`](RunConfig::budget_tracker), its
    /// [`spend_ledger`](RunConfig::spend_ledger) and
    /// [`llm_spend_estimate`](RunConfig::llm_spend_estimate). Spend is attributed to
    /// `org` (the runner passes its app name) and the [`AgentId`](crate::AgentId) the
    /// agent named `agent_name` acts as
    /// ([`ActingConfig::principal_for`](crate::ActingConfig::principal_for): the
    /// configured principal, or `urn:adk:agent:{org}/{agent_name}`).
    pub fn for_run(config: &RunConfig, org: &str, agent_name: &str) -> Self {
        Self {
            budget: config.budget_tracker.clone(),
            spend: config.spend_ledger.clone().map(|ledger| SpendScope {
                ledger,
                org: org.to_string(),
                agent: Some(config.acting.principal_for(org, agent_name).id.into()),
                estimate: config.llm_spend_estimate.unwrap_or_default(),
            }),
        }
    }

    /// Counts calls against `tracker`.
    pub fn with_budget(mut self, tracker: Arc<BudgetTracker>) -> Self {
        self.budget = Some(tracker);
        self
    }

    /// Reserves and commits call spend in `ledger`, attributed to `org`.
    pub fn with_spend_ledger(
        mut self,
        ledger: Arc<dyn SpendLedger>,
        org: impl Into<String>,
    ) -> Self {
        self.spend = Some(SpendScope {
            ledger,
            org: org.into(),
            agent: self.spend.as_ref().and_then(|spend| spend.agent.clone()),
            estimate: self.spend.as_ref().map(|spend| spend.estimate).unwrap_or_default(),
        });
        self
    }

    /// Attributes spend to `agent`. Has no effect without a spend ledger.
    pub fn with_agent(mut self, agent: impl Into<String>) -> Self {
        if let Some(spend) = self.spend.as_mut() {
            spend.agent = Some(agent.into());
        }
        self
    }

    /// Sets the hold reserved before each call. Has no effect without a spend ledger.
    pub fn with_spend_estimate(mut self, estimate: LlmSpendEstimate) -> Self {
        if let Some(spend) = self.spend.as_mut() {
            spend.estimate = estimate;
        }
        self
    }

    /// The budget tracker calls count against.
    pub fn budget(&self) -> Option<&Arc<BudgetTracker>> {
        self.budget.as_ref()
    }

    /// Returns `true` when the scope has neither a budget nor a spend ledger.
    pub fn is_unmetered(&self) -> bool {
        self.budget.is_none() && self.spend.is_none()
    }

    /// The key a call to `model` reserves under, when the scope has a spend ledger.
    pub fn spend_key(&self, model: &str) -> Option<SpendKey> {
        self.vendor_spend_key(vendor_for_model(model))
    }

    /// The key a call billed by `vendor` reserves under, when the scope has a spend ledger.
    ///
    /// # Example
    ///
    /// ```
    /// use std::sync::Arc;
    /// use adk_core::{InMemorySpendLedger, ModelCallScope};
    ///
    /// let scope = ModelCallScope::new().with_spend_ledger(Arc::new(InMemorySpendLedger::default()), "acme");
    /// assert_eq!(scope.vendor_spend_key("muse").unwrap().vendor.as_deref(), Some("muse"));
    /// assert!(ModelCallScope::new().vendor_spend_key("muse").is_none());
    /// ```
    pub fn vendor_spend_key(&self, vendor: &str) -> Option<SpendKey> {
        let spend = self.spend.as_ref()?;
        let key = SpendKey::org(&spend.org).with_vendor(vendor);
        Some(match &spend.agent {
            Some(agent) => key.with_agent(agent.clone()),
            None => key,
        })
    }

    /// The scope of the run a tool call belongs to, when the tool's runtime exposes the
    /// run's [`RunConfig`] ([`ToolContext::run_config`]).
    ///
    /// Spend is attributed to the context's app and the agent making the call, as
    /// [`for_run`](Self::for_run) attributes it.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let scope = adk_core::ModelCallScope::for_tool(ctx.as_ref());
    /// ```
    pub fn for_tool(ctx: &dyn ToolContext) -> Option<Self> {
        ctx.run_config().map(|config| Self::for_run(config, ctx.app_name(), ctx.agent_name()))
    }

    /// Runs a tool call with the scope of the run it belongs to current
    /// ([`for_tool`](Self::for_tool)), so the media and model calls the tool makes through
    /// [`meter_usage`] and [`generate_metered`] count against the run. Without a run
    /// configuration on `ctx`, `future` runs as is.
    ///
    /// Agents call this around every tool execution.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let result = ModelCallScope::within_tool(ctx.as_ref(), tool.execute(ctx.clone(), args)).await;
    /// ```
    pub fn within_tool<F: Future>(
        ctx: &dyn ToolContext,
        future: F,
    ) -> impl Future<Output = F::Output> + use<F> {
        let scope = Self::for_tool(ctx);
        async move {
            match scope {
                Some(scope) => scope.within(future).await,
                None => future.await,
            }
        }
    }

    /// Returns `true` when both scopes count against the same budget and ledger.
    pub fn same_as(&self, other: &Self) -> bool {
        let budget = match (&self.budget, &other.budget) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        let spend = match (&self.spend, &other.spend) {
            (Some(a), Some(b)) => {
                Arc::ptr_eq(&a.ledger, &b.ledger) && a.org == b.org && a.agent == b.agent
            }
            (None, None) => true,
            _ => false,
        };
        budget && spend
    }

    /// The scope made current by an enclosing [`within`](Self::within), if any.
    pub fn current() -> Option<Self> {
        CURRENT_SCOPE.try_with(Clone::clone).ok()
    }

    /// Runs `future` with this scope current, so [`generate_metered`] and [`MeteredLlm`]
    /// inside it meter against this scope.
    ///
    /// The scope does not follow work spawned onto other tasks.
    pub async fn within<F: Future>(self, future: F) -> F::Output {
        CURRENT_SCOPE.scope(self, future).await
    }

    /// Calls `model` under this scope.
    ///
    /// Reserves the call's upper-bound cost ([`Llm::max_cost_micro_usd`], or
    /// [`UNBOUNDED_HOLD_MICRO_USD`] when there is none), then the budget's model call,
    /// before the request is sent; a refusal from either stops the call. The returned
    /// stream counts the call's usage once and commits its cost at the turn-complete
    /// response, or when the stream ends or is dropped.
    ///
    /// # Errors
    ///
    /// Returns the ledger's error when the reservation is refused, a
    /// [`ResourceExhausted`](crate::ErrorCategory::ResourceExhausted) error when the
    /// budget does not allow another call, or the model's own error.
    pub async fn generate(
        &self,
        model: &dyn Llm,
        request: LlmRequest,
        stream: bool,
    ) -> Result<LlmResponseStream> {
        if self.is_unmetered() {
            return model.generate_content(request, stream).await;
        }
        let hold = match (&self.spend, self.spend_key(&request.model)) {
            (Some(spend), Some(key)) => Some(match model.max_cost_micro_usd(&request) {
                Some(bound) => ModelSpendHold::reserve(spend.ledger.clone(), key, bound).await?,
                None => {
                    ModelSpendHold::reserve(spend.ledger.clone(), key, UNBOUNDED_HOLD_MICRO_USD)
                        .await?
                        .with_unpriced_commit(spend.estimate.estimate(&request))
                }
            }),
            _ => None,
        };
        let meter = match self.budget.as_ref().map(|tracker| tracker.begin_model_call(model.name()))
        {
            Some(Ok(meter)) => Some(meter),
            Some(Err(exceeded)) => {
                // A hold that saw no response is released.
                if let Some(hold) = hold {
                    hold.settle().await;
                }
                return Err(exceeded.into());
            }
            None => None,
        };
        let responses = match model.generate_content(request, stream).await {
            Ok(responses) => responses,
            Err(error) => {
                if let Some(hold) = hold {
                    hold.settle().await;
                }
                return Err(error);
            }
        };
        let responses = match meter {
            Some(meter) => meter_stream(responses, meter),
            None => responses,
        };
        Ok(match hold {
            Some(hold) => commit_on_completion(responses, hold),
            None => responses,
        })
    }
}

impl ModelCallScope {
    /// Checks the budget and reserves the spend for one call billed in units.
    ///
    /// Reserves [`UsageCall::max_cost_micro_usd`] in the spend ledger — or
    /// [`UNBOUNDED_HOLD_MICRO_USD`] when the call has no known price, which every covering
    /// cap refuses — then checks the run budget, which refuses a call with no known price
    /// under a cost cap. Settle the returned hold with [`UsageHold::commit`] once the call
    /// completes; a hold released or dropped unsettled records nothing.
    ///
    /// # Errors
    ///
    /// Returns the ledger's error when the reservation is refused, or a
    /// [`ResourceExhausted`](crate::ErrorCategory::ResourceExhausted) error when the
    /// budget refuses the call. Nothing has been sent when either refuses.
    pub async fn begin_usage(&self, call: &UsageCall) -> Result<UsageHold> {
        let spend = match (&self.spend, self.vendor_spend_key(&call.vendor)) {
            (Some(spend), Some(key)) => Some(match call.max_cost_micro_usd {
                Some(bound) => ModelSpendHold::reserve(spend.ledger.clone(), key, bound).await?,
                None => {
                    ModelSpendHold::reserve(spend.ledger.clone(), key, UNBOUNDED_HOLD_MICRO_USD)
                        .await?
                        .with_unpriced_commit(spend.estimate.per_call_micro_usd)
                }
            }),
            _ => None,
        };
        if let Some(tracker) = &self.budget
            && let Err(exceeded) =
                tracker.begin_usage(&call.model, call.max_cost_micro_usd.is_some())
        {
            if let Some(hold) = spend {
                hold.settle().await;
            }
            return Err(exceeded.into());
        }
        Ok(UsageHold { spend, budget: self.budget.clone(), model: call.model.clone() })
    }

    /// Runs one call billed in units under this scope.
    ///
    /// `future` is the call: it returns its value with the [`MeteredUsage`] it incurred.
    /// The budget is checked and the spend reserved before `future` is polled
    /// ([`begin_usage`](Self::begin_usage)), the usage is committed when it succeeds, and
    /// the hold is released when it fails. An unmetered scope runs `future` as is.
    ///
    /// # Errors
    ///
    /// A refusal from [`begin_usage`](Self::begin_usage), in which case `future` never
    /// runs, or the error `future` returns.
    ///
    /// # Example
    ///
    /// ```
    /// use std::sync::Arc;
    /// use adk_core::{
    ///     InMemorySpendLedger, MeteredUsage, ModelCallScope, SpendKey, SpendLedger, SpendLimits,
    ///     SpendPeriod, UsageCall, UsageUnit, UsageVector,
    /// };
    ///
    /// # async fn demo() -> adk_core::Result<()> {
    /// let limits = SpendLimits::new().limit(SpendKey::org("acme").per(SpendPeriod::Day), 1_000_000);
    /// let ledger = Arc::new(InMemorySpendLedger::new(limits));
    /// let scope = ModelCallScope::new().with_spend_ledger(ledger.clone(), "acme");
    ///
    /// let call = UsageCall::new("muse", "muse-image-1.0").with_max_cost_micro_usd(10_000);
    /// let image = scope
    ///     .meter(call, async {
    ///         let usage = MeteredUsage::new(UsageVector::new().with(UsageUnit::Images, 1))
    ///             .with_cost_micro_usd(10_000);
    ///         Ok(("image bytes", usage))
    ///     })
    ///     .await?;
    /// assert_eq!(image, "image bytes");
    /// assert_eq!(ledger.spent(&SpendKey::org("acme"), SpendPeriod::Day).await?, 10_000);
    /// # Ok(())
    /// # }
    /// ```
    pub async fn meter<T, F>(&self, call: UsageCall, future: F) -> Result<T>
    where
        F: Future<Output = Result<(T, MeteredUsage)>>,
    {
        if self.is_unmetered() {
            return future.await.map(|(value, _)| value);
        }
        let hold = self.begin_usage(&call).await?;
        match future.await {
            Ok((value, usage)) => {
                hold.commit(usage).await;
                Ok(value)
            }
            Err(error) => {
                hold.release().await;
                Err(error)
            }
        }
    }
}

/// One call billed in units: the vendor and model it is billed under, and the most it
/// can cost.
///
/// # Example
///
/// ```
/// use adk_core::UsageCall;
///
/// let call = UsageCall::new("muse", "muse-image-1.0").with_max_cost_micro_usd(40_000);
/// assert_eq!(call.max_cost_micro_usd, Some(40_000));
/// assert_eq!(UsageCall::new("deepgram", "nova-3").max_cost_micro_usd, None);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct UsageCall {
    /// The spend key vendor the call is reserved and committed under, such as `muse`.
    pub vendor: String,
    /// The model or service called, named in budget errors and logs.
    pub model: String,
    /// Upper bound on the call's cost, or `None` when no price is known.
    pub max_cost_micro_usd: Option<u64>,
}

impl UsageCall {
    /// A call to `model`, billed by `vendor`, with no known price.
    pub fn new(vendor: impl Into<String>, model: impl Into<String>) -> Self {
        Self { vendor: vendor.into(), model: model.into(), max_cost_micro_usd: None }
    }

    /// Sets the most the call can cost.
    pub fn with_max_cost_micro_usd(mut self, max_cost_micro_usd: u64) -> Self {
        self.max_cost_micro_usd = Some(max_cost_micro_usd);
        self
    }
}

/// What one call billed in units used, and what it cost when the price is known.
///
/// # Example
///
/// ```
/// use adk_core::{MeteredUsage, UsageUnit, UsageVector};
///
/// let usage = MeteredUsage::new(UsageVector::new().with(UsageUnit::AudioMillis, 90_000))
///     .with_cost_micro_usd(4_500);
/// assert_eq!(usage.units.get(UsageUnit::AudioMillis), 90_000);
/// assert_eq!(usage.cost_micro_usd, Some(4_500));
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct MeteredUsage {
    /// Usage per unit.
    pub units: UsageVector,
    /// Cost in micro-USD, or `None` when no price is known.
    pub cost_micro_usd: Option<u64>,
}

impl MeteredUsage {
    /// Usage of `units` at an unknown cost.
    pub fn new(units: UsageVector) -> Self {
        Self { units, cost_micro_usd: None }
    }

    /// Sets the cost.
    pub fn with_cost_micro_usd(mut self, cost_micro_usd: u64) -> Self {
        self.cost_micro_usd = Some(cost_micro_usd);
        self
    }
}

/// The open budget check and spend hold of one call billed in units, from
/// [`ModelCallScope::begin_usage`].
///
/// | Settled with | Spend ledger | Budget |
/// |--------------|--------------|--------|
/// | [`commit`](Self::commit) | the reported cost, or the reserved bound (the scope's per-call estimate for an unpriced call) when none is reported | cost, tokens and units counted; an unknown cost under a cost cap stops the run |
/// | [`release`](Self::release), or dropped | hold released | nothing counted |
#[must_use = "an unsettled usage hold records nothing"]
pub struct UsageHold {
    spend: Option<ModelSpendHold>,
    budget: Option<Arc<BudgetTracker>>,
    model: String,
}

impl fmt::Debug for UsageHold {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UsageHold")
            .field("spend", &self.spend)
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

impl UsageHold {
    /// Records what the call used.
    pub async fn commit(mut self, usage: MeteredUsage) {
        if let Some(tracker) = &self.budget {
            tracker.record_units(&self.model, &usage.units, usage.cost_micro_usd);
        }
        if let Some(mut hold) = self.spend.take() {
            hold.observe_cost(usage.cost_micro_usd);
            hold.settle().await;
        }
    }

    /// Releases the hold of a call that failed, recording nothing.
    pub async fn release(mut self) {
        if let Some(hold) = self.spend.take() {
            hold.settle().await;
        }
    }
}

/// Runs one call billed in units under the [current](ModelCallScope::current) scope, or
/// unmetered when there is none. See [`ModelCallScope::meter`].
///
/// Media clients call this so a call made inside a tool counts against the run the tool
/// belongs to.
///
/// # Errors
///
/// See [`ModelCallScope::meter`].
///
/// # Example
///
/// ```
/// use adk_core::{MeteredUsage, UsageCall, UsageUnit, UsageVector, meter_usage};
///
/// # async fn demo() -> adk_core::Result<()> {
/// let call = UsageCall::new("muse", "muse-image-1.0").with_max_cost_micro_usd(10_000);
/// let images = meter_usage(call, async {
///     Ok((1, MeteredUsage::new(UsageVector::new().with(UsageUnit::Images, 1))))
/// })
/// .await?;
/// assert_eq!(images, 1);
/// # Ok(())
/// # }
/// ```
pub async fn meter_usage<T, F>(call: UsageCall, future: F) -> Result<T>
where
    F: Future<Output = Result<(T, MeteredUsage)>>,
{
    match ModelCallScope::current() {
        Some(scope) => scope.meter(call, future).await,
        None => future.await.map(|(value, _)| value),
    }
}

/// Settles `hold` from the responses of `responses` once the stream ends.
fn commit_on_completion(responses: LlmResponseStream, hold: ModelSpendHold) -> LlmResponseStream {
    // Dropping the stream early drops the hold, which settles it on the runtime.
    Box::pin(futures::stream::unfold(Some((responses, Some(hold))), |state| async move {
        let (mut responses, mut hold) = state?;
        match responses.next().await {
            Some(item) => {
                if let (Ok(response), Some(open)) = (&item, hold.as_mut()) {
                    open.observe(response);
                    // Committed before the caller sees the final response, so the next
                    // call's reservation already counts this one's cost.
                    if response.turn_complete
                        && !response.partial
                        && let Some(open) = hold.take()
                    {
                        open.settle().await;
                    }
                }
                Some((item, Some((responses, hold))))
            }
            None => {
                if let Some(open) = hold {
                    open.settle().await;
                }
                None
            }
        }
    }))
}

/// Calls `model` under the [current](ModelCallScope::current) scope, or unmetered when
/// there is none.
///
/// Built-in compaction summarizers and the MCP sampling handler call models through this
/// function; use it in a custom [`BaseEventsSummarizer`](crate::BaseEventsSummarizer) or
/// sampling handler so its calls count against the run.
///
/// # Errors
///
/// See [`ModelCallScope::generate`].
///
/// # Example
///
/// ```rust,ignore
/// let stream = adk_core::generate_metered(model.as_ref(), request, false).await?;
/// ```
pub async fn generate_metered(
    model: &dyn Llm,
    request: LlmRequest,
    stream: bool,
) -> Result<LlmResponseStream> {
    match ModelCallScope::current() {
        Some(scope) => scope.generate(model, request, stream).await,
        None => model.generate_content(request, stream).await,
    }
}

/// An [`Llm`] that meters every call against a [`ModelCallScope`].
///
/// With an explicit scope ([`with_scope`](Self::with_scope)) every call counts against
/// it; otherwise calls count against the [current](ModelCallScope::current) scope and run
/// unmetered outside one. Wrap the model of a custom summarizer or judge with it; do not
/// wrap an agent's own model, whose calls the agent already meters.
///
/// # Example
///
/// ```rust,ignore
/// use std::sync::Arc;
/// use adk_core::{MeteredLlm, ModelCallScope};
///
/// let judge_model = Arc::new(MeteredLlm::new(model).with_scope(scope));
/// ```
pub struct MeteredLlm {
    inner: Arc<dyn Llm>,
    scope: Option<ModelCallScope>,
}

impl MeteredLlm {
    /// Wraps `inner`, metering against the current scope.
    pub fn new(inner: Arc<dyn Llm>) -> Self {
        Self { inner, scope: None }
    }

    /// Meters every call against `scope`, whether or not another scope is current.
    pub fn with_scope(mut self, scope: ModelCallScope) -> Self {
        self.scope = Some(scope);
        self
    }
}

#[async_trait]
impl Llm for MeteredLlm {
    fn name(&self) -> &str {
        self.inner.name()
    }

    async fn generate_content(&self, req: LlmRequest, stream: bool) -> Result<LlmResponseStream> {
        match self.scope.clone().or_else(ModelCallScope::current) {
            Some(scope) => scope.generate(self.inner.as_ref(), req, stream).await,
            None => self.inner.generate_content(req, stream).await,
        }
    }

    fn schema_adapter(&self) -> &dyn SchemaAdapter {
        self.inner.schema_adapter()
    }

    fn uses_interactions_api(&self) -> bool {
        self.inner.uses_interactions_api()
    }

    fn max_cost_micro_usd(&self, request: &LlmRequest) -> Option<u64> {
        self.inner.max_cost_micro_usd(request)
    }

    fn native_web_search(&self) -> Option<crate::NativeWebSearch> {
        self.inner.native_web_search()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::{InMemorySpendLedger, RunBudget, SpendLimits, SpendPeriod, UsageUnit};
    use std::sync::atomic::{AtomicBool, Ordering};

    fn image_call(images: u64) -> UsageCall {
        UsageCall::new("muse", "muse-image-1.0").with_max_cost_micro_usd(images * 10_000)
    }

    fn images(count: u64) -> MeteredUsage {
        MeteredUsage::new(UsageVector::new().with(UsageUnit::Images, count))
            .with_cost_micro_usd(count * 10_000)
    }

    fn capped(micro_usd: u64) -> Arc<InMemorySpendLedger> {
        Arc::new(InMemorySpendLedger::new(
            SpendLimits::new().limit(SpendKey::org("acme").per(SpendPeriod::Day), micro_usd),
        ))
    }

    #[tokio::test]
    async fn a_unit_call_over_the_cap_is_refused_before_it_is_sent() {
        let ledger = capped(5_000);
        let scope = ModelCallScope::new().with_spend_ledger(ledger.clone(), "acme");
        let sent = AtomicBool::new(false);
        let refused = scope
            .meter(image_call(1), async {
                sent.store(true, Ordering::SeqCst);
                Ok(((), images(1)))
            })
            .await
            .unwrap_err();
        assert_eq!(refused.code, crate::SPEND_LIMIT_EXCEEDED_CODE);
        assert!(!sent.load(Ordering::SeqCst));
        assert_eq!(ledger.spent(&SpendKey::org("acme"), SpendPeriod::Day).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn a_unit_call_commits_its_cost_to_the_ledger_and_the_budget() {
        let ledger = capped(1_000_000);
        let tracker = Arc::new(BudgetTracker::new(RunBudget::new().max_cost_usd(1.0)));
        let scope = ModelCallScope::new()
            .with_budget(tracker.clone())
            .with_spend_ledger(ledger.clone(), "acme");
        scope.meter(image_call(2), async { Ok(((), images(2))) }).await.unwrap();

        let key = SpendKey::org("acme").with_vendor("muse");
        assert_eq!(ledger.spent(&key, SpendPeriod::Day).await.unwrap(), 20_000);
        assert_eq!(tracker.usage().cost_micro_usd, 20_000);
        assert_eq!(tracker.usage().model_calls, 0);
        assert_eq!(tracker.units(), UsageVector::new().with(UsageUnit::Images, 2));
    }

    #[tokio::test]
    async fn a_failed_unit_call_releases_its_hold_and_counts_nothing() {
        let ledger = capped(10_000);
        let tracker = Arc::new(BudgetTracker::new(RunBudget::new()));
        let scope = ModelCallScope::new()
            .with_budget(tracker.clone())
            .with_spend_ledger(ledger.clone(), "acme");
        let failed = scope
            .meter(image_call(1), async {
                Err::<((), MeteredUsage), _>(crate::AdkError::model("down"))
            })
            .await
            .unwrap_err();
        assert_eq!(failed.message, "down");
        assert_eq!(tracker.units(), UsageVector::new());
        // The released hold leaves the whole cap for the next call.
        scope.meter(image_call(1), async { Ok(((), images(1))) }).await.unwrap();
        assert_eq!(ledger.spent(&SpendKey::org("acme"), SpendPeriod::Day).await.unwrap(), 10_000);
    }

    #[tokio::test]
    async fn an_unpriced_unit_call_is_refused_under_a_ledger_cap() {
        let scope = ModelCallScope::new().with_spend_ledger(capped(1_000_000), "acme");
        let sent = AtomicBool::new(false);
        let refused = scope
            .meter(UsageCall::new("deepgram", "nova-3"), async {
                sent.store(true, Ordering::SeqCst);
                Ok(((), MeteredUsage::default()))
            })
            .await
            .unwrap_err();
        assert_eq!(refused.code, crate::SPEND_LIMIT_EXCEEDED_CODE);
        assert!(!sent.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn an_unpriced_unit_call_is_refused_under_a_budget_cost_cap() {
        let tracker = Arc::new(BudgetTracker::new(RunBudget::new().max_cost_usd(5.0)));
        let scope = ModelCallScope::new().with_budget(tracker.clone());
        let sent = AtomicBool::new(false);
        let refused = scope
            .meter(UsageCall::new("deepgram", "nova-3"), async {
                sent.store(true, Ordering::SeqCst);
                Ok(((), MeteredUsage::default()))
            })
            .await
            .unwrap_err();
        assert_eq!(refused.code, "budget.cost_unknown");
        assert!(!sent.load(Ordering::SeqCst));
        // Nothing ran, so nothing is recorded against later calls.
        assert!(tracker.check().is_ok());

        let opted_out = Arc::new(BudgetTracker::new(
            RunBudget::new().max_cost_usd(5.0).allow_unpriced_models(),
        ));
        let scope = ModelCallScope::new().with_budget(opted_out.clone());
        scope
            .meter(UsageCall::new("deepgram", "nova-3"), async {
                Ok(((), MeteredUsage::new(UsageVector::new().with(UsageUnit::AudioMillis, 800))))
            })
            .await
            .unwrap();
        assert_eq!(opted_out.units(), UsageVector::new().with(UsageUnit::AudioMillis, 800));
    }

    #[tokio::test]
    async fn an_unpriced_unit_call_without_a_cap_commits_the_estimate() {
        let ledger = Arc::new(InMemorySpendLedger::default());
        let scope = ModelCallScope::new()
            .with_spend_ledger(ledger.clone(), "acme")
            .with_spend_estimate(LlmSpendEstimate::per_call(7_000));
        scope
            .meter(UsageCall::new("deepgram", "nova-3"), async {
                Ok(((), MeteredUsage::default()))
            })
            .await
            .unwrap();
        let key = SpendKey::org("acme").with_vendor("deepgram");
        assert_eq!(ledger.spent(&key, SpendPeriod::Day).await.unwrap(), 7_000);
    }

    #[tokio::test]
    async fn a_unit_call_is_refused_once_the_budget_is_spent() {
        let tracker = Arc::new(BudgetTracker::new(RunBudget::new().max_cost_micro_usd(10_000)));
        let scope = ModelCallScope::new().with_budget(tracker.clone());
        scope.meter(image_call(1), async { Ok(((), images(1))) }).await.unwrap();
        let refused = scope.meter(image_call(1), async { Ok(((), images(1))) }).await.unwrap_err();
        assert_eq!(refused.code, "budget.cost");
    }

    #[tokio::test]
    async fn the_free_function_meters_against_the_current_scope() {
        let tracker = Arc::new(BudgetTracker::new(RunBudget::new()));
        let scope = ModelCallScope::new().with_budget(tracker.clone());
        scope.within(meter_usage(image_call(1), async { Ok(((), images(1))) })).await.unwrap();
        assert_eq!(tracker.usage().cost_micro_usd, 10_000);
        // Outside any scope the call runs unmetered.
        meter_usage(image_call(1), async { Ok(((), images(1))) }).await.unwrap();
        assert_eq!(tracker.usage().cost_micro_usd, 10_000);
    }

    #[test]
    fn vendor_is_read_from_the_model_family() {
        assert_eq!(vendor_for_model("gemini-3.7-flash"), "gemini");
        assert_eq!(vendor_for_model("models/gemini-3.1-pro-preview"), "gemini");
        assert_eq!(vendor_for_model("claude-opus-4-8"), "anthropic");
        assert_eq!(vendor_for_model("gpt-5.2"), "openai");
        assert_eq!(vendor_for_model("o3-mini"), "openai");
        assert_eq!(vendor_for_model("deepseek-v4"), "deepseek");
        assert_eq!(vendor_for_model("muse-image-1.0"), "muse");
        assert_eq!(vendor_for_model("llama-4-scout"), "unknown");
    }

    #[test]
    fn the_estimate_without_an_output_price_is_the_per_call_amount() {
        let mut request = LlmRequest::new("gemini-3.7-flash", vec![]);
        request.config = Some(crate::GenerateContentConfig {
            max_output_tokens: Some(1_000),
            ..Default::default()
        });
        assert_eq!(LlmSpendEstimate::per_call(7).estimate(&request), 7);
        assert_eq!(LlmSpendEstimate::default().estimate(&request), 50_000);
    }

    struct Searching;

    #[async_trait]
    impl Llm for Searching {
        fn name(&self) -> &str {
            "searching"
        }

        async fn generate_content(&self, _: LlmRequest, _: bool) -> Result<LlmResponseStream> {
            Ok(Box::pin(futures::stream::empty()))
        }

        fn native_web_search(&self) -> Option<crate::NativeWebSearch> {
            Some(crate::NativeWebSearch::new(
                "gemini",
                crate::NativeWebSearchTool::GoogleSearch,
                false,
            ))
        }
    }

    #[test]
    fn the_metered_wrapper_reports_the_native_search_of_the_model_it_wraps() {
        let metered = MeteredLlm::new(Arc::new(Searching));
        assert_eq!(
            metered.native_web_search(),
            Some(crate::NativeWebSearch::new(
                "gemini",
                crate::NativeWebSearchTool::GoogleSearch,
                false
            ))
        );
    }
}
