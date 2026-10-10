//! Run budgets: limits on model calls, tokens, cost, wall time and tool calls.
//!
//! A [`RunBudget`] states the limits for one run. A [`BudgetTracker`] holds the
//! shared counters for those limits. `Runner` creates one tracker per run from
//! [`RunConfig::budget`](crate::RunConfig::budget) and stores it in
//! [`RunConfig::budget_tracker`](crate::RunConfig::budget_tracker); because the
//! run configuration travels with the invocation, transfer targets, workflow
//! sub-agents and agents behind an agent tool all count against the same `Arc`.
//!
//! # Enforcement points
//!
//! | Checkpoint | Limits checked |
//! |------------|----------------|
//! | Before every model call ([`BudgetTracker::begin_model_call`]) | model calls, total tokens, cost, wall time |
//! | Before every tool dispatch ([`BudgetTracker::begin_tool_calls`]) | tool calls, total tokens, cost, wall time |
//! | After every non-partial event ([`BudgetTracker::record_event`]) | usage the call sites did not meter |
//!
//! A limit is *reached* when usage equals or exceeds it; a reached limit blocks
//! new work of that kind. Limits are checked before a call starts, so a single
//! model call can take usage past a limit; no further call starts after that.
//!
//! # Usage accounting
//!
//! Each model call is metered once, from the first non-partial response that
//! carries usage. Partial streaming chunks never add to the totals — Gemini
//! repeats cumulative usage on every chunk, so counting chunks would multiply
//! the spend.
//!
//! When a cost cap is set and a response has no known cost, the tracker fails
//! closed and stops the run with `cost unknown for model …`. Use
//! [`RunBudget::allow_unpriced_models`] to count such responses as zero cost.
//!
//! # Example
//!
//! ```
//! use std::time::Duration;
//! use adk_core::{BudgetTracker, RunBudget, RunConfig};
//!
//! let budget = RunBudget::new()
//!     .max_model_calls(20)
//!     .max_cost_usd(50.0)
//!     .max_wall_time(Duration::from_secs(600));
//! assert_eq!(budget.max_cost_micro_usd, Some(50_000_000));
//!
//! let config = RunConfig::builder().budget(budget.clone()).build();
//! assert_eq!(config.budget, Some(budget.clone()));
//!
//! let tracker = BudgetTracker::new(budget);
//! assert!(tracker.begin_tool_calls(1).is_ok());
//! assert_eq!(tracker.usage().tool_calls, 1);
//! ```

use crate::{
    AdkError, ErrorCategory, ErrorComponent, ErrorDetails, Event, Llm, LlmRequest, LlmResponse,
    LlmResponseStream, Result, UsageMetadata,
};
use futures::Stream;
use std::fmt;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

/// Event-level `provider_metadata` key marking an event whose model usage and
/// tool calls are already counted by a [`BudgetTracker`].
///
/// Call sites that meter a model call set it on the events they emit for that
/// call, so [`BudgetTracker::record_event`] does not count the usage twice.
pub const BUDGET_RECORDED_KEY: &str = "adk.budget.recorded";

/// Event-level `provider_metadata` key naming the exhausted limit on the event
/// that explains why a run stopped. See [`budget_exceeded_event`].
pub const BUDGET_LIMIT_KEY: &str = "adk.budget.limit";

/// Resource limits for one run.
///
/// Every limit is optional; `None` leaves that resource unlimited. Build one
/// with the chained setters and attach it with
/// [`RunConfigBuilder::budget`](crate::RunConfigBuilder::budget) or
/// `RunnerConfigBuilder::budget`.
///
/// # Example
///
/// ```
/// use std::time::Duration;
/// use adk_core::RunBudget;
///
/// let budget = RunBudget::new()
///     .max_model_calls(10)
///     .max_total_tokens(200_000)
///     .max_cost_micro_usd(2_500_000)
///     .max_wall_time(Duration::from_secs(120))
///     .max_tool_calls(25);
/// assert_eq!(budget.max_tool_calls, Some(25));
/// assert!(!budget.allow_unpriced_models);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct RunBudget {
    /// Maximum model calls started in the run.
    pub max_model_calls: Option<u64>,
    /// Maximum input plus output tokens reported by the models.
    pub max_total_tokens: Option<u64>,
    /// Maximum cost in millionths of a US dollar.
    pub max_cost_micro_usd: Option<u64>,
    /// Maximum wall-clock time since the tracker was created.
    pub max_wall_time: Option<Duration>,
    /// Maximum tool calls dispatched in the run.
    pub max_tool_calls: Option<u64>,
    /// Count responses with no known cost as zero cost instead of stopping.
    ///
    /// Only consulted when [`max_cost_micro_usd`](Self::max_cost_micro_usd) is
    /// set. The default `false` stops the run, because an unpriced model would
    /// otherwise spend without limit under a cost cap.
    pub allow_unpriced_models: bool,
}

impl RunBudget {
    /// Creates a budget with no limits.
    pub fn new() -> Self {
        Self::default()
    }

    /// Limits the number of model calls.
    pub fn max_model_calls(mut self, max: u64) -> Self {
        self.max_model_calls = Some(max);
        self
    }

    /// Limits the input plus output tokens reported by the models.
    pub fn max_total_tokens(mut self, max: u64) -> Self {
        self.max_total_tokens = Some(max);
        self
    }

    /// Limits the cost in millionths of a US dollar.
    pub fn max_cost_micro_usd(mut self, max: u64) -> Self {
        self.max_cost_micro_usd = Some(max);
        self
    }

    /// Limits the cost in US dollars, stored as micro-USD.
    ///
    /// Negative and non-finite values become a zero cap.
    pub fn max_cost_usd(self, max: f64) -> Self {
        self.max_cost_micro_usd(usd_to_micro(max))
    }

    /// Limits the wall-clock time of the run.
    pub fn max_wall_time(mut self, max: Duration) -> Self {
        self.max_wall_time = Some(max);
        self
    }

    /// Limits the number of tool calls.
    pub fn max_tool_calls(mut self, max: u64) -> Self {
        self.max_tool_calls = Some(max);
        self
    }

    /// Counts responses with no known cost as zero cost under a cost cap.
    ///
    /// Use this for local or self-hosted models that have no per-token price.
    pub fn allow_unpriced_models(mut self) -> Self {
        self.allow_unpriced_models = true;
        self
    }

    /// Returns `true` when no limit is set.
    pub fn is_unlimited(&self) -> bool {
        self.max_model_calls.is_none()
            && self.max_total_tokens.is_none()
            && self.max_cost_micro_usd.is_none()
            && self.max_wall_time.is_none()
            && self.max_tool_calls.is_none()
    }
}

/// The resource a budget check found exhausted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum BudgetLimit {
    /// [`RunBudget::max_model_calls`].
    ModelCalls,
    /// [`RunBudget::max_total_tokens`], or token usage the provider did not report.
    TotalTokens,
    /// [`RunBudget::max_cost_micro_usd`], or a cost no price is known for.
    Cost,
    /// [`RunBudget::max_wall_time`].
    WallTime,
    /// [`RunBudget::max_tool_calls`].
    ToolCalls,
}

impl BudgetLimit {
    /// Stable identifier used in error details and event metadata.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ModelCalls => "model_calls",
            Self::TotalTokens => "total_tokens",
            Self::Cost => "cost_micro_usd",
            Self::WallTime => "wall_time_ms",
            Self::ToolCalls => "tool_calls",
        }
    }
}

impl fmt::Display for BudgetLimit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A budget violation: which limit, how much was used, and the maximum.
///
/// Converts into an [`AdkError`] with category
/// [`ErrorCategory::ResourceExhausted`] and a `budget.*` code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetExceeded {
    limit: BudgetLimit,
    used: u64,
    max: u64,
    code: &'static str,
    message: String,
}

impl BudgetExceeded {
    fn reached(limit: BudgetLimit, used: u64, max: u64) -> Self {
        let (code, message) = match limit {
            BudgetLimit::ModelCalls => (
                "budget.model_calls",
                format!("budget exhausted: {used} of {max} model calls used"),
            ),
            BudgetLimit::TotalTokens => {
                ("budget.total_tokens", format!("budget exhausted: {used} of {max} tokens used"))
            }
            BudgetLimit::Cost => (
                "budget.cost",
                format!(
                    "budget exhausted: {} of {} spent",
                    format_micro_usd(used),
                    format_micro_usd(max)
                ),
            ),
            BudgetLimit::WallTime => (
                "budget.wall_time",
                format!("budget exhausted: {used}ms of {max}ms wall time elapsed"),
            ),
            BudgetLimit::ToolCalls => {
                ("budget.tool_calls", format!("budget exhausted: {used} of {max} tool calls used"))
            }
        };
        Self { limit, used, max, code, message }
    }

    fn tool_batch(used: u64, requested: u64, max: u64) -> Self {
        Self {
            limit: BudgetLimit::ToolCalls,
            used,
            max,
            code: "budget.tool_calls",
            message: format!(
                "budget exhausted: {requested} tool calls requested with {used} of {max} already used"
            ),
        }
    }

    fn unpriced(model: &str, used: u64, max: u64) -> Self {
        Self {
            limit: BudgetLimit::Cost,
            used,
            max,
            code: "budget.cost_unknown",
            message: format!(
                "cost unknown for model '{model}': the budget has a cost cap of {} and no price is known for this model; set UsageMetadata::cost in the provider or opt out with RunBudget::allow_unpriced_models()",
                format_micro_usd(max)
            ),
        }
    }

    fn untokened(model: &str, used: u64, max: u64) -> Self {
        Self {
            limit: BudgetLimit::TotalTokens,
            used,
            max,
            code: "budget.tokens_unknown",
            message: format!(
                "token usage unknown for model '{model}': the provider reported no usage and the budget has a token cap of {max}"
            ),
        }
    }

    /// The exhausted limit.
    pub fn limit(&self) -> BudgetLimit {
        self.limit
    }

    /// Usage when the violation was detected, in the limit's unit.
    pub fn used(&self) -> u64 {
        self.used
    }

    /// The configured maximum, in the limit's unit.
    pub fn max(&self) -> u64 {
        self.max
    }

    /// Stable error code, for example `budget.cost` or `budget.cost_unknown`.
    pub fn code(&self) -> &'static str {
        self.code
    }

    /// Human-readable explanation.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for BudgetExceeded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for BudgetExceeded {}

impl From<BudgetExceeded> for AdkError {
    fn from(exceeded: BudgetExceeded) -> Self {
        let mut details = ErrorDetails::default();
        details.metadata.insert("limit".to_string(), exceeded.limit.as_str().into());
        details.metadata.insert("used".to_string(), exceeded.used.into());
        details.metadata.insert("max".to_string(), exceeded.max.into());
        AdkError::new(
            ErrorComponent::Agent,
            ErrorCategory::ResourceExhausted,
            exceeded.code,
            exceeded.message,
        )
        .with_details(details)
    }
}

/// A snapshot of the usage counted by a [`BudgetTracker`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BudgetUsage {
    /// Model calls started.
    pub model_calls: u64,
    /// Input plus output tokens reported.
    pub total_tokens: u64,
    /// Cost in millionths of a US dollar.
    pub cost_micro_usd: u64,
    /// Tool calls dispatched.
    pub tool_calls: u64,
    /// Wall-clock time since the tracker was created.
    pub elapsed: Duration,
}

/// Shared, thread-safe counters for one [`RunBudget`].
///
/// Clone the `Arc`, never the tracker: every holder of the same `Arc` counts
/// against the same limits. A tracker created with
/// [`with_parent`](Self::with_parent) also counts against its parent, so a
/// team budget nested in a run budget enforces both.
///
/// # Example
///
/// ```
/// use std::sync::Arc;
/// use adk_core::{BudgetLimit, BudgetTracker, RunBudget};
///
/// let tracker = Arc::new(BudgetTracker::new(RunBudget::new().max_model_calls(1)));
/// let meter = tracker.begin_model_call("gemini-3.7-flash").unwrap();
/// drop(meter);
///
/// let exceeded = tracker.begin_model_call("gemini-3.7-flash").unwrap_err();
/// assert_eq!(exceeded.limit(), BudgetLimit::ModelCalls);
/// assert_eq!(tracker.usage().model_calls, 1);
/// ```
#[derive(Debug)]
pub struct BudgetTracker {
    budget: RunBudget,
    started_at: Instant,
    model_calls: AtomicU64,
    total_tokens: AtomicU64,
    cost_micro_usd: AtomicU64,
    tool_calls: AtomicU64,
    exceeded: Mutex<Option<BudgetExceeded>>,
    parent: Option<Arc<BudgetTracker>>,
}

impl BudgetTracker {
    /// Creates a tracker for `budget` with zero usage, starting the wall clock now.
    pub fn new(budget: RunBudget) -> Self {
        Self::resume(budget, BudgetUsage::default(), None)
    }

    /// Creates a tracker that also counts against, and is limited by, `parent`.
    pub fn with_parent(budget: RunBudget, parent: Arc<BudgetTracker>) -> Self {
        Self::resume(budget, BudgetUsage::default(), Some(parent))
    }

    /// Creates a tracker that continues from previously recorded usage.
    ///
    /// The wall clock starts `usage.elapsed` in the past, so a resumed run keeps
    /// its elapsed time. Usage is not added to `parent`.
    pub fn resume(
        budget: RunBudget,
        usage: BudgetUsage,
        parent: Option<Arc<BudgetTracker>>,
    ) -> Self {
        let now = Instant::now();
        Self {
            budget,
            started_at: now.checked_sub(usage.elapsed).unwrap_or(now),
            model_calls: AtomicU64::new(usage.model_calls),
            total_tokens: AtomicU64::new(usage.total_tokens),
            cost_micro_usd: AtomicU64::new(usage.cost_micro_usd),
            tool_calls: AtomicU64::new(usage.tool_calls),
            exceeded: Mutex::new(None),
            parent,
        }
    }

    /// The limits this tracker enforces.
    pub fn budget(&self) -> &RunBudget {
        &self.budget
    }

    /// The parent tracker, if this tracker was created with one.
    pub fn parent(&self) -> Option<&Arc<BudgetTracker>> {
        self.parent.as_ref()
    }

    /// Usage counted so far by this tracker.
    pub fn usage(&self) -> BudgetUsage {
        BudgetUsage {
            model_calls: self.model_calls.load(Ordering::SeqCst),
            total_tokens: self.total_tokens.load(Ordering::SeqCst),
            cost_micro_usd: self.cost_micro_usd.load(Ordering::SeqCst),
            tool_calls: self.tool_calls.load(Ordering::SeqCst),
            elapsed: self.started_at.elapsed(),
        }
    }

    /// The first unknown-usage violation recorded by this tracker or an ancestor.
    ///
    /// A response with no known cost under a cost cap, or no usage under a token
    /// cap, cannot be read back from the counters, so it is recorded and every
    /// later check fails with it. Limits reached by usage are recomputed at each
    /// check instead.
    pub fn exceeded(&self) -> Option<BudgetExceeded> {
        let own = self.exceeded.lock().unwrap_or_else(|e| e.into_inner()).clone();
        own.or_else(|| self.parent.as_ref().and_then(|parent| parent.exceeded()))
    }

    /// Checks that no token, cost or wall-time limit is reached.
    ///
    /// # Errors
    ///
    /// Returns the violation when a limit is reached in this tracker or an
    /// ancestor, or when a violation was recorded earlier.
    pub fn check(&self) -> std::result::Result<(), BudgetExceeded> {
        self.check_chain(Strictness::Reached)
    }

    /// Reserves one model call, after checking every limit that gates it.
    ///
    /// The returned [`ModelCallMeter`] records the call's usage when it sees
    /// the first non-partial response carrying usage, or when it is dropped.
    ///
    /// # Errors
    ///
    /// Returns the violation when the model-call, token, cost or wall-time
    /// limit is reached in this tracker or an ancestor.
    pub fn begin_model_call(
        self: &Arc<Self>,
        model: impl Into<String>,
    ) -> std::result::Result<ModelCallMeter, BudgetExceeded> {
        self.check()?;
        self.reserve(Counter::ModelCalls, 1)?;
        Ok(ModelCallMeter {
            tracker: Arc::clone(self),
            model: model.into(),
            fallback: None,
            responses_seen: false,
            settled: false,
        })
    }

    /// Reserves `count` tool calls, after checking every limit that gates them.
    ///
    /// The reservation is all or nothing: a batch that does not fit in the
    /// remaining tool-call budget reserves nothing.
    ///
    /// # Errors
    ///
    /// Returns the violation when the batch does not fit, or when the token,
    /// cost or wall-time limit is reached in this tracker or an ancestor.
    pub fn begin_tool_calls(&self, count: u64) -> std::result::Result<(), BudgetExceeded> {
        self.check()?;
        if count == 0 {
            return Ok(());
        }
        self.reserve(Counter::ToolCalls, count)
    }

    /// Counts usage carried by an event that no call site metered.
    ///
    /// Partial events and events already marked with [`BUDGET_RECORDED_KEY`]
    /// are skipped. Otherwise the event's usage counts as one model call and its
    /// function calls as tool calls, and the event is marked so an outer
    /// tracker does not count it again. Use this where events from agents that
    /// call models directly — custom or remote agents — pass through.
    ///
    /// # Errors
    ///
    /// Returns the violation when a recorded violation exists or when usage now
    /// exceeds a limit.
    pub fn record_event(&self, event: &mut Event) -> std::result::Result<(), BudgetExceeded> {
        if event.llm_response.partial || event.provider_metadata.contains_key(BUDGET_RECORDED_KEY) {
            return self.check_chain(Strictness::Exceeded);
        }
        let usage = event.llm_response.usage_metadata.clone();
        let tool_calls = event.tool_calls().len() as u64;
        if usage.is_none() && tool_calls == 0 {
            return self.check_chain(Strictness::Exceeded);
        }
        if let Some(usage) = usage {
            self.add(Counter::ModelCalls, 1);
            let model =
                event.llm_response.model.clone().unwrap_or_else(|| "unattributed".to_string());
            self.record_usage(&model, Some(&usage));
        }
        self.add(Counter::ToolCalls, tool_calls);
        event.provider_metadata.insert(BUDGET_RECORDED_KEY.to_string(), "true".to_string());
        self.check_chain(Strictness::Exceeded)
    }

    fn check_chain(&self, strictness: Strictness) -> std::result::Result<(), BudgetExceeded> {
        if let Some(exceeded) = self.exceeded.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            return Err(exceeded);
        }
        if let Some(violation) = self.limit_violation(strictness) {
            return Err(violation);
        }
        match &self.parent {
            Some(parent) => parent.check_chain(strictness),
            None => Ok(()),
        }
    }

    fn limit_violation(&self, strictness: Strictness) -> Option<BudgetExceeded> {
        let usage = self.usage();
        let over = |used: u64, max: u64| match strictness {
            Strictness::Reached => used >= max,
            Strictness::Exceeded => used > max,
        };
        if let Some(max) = self.budget.max_total_tokens
            && over(usage.total_tokens, max)
        {
            return Some(BudgetExceeded::reached(
                BudgetLimit::TotalTokens,
                usage.total_tokens,
                max,
            ));
        }
        if let Some(max) = self.budget.max_cost_micro_usd
            && over(usage.cost_micro_usd, max)
        {
            return Some(BudgetExceeded::reached(BudgetLimit::Cost, usage.cost_micro_usd, max));
        }
        if let Some(max) = self.budget.max_wall_time {
            let elapsed = duration_ms(usage.elapsed);
            let max = duration_ms(max);
            if over(elapsed, max) {
                return Some(BudgetExceeded::reached(BudgetLimit::WallTime, elapsed, max));
            }
        }
        if strictness == Strictness::Exceeded {
            if let Some(max) = self.budget.max_model_calls
                && usage.model_calls > max
            {
                return Some(BudgetExceeded::reached(
                    BudgetLimit::ModelCalls,
                    usage.model_calls,
                    max,
                ));
            }
            if let Some(max) = self.budget.max_tool_calls
                && usage.tool_calls > max
            {
                return Some(BudgetExceeded::reached(
                    BudgetLimit::ToolCalls,
                    usage.tool_calls,
                    max,
                ));
            }
        }
        None
    }

    fn remember(&self, violation: BudgetExceeded) -> BudgetExceeded {
        let mut slot = self.exceeded.lock().unwrap_or_else(|e| e.into_inner());
        slot.get_or_insert(violation).clone()
    }

    fn counter(&self, counter: Counter) -> &AtomicU64 {
        match counter {
            Counter::ModelCalls => &self.model_calls,
            Counter::ToolCalls => &self.tool_calls,
            Counter::TotalTokens => &self.total_tokens,
            Counter::Cost => &self.cost_micro_usd,
        }
    }

    fn reserve(&self, counter: Counter, count: u64) -> std::result::Result<(), BudgetExceeded> {
        let max = match counter {
            Counter::ModelCalls => self.budget.max_model_calls,
            Counter::ToolCalls => self.budget.max_tool_calls,
            Counter::TotalTokens | Counter::Cost => None,
        };
        let atomic = self.counter(counter);
        let reserved = atomic.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |used| {
            let next = used.saturating_add(count);
            match max {
                Some(max) if next > max => None,
                _ => Some(next),
            }
        });
        if let Err(used) = reserved {
            let max = max.unwrap_or_default();
            let violation = match counter {
                Counter::ToolCalls if count > 1 => BudgetExceeded::tool_batch(used, count, max),
                Counter::ToolCalls => BudgetExceeded::reached(BudgetLimit::ToolCalls, used, max),
                Counter::ModelCalls | Counter::TotalTokens | Counter::Cost => {
                    BudgetExceeded::reached(BudgetLimit::ModelCalls, used, max)
                }
            };
            return Err(violation);
        }
        if let Some(parent) = &self.parent
            && let Err(violation) = parent.reserve(counter, count)
        {
            atomic.fetch_sub(count, Ordering::SeqCst);
            return Err(violation);
        }
        Ok(())
    }

    fn add(&self, counter: Counter, amount: u64) {
        if amount == 0 {
            return;
        }
        let atomic = self.counter(counter);
        let _ = atomic.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |used| {
            Some(used.saturating_add(amount))
        });
        if let Some(parent) = &self.parent {
            parent.add(counter, amount);
        }
    }

    /// Adds one call's usage to this tracker and its ancestors.
    fn record_usage(&self, model: &str, usage: Option<&UsageMetadata>) {
        match usage {
            Some(usage) => {
                self.add(Counter::TotalTokens, u64::try_from(usage.total_token_count).unwrap_or(0));
                match usage.cost {
                    Some(cost) => self.add(Counter::Cost, usd_to_micro(cost)),
                    None => self.flag_unpriced(model),
                }
            }
            None => {
                self.flag_unpriced(model);
                self.flag_untokened(model);
            }
        }
    }

    fn flag_unpriced(&self, model: &str) {
        if let Some(max) = self.budget.max_cost_micro_usd
            && !self.budget.allow_unpriced_models
        {
            let used = self.cost_micro_usd.load(Ordering::SeqCst);
            self.remember(BudgetExceeded::unpriced(model, used, max));
        }
        if let Some(parent) = &self.parent {
            parent.flag_unpriced(model);
        }
    }

    fn flag_untokened(&self, model: &str) {
        if let Some(max) = self.budget.max_total_tokens {
            let used = self.total_tokens.load(Ordering::SeqCst);
            self.remember(BudgetExceeded::untokened(model, used, max));
        }
        if let Some(parent) = &self.parent {
            parent.flag_untokened(model);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Strictness {
    /// Usage equal to the limit fails: gates new work.
    Reached,
    /// Only usage above the limit fails: audits work that already happened.
    Exceeded,
}

#[derive(Debug, Clone, Copy)]
enum Counter {
    ModelCalls,
    ToolCalls,
    TotalTokens,
    Cost,
}

/// Records the usage of one model call into a [`BudgetTracker`].
///
/// Feed every response of the call to [`observe`](Self::observe). The meter
/// records once: at the first non-partial response that carries usage, or —
/// when the stream ends early or carries usage only on partial chunks — when
/// the meter is dropped, from the last usage it saw. Created by
/// [`BudgetTracker::begin_model_call`].
#[derive(Debug)]
pub struct ModelCallMeter {
    tracker: Arc<BudgetTracker>,
    model: String,
    fallback: Option<(String, UsageMetadata)>,
    responses_seen: bool,
    settled: bool,
}

impl ModelCallMeter {
    /// Observes one response of the call.
    pub fn observe(&mut self, response: &LlmResponse) {
        if self.settled {
            return;
        }
        self.responses_seen = true;
        let Some(usage) = &response.usage_metadata else {
            return;
        };
        let model = response.model.clone().unwrap_or_else(|| self.model.clone());
        if response.partial {
            self.fallback = Some((model, usage.clone()));
            return;
        }
        self.settled = true;
        self.tracker.record_usage(&model, Some(usage));
    }

    /// Returns `true` once the call's usage has been recorded.
    pub fn is_settled(&self) -> bool {
        self.settled
    }

    /// Records the call now, from the best usage observed so far.
    pub fn finish(mut self) {
        self.settle();
    }

    fn settle(&mut self) {
        if self.settled {
            return;
        }
        self.settled = true;
        if !self.responses_seen {
            return;
        }
        match self.fallback.take() {
            Some((model, usage)) => self.tracker.record_usage(&model, Some(&usage)),
            None => self.tracker.record_usage(&self.model, None),
        }
    }
}

impl Drop for ModelCallMeter {
    fn drop(&mut self) {
        self.settle();
    }
}

/// Wraps a model response stream so that `meter` observes every response and
/// records the call when the stream ends or is dropped.
pub fn meter_stream(stream: LlmResponseStream, meter: ModelCallMeter) -> LlmResponseStream {
    Box::pin(MeteredStream { inner: stream, meter: Some(meter) })
}

struct MeteredStream {
    inner: LlmResponseStream,
    meter: Option<ModelCallMeter>,
}

impl Stream for MeteredStream {
    type Item = Result<LlmResponse>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let item = self.inner.as_mut().poll_next(cx);
        match &item {
            Poll::Ready(Some(Ok(response))) => {
                if let Some(meter) = self.meter.as_mut() {
                    meter.observe(response);
                }
            }
            Poll::Ready(None) => {
                self.meter.take();
            }
            Poll::Ready(Some(Err(_))) | Poll::Pending => {}
        }
        item
    }
}

/// Calls `model` under `tracker`, when there is one.
///
/// Reserves the call before it starts and meters the returned stream. Agents
/// that call models directly use this so their calls count against the run
/// budget.
///
/// # Errors
///
/// Returns a [`ErrorCategory::ResourceExhausted`] error when the budget does
/// not allow another model call, or the model's own error.
///
/// # Example
///
/// ```rust,ignore
/// let tracker = ctx.run_config().budget_tracker.clone();
/// let stream = adk_core::generate_with_budget(model.as_ref(), request, false, tracker.as_ref()).await?;
/// ```
pub async fn generate_with_budget(
    model: &dyn Llm,
    request: LlmRequest,
    stream: bool,
    tracker: Option<&Arc<BudgetTracker>>,
) -> Result<LlmResponseStream> {
    let Some(tracker) = tracker else {
        return model.generate_content(request, stream).await;
    };
    let meter = tracker.begin_model_call(model.name())?;
    let responses = model.generate_content(request, stream).await?;
    Ok(meter_stream(responses, meter))
}

/// Builds the event that explains why a run stopped on a budget.
///
/// Returns `None` unless `error` has category
/// [`ErrorCategory::ResourceExhausted`]. The event is non-partial and
/// turn-complete, carries the error code and message in
/// `llm_response.error_code` / `error_message`, and names the limit under
/// [`BUDGET_LIMIT_KEY`].
///
/// # Example
///
/// ```
/// use adk_core::{AdkError, BudgetTracker, RunBudget, budget_exceeded_event};
///
/// let tracker = BudgetTracker::new(RunBudget::new().max_tool_calls(0));
/// let error: AdkError = tracker.begin_tool_calls(1).unwrap_err().into();
/// let event = budget_exceeded_event("inv-1", "assistant", &error).unwrap();
/// assert_eq!(event.llm_response.error_code.as_deref(), Some("budget.tool_calls"));
/// assert_eq!(event.provider_metadata["adk.budget.limit"], "tool_calls");
/// ```
pub fn budget_exceeded_event(invocation_id: &str, author: &str, error: &AdkError) -> Option<Event> {
    if error.category != ErrorCategory::ResourceExhausted {
        return None;
    }
    let mut event = Event::new(invocation_id);
    event.author = author.to_string();
    event.llm_response.partial = false;
    event.llm_response.turn_complete = true;
    event.llm_response.finish_reason = Some(crate::FinishReason::Other);
    event.llm_response.error_code = Some(error.code.to_string());
    event.llm_response.error_message = Some(error.message.clone());
    if let Some(limit) = error.details.metadata.get("limit").and_then(|value| value.as_str()) {
        event.provider_metadata.insert(BUDGET_LIMIT_KEY.to_string(), limit.to_string());
    }
    Some(event)
}

fn usd_to_micro(usd: f64) -> u64 {
    if !usd.is_finite() || usd <= 0.0 {
        return 0;
    }
    let micro = (usd * 1_000_000.0).round();
    if micro >= u64::MAX as f64 { u64::MAX } else { micro as u64 }
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn format_micro_usd(micro: u64) -> String {
    format!("${}.{:06}", micro / 1_000_000, micro % 1_000_000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Content, Part};

    fn usage(total: i32, cost: Option<f64>) -> UsageMetadata {
        UsageMetadata {
            prompt_token_count: total / 2,
            candidates_token_count: total - total / 2,
            total_token_count: total,
            cost,
            ..Default::default()
        }
    }

    fn response(partial: bool, usage: Option<UsageMetadata>) -> LlmResponse {
        LlmResponse {
            partial,
            usage_metadata: usage,
            model: Some("gemini-3.7-flash".to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn model_call_limit_blocks_the_next_call() {
        let tracker = Arc::new(BudgetTracker::new(RunBudget::new().max_model_calls(2)));
        tracker.begin_model_call("m").unwrap().finish();
        tracker.begin_model_call("m").unwrap().finish();
        let error = tracker.begin_model_call("m").unwrap_err();
        assert_eq!(error.limit(), BudgetLimit::ModelCalls);
        assert_eq!(tracker.usage().model_calls, 2);
    }

    #[test]
    fn partial_chunks_do_not_add_usage() {
        let tracker = Arc::new(BudgetTracker::new(RunBudget::new()));
        let mut meter = tracker.begin_model_call("gemini-3.7-flash").unwrap();
        // Gemini repeats cumulative usage on every chunk.
        meter.observe(&response(true, Some(usage(100, Some(0.001)))));
        meter.observe(&response(true, Some(usage(150, Some(0.0015)))));
        meter.observe(&response(false, Some(usage(200, Some(0.002)))));
        meter.observe(&response(false, Some(usage(200, Some(0.002)))));
        drop(meter);
        let usage = tracker.usage();
        assert_eq!((usage.model_calls, usage.total_tokens, usage.cost_micro_usd), (1, 200, 2_000));
    }

    #[test]
    fn a_cut_stream_records_its_last_partial_usage() {
        let tracker = Arc::new(BudgetTracker::new(RunBudget::new()));
        let mut meter = tracker.begin_model_call("m").unwrap();
        meter.observe(&response(true, Some(usage(100, Some(0.001)))));
        meter.observe(&response(true, Some(usage(150, Some(0.0015)))));
        drop(meter);
        assert_eq!(tracker.usage().total_tokens, 150);
        assert_eq!(tracker.usage().cost_micro_usd, 1_500);
    }

    #[test]
    fn cost_cap_stops_once_reached() {
        let tracker = Arc::new(BudgetTracker::new(RunBudget::new().max_cost_usd(0.003)));
        for _ in 0..3 {
            let mut meter = tracker.begin_model_call("m").unwrap();
            meter.observe(&response(false, Some(usage(10, Some(0.001)))));
        }
        let error = tracker.begin_model_call("m").unwrap_err();
        assert_eq!(error.limit(), BudgetLimit::Cost);
        assert_eq!(error.code(), "budget.cost");
        assert_eq!(error.used(), 3_000);
        assert!(tracker.begin_tool_calls(1).is_err());
    }

    #[test]
    fn unknown_cost_under_a_cap_fails_closed() {
        let tracker = Arc::new(BudgetTracker::new(RunBudget::new().max_cost_usd(10.0)));
        let mut meter = tracker.begin_model_call("mystery-model").unwrap();
        meter.observe(&response(false, Some(usage(10, None))));
        let error = tracker.begin_model_call("mystery-model").unwrap_err();
        assert_eq!(error.code(), "budget.cost_unknown");
        assert!(error.message().contains("cost unknown for model 'gemini-3.7-flash'"));

        let allowed = Arc::new(BudgetTracker::new(
            RunBudget::new().max_cost_usd(10.0).allow_unpriced_models(),
        ));
        let mut meter = allowed.begin_model_call("mystery-model").unwrap();
        meter.observe(&response(false, Some(usage(10, None))));
        drop(meter);
        assert!(allowed.begin_model_call("mystery-model").is_ok());
    }

    #[test]
    fn missing_usage_under_a_token_cap_fails_closed() {
        let tracker = Arc::new(BudgetTracker::new(RunBudget::new().max_total_tokens(100)));
        let mut meter = tracker.begin_model_call("silent").unwrap();
        meter.observe(&response(false, None));
        drop(meter);
        assert_eq!(tracker.check().unwrap_err().code(), "budget.tokens_unknown");
    }

    #[test]
    fn tool_batches_are_all_or_nothing() {
        let tracker = BudgetTracker::new(RunBudget::new().max_tool_calls(3));
        tracker.begin_tool_calls(2).unwrap();
        let error = tracker.begin_tool_calls(2).unwrap_err();
        assert_eq!(error.limit(), BudgetLimit::ToolCalls);
        assert_eq!(tracker.usage().tool_calls, 2);
        tracker.begin_tool_calls(1).unwrap();
        assert!(tracker.begin_tool_calls(1).is_err());
    }

    #[test]
    fn wall_time_is_checked_at_every_checkpoint() {
        let tracker = Arc::new(BudgetTracker::resume(
            RunBudget::new().max_wall_time(Duration::from_millis(50)),
            BudgetUsage { elapsed: Duration::from_millis(60), ..Default::default() },
            None,
        ));
        assert_eq!(tracker.begin_model_call("m").unwrap_err().limit(), BudgetLimit::WallTime);
        assert_eq!(tracker.begin_tool_calls(1).unwrap_err().limit(), BudgetLimit::WallTime);
    }

    #[test]
    fn child_trackers_count_against_their_parent() {
        let parent = Arc::new(BudgetTracker::new(RunBudget::new().max_model_calls(2)));
        let child = Arc::new(BudgetTracker::with_parent(
            RunBudget::new().max_model_calls(5),
            parent.clone(),
        ));
        child.begin_model_call("m").unwrap().finish();
        parent.begin_model_call("m").unwrap().finish();
        let error = child.begin_model_call("m").unwrap_err();
        assert_eq!(error.limit(), BudgetLimit::ModelCalls);
        // The failed parent reservation rolled the child back.
        assert_eq!(child.usage().model_calls, 1);
        assert_eq!(parent.usage().model_calls, 2);
    }

    #[test]
    fn record_event_counts_unmetered_events_once() {
        let tracker = BudgetTracker::new(RunBudget::new());
        let mut event = Event::new("inv");
        event.llm_response.usage_metadata = Some(usage(40, Some(0.004)));
        event.llm_response.content = Some(Content {
            role: "model".to_string(),
            parts: vec![Part::FunctionCall {
                name: "lookup".to_string(),
                args: serde_json::json!({}),
                id: None,
                thought_signature: None,
            }],
        });
        tracker.record_event(&mut event).unwrap();
        tracker.record_event(&mut event).unwrap();
        let mut partial = Event::new("inv");
        partial.llm_response.partial = true;
        partial.llm_response.usage_metadata = Some(usage(999, Some(9.0)));
        tracker.record_event(&mut partial).unwrap();
        let usage = tracker.usage();
        assert_eq!(
            (usage.model_calls, usage.total_tokens, usage.cost_micro_usd, usage.tool_calls),
            (1, 40, 4_000, 1)
        );
        assert_eq!(event.provider_metadata[BUDGET_RECORDED_KEY], "true");
    }

    #[test]
    fn violations_convert_to_resource_exhausted_errors() {
        let tracker = BudgetTracker::new(RunBudget::new().max_tool_calls(0));
        let error: AdkError = tracker.begin_tool_calls(1).unwrap_err().into();
        assert_eq!(error.category, ErrorCategory::ResourceExhausted);
        assert_eq!(error.code, "budget.tool_calls");
        assert!(!error.is_retryable());
        assert_eq!(error.details.metadata["limit"], "tool_calls");
        let event = budget_exceeded_event("inv", "agent", &error).unwrap();
        assert!(!event.llm_response.partial);
        assert_eq!(event.llm_response.error_message.as_deref(), Some(error.message.as_str()));
        assert!(budget_exceeded_event("inv", "agent", &AdkError::agent("other")).is_none());
    }

    #[tokio::test]
    async fn metered_streams_record_on_completion() {
        let tracker = Arc::new(BudgetTracker::new(RunBudget::new()));
        let meter = tracker.begin_model_call("m").unwrap();
        let responses: Vec<Result<LlmResponse>> = vec![
            Ok(response(true, Some(usage(5, Some(0.0005))))),
            Ok(response(false, Some(usage(10, Some(0.001))))),
        ];
        let stream = meter_stream(Box::pin(futures::stream::iter(responses)), meter);
        let collected: Vec<_> = futures::StreamExt::collect(stream).await;
        assert_eq!(collected.len(), 2);
        assert_eq!(tracker.usage().total_tokens, 10);
        assert_eq!(tracker.usage().cost_micro_usd, 1_000);
    }
}
