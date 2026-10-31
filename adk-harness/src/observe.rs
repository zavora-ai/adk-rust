//! Observability and self-correction (spec 3.1 §7.13, feature `observe`).
//!
//! | What | Item |
//! |---|---|
//! | Spans | `harness.run` and `harness.decide` spans carrying [`ATTR_MODE`], [`ATTR_TOOL_KIND`], [`ATTR_ISOLATION`], [`ATTR_RULE_ID`] and [`ATTR_MEMBER`]; [`member_span`] and [`goal_iteration_span`] for team members and goal iterations |
//! | Cost | a [`SpendLedger`] on every run, with spend attributed to the acting agent (`SpendKey::agent`) and the subject; [`SpendReport`] from `Harness::spend`; a `harness.spend` record at the end of each run |
//! | Loop guard | [`LoopGuard`] runs `adk-eval`'s `TraceAnalyzer` over the invocation's tool calls before each model request; a loop tightens the session to ReadOnly and ends the turn |

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use adk_core::audit_log::{AuditActor, AuditTrail};
use adk_core::{
    AdkError, BudgetTracker, BudgetUsage, ErrorCategory, ErrorComponent, EventStream,
    InvocationContext, LlmRequest, Part, ReadonlyContext, ReservationId, Result, RunBudget,
    SpendKey, SpendLedger, SpendPeriod, SubjectRef,
};
use adk_eval::{ToolCallRecord, TraceAnalyzer, TracePattern};
use async_trait::async_trait;
use futures::StreamExt as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::{Instrument as _, Span};

use crate::audit::HarnessAudit;
use crate::builder::LoopGuard;
use crate::mode::{HarnessMode, ModeSource, ModeStore, write_mode_change};
use crate::policy::{Isolation, ToolKind};
use crate::shell::IsolationStatus;
use crate::toolset::RequestObserver;

/// Span attribute: the mode a run or decision is under (spec 3.1 §7.13).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::ATTR_MODE, "harness.mode");
/// ```
pub const ATTR_MODE: &str = "harness.mode";
/// Span attribute: the harness class of a tool call.
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::ATTR_TOOL_KIND, "harness.tool_kind");
/// ```
pub const ATTR_TOOL_KIND: &str = "harness.tool_kind";
/// Span attribute: where `bash` runs.
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::ATTR_ISOLATION, "harness.isolation");
/// ```
pub const ATTR_ISOLATION: &str = "harness.isolation";
/// Span attribute: the standing rule that allowed a call.
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::ATTR_RULE_ID, "harness.rule_id");
/// ```
pub const ATTR_RULE_ID: &str = "harness.rule_id";
/// Span attribute: the team member a call or run belongs to.
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::ATTR_MEMBER, "harness.member");
/// ```
pub const ATTR_MEMBER: &str = "harness.member";
/// Span attribute: the iteration of a goal.
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::ATTR_GOAL_ITERATION, "harness.goal.iteration");
/// ```
pub const ATTR_GOAL_ITERATION: &str = "harness.goal.iteration";

/// Error code of a turn the loop guard ended (spec 3.1 §7.13).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::LOOP_DETECTED_CODE, "harness.loop_detected");
/// ```
pub const LOOP_DETECTED_CODE: &str = "harness.loop_detected";

// ── Spans ──────────────────────────────────────────────────────────────────────────

/// A short label of a tool class, such as `shell:host` or `external:read_only:trusted`.
///
/// # Example
///
/// ```rust
/// use adk_harness::{Isolation, ToolKind, tool_kind_label};
///
/// assert_eq!(tool_kind_label(&ToolKind::Shell(Isolation::OsSandbox)), "shell:os_sandbox");
/// assert_eq!(tool_kind_label(&ToolKind::MemoryWrite), "memory_write");
/// ```
pub fn tool_kind_label(kind: &ToolKind) -> String {
    let isolation = |isolation: &Isolation| match isolation {
        Isolation::OsSandbox => "os_sandbox",
        Isolation::Container => "container",
        Isolation::Host => "host",
    };
    match kind {
        ToolKind::Read => "read".to_string(),
        ToolKind::Todo => "todo".to_string(),
        ToolKind::PlanExit => "plan_exit".to_string(),
        ToolKind::Edit => "edit".to_string(),
        ToolKind::Shell(iso) => format!("shell:{}", isolation(iso)),
        ToolKind::ShellEscalation => "shell_escalation".to_string(),
        ToolKind::Delegate => "delegate".to_string(),
        ToolKind::MemoryWrite => "memory_write".to_string(),
        ToolKind::MemoryPropose => "memory_propose".to_string(),
        ToolKind::Web { allowlisted } => {
            format!("web:{}", if *allowlisted { "allowlisted" } else { "other" })
        }
        ToolKind::Payment => "payment".to_string(),
        ToolKind::ComputerUse => "computer_use".to_string(),
        ToolKind::External { effect, trusted_origin } => format!(
            "external:{}:{}",
            serde_json::to_value(effect)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_default(),
            if *trusted_origin { "trusted" } else { "untrusted" }
        ),
    }
}

/// A short label of where `bash` runs: `host`, `os_sandbox:<enforcer>`,
/// `container:<image>` or `degraded:<requested>`.
///
/// # Example
///
/// ```rust
/// use adk_harness::{IsolationStatus, isolation_label};
///
/// assert_eq!(isolation_label(&IsolationStatus::Host), "host");
/// assert_eq!(isolation_label(&IsolationStatus::OsSandbox { enforcer: "seatbelt".into() }), "os_sandbox:seatbelt");
/// ```
pub fn isolation_label(status: &IsolationStatus) -> String {
    match status {
        IsolationStatus::Host => "host".to_string(),
        IsolationStatus::OsSandbox { enforcer } => format!("os_sandbox:{enforcer}"),
        IsolationStatus::Container { image } => format!("container:{image}"),
        IsolationStatus::Degraded { requested, .. } => format!("degraded:{requested}"),
    }
}

/// The span of one harness run: `harness.run` with the mode and isolation
/// (spec 3.1 §7.13).
///
/// # Example
///
/// ```rust
/// use adk_harness::{HarnessMode, IsolationStatus, run_span};
///
/// let span = run_span("harness", "inv-1", HarnessMode::Ask, &IsolationStatus::Host);
/// # let _ = span;
/// ```
pub fn run_span(
    agent: &str,
    invocation_id: &str,
    mode: HarnessMode,
    isolation: &IsolationStatus,
) -> Span {
    tracing::info_span!(
        "harness.run",
        agent.name = agent,
        invocation.id = invocation_id,
        harness.mode = %mode,
        harness.isolation = isolation_label(isolation),
        otel.kind = "internal"
    )
}

/// The span of one policy decision: `harness.decide`, with the class, mode, rule and
/// member recorded once the decision is made (spec 3.1 §7.13).
pub(crate) fn decision_span(tool: &str, isolation: &IsolationStatus) -> Span {
    tracing::info_span!(
        "harness.decide",
        tool.name = tool,
        harness.isolation = isolation_label(isolation),
        harness.tool_kind = tracing::field::Empty,
        harness.mode = tracing::field::Empty,
        harness.rule_id = tracing::field::Empty,
        harness.member = tracing::field::Empty,
        harness.decision = tracing::field::Empty,
    )
}

/// The span of a team member's delegated run: `harness.member` (spec 3.1 §7.13).
///
/// # Example
///
/// ```rust
/// let span = adk_harness::member_span("explore", "inv-1");
/// # let _ = span;
/// ```
pub fn member_span(member: &str, invocation_id: &str) -> Span {
    tracing::info_span!("harness.member", harness.member = member, invocation.id = invocation_id)
}

/// The span of one goal iteration: `harness.goal` with `harness.goal.iteration`
/// (spec 3.1 §7.13).
///
/// # Example
///
/// ```rust
/// let span = adk_harness::goal_iteration_span(2, 10);
/// # let _ = span;
/// ```
pub fn goal_iteration_span(iteration: u32, max_iterations: u32) -> Span {
    tracing::info_span!(
        "harness.goal",
        harness.goal.iteration = iteration,
        harness.goal.max_iterations = max_iterations
    )
}

// ── Spend ──────────────────────────────────────────────────────────────────────────

/// Spend under one key for one subject (spec 3.1 §7.13).
///
/// # Example
///
/// ```rust
/// use adk_core::SpendKey;
/// use adk_harness::SpendEntry;
///
/// let entry = SpendEntry { key: SpendKey::org("app").with_agent("urn:adk:agent:app/harness"), subject: None, charges: 2, cost_micro_usd: 1_500 };
/// assert_eq!(entry.charges, 2);
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpendEntry {
    /// The key spend was reserved under: app, acting agent, vendor.
    pub key: SpendKey,
    /// The subject the run acted for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<SubjectRef>,
    /// Committed charges: model calls and payments.
    pub charges: u64,
    /// Committed cost in millionths of a US dollar.
    pub cost_micro_usd: u64,
}

/// The harness's spend, per acting agent and subject (spec 3.1 §7.13): what the CLI's
/// `/cost` and the web UI's `harness.spend` show.
///
/// # Example
///
/// ```rust
/// let report = adk_harness::SpendReport::default();
/// assert_eq!(report.cost_usd(), 0.0);
/// assert!(report.render().contains("$0.000000"));
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpendReport {
    /// Spend per key and subject, sorted by subject, then key.
    pub entries: Vec<SpendEntry>,
    /// Total committed charges.
    pub charges: u64,
    /// Total committed cost in millionths of a US dollar.
    pub cost_micro_usd: u64,
    /// Finished runs.
    pub runs: u64,
    /// Model calls of the finished runs.
    pub model_calls: u64,
    /// Tokens the models reported for the finished runs.
    pub total_tokens: u64,
    /// Prompt tokens of the finished runs' model calls (spec 3.1 §7.23).
    #[serde(default)]
    pub prompt_tokens: u64,
    /// Prompt tokens the providers served from their prompt cache (spec 3.1 §7.23).
    #[serde(default)]
    pub cached_prompt_tokens: u64,
    /// The most recent runs' prompt tokens, oldest first, at most [`SpendReport::TURNS_KEPT`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub turns: Vec<TurnTokens>,
}

/// One run's prompt tokens, cached against uncached (spec 3.1 §7.23): what `/cost` lists
/// per turn.
///
/// # Example
///
/// ```rust
/// let turn = adk_harness::TurnTokens { model_calls: 2, prompt_tokens: 9_000, cached_prompt_tokens: 8_000 };
/// assert_eq!(turn.uncached_prompt_tokens(), 1_000);
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnTokens {
    /// Model calls in the run.
    pub model_calls: u64,
    /// Prompt tokens over those calls.
    pub prompt_tokens: u64,
    /// Of them, tokens the provider read from its prompt cache.
    pub cached_prompt_tokens: u64,
}

impl TurnTokens {
    /// Prompt tokens the provider processed afresh.
    pub fn uncached_prompt_tokens(&self) -> u64 {
        self.prompt_tokens.saturating_sub(self.cached_prompt_tokens)
    }
}

impl SpendReport {
    /// How many recent runs [`turns`](Self::turns) keeps.
    pub const TURNS_KEPT: usize = 20;

    /// The total cost in US dollars.
    pub fn cost_usd(&self) -> f64 {
        self.cost_micro_usd as f64 / 1_000_000.0
    }

    /// A plain-text summary: totals, then one line per agent and subject.
    pub fn render(&self) -> String {
        let usd = |micro: u64| format!("${}.{:06}", micro / 1_000_000, micro % 1_000_000);
        let mut out = format!(
            "{} over {} runs: {} model calls, {} tokens\n",
            usd(self.cost_micro_usd),
            self.runs,
            self.model_calls,
            self.total_tokens
        );
        out.push_str(&format!(
            "prompt tokens: {} ({} cached, {} uncached)\n",
            self.prompt_tokens,
            self.cached_prompt_tokens,
            self.prompt_tokens.saturating_sub(self.cached_prompt_tokens)
        ));
        for (i, turn) in self.turns.iter().enumerate() {
            out.push_str(&format!(
                "  turn {}: {} model calls, {} prompt tokens ({} cached, {} uncached)\n",
                i + 1,
                turn.model_calls,
                turn.prompt_tokens,
                turn.cached_prompt_tokens,
                turn.uncached_prompt_tokens()
            ));
        }
        for entry in &self.entries {
            out.push_str(&format!(
                "  {} {} {}: {} charges, {}\n",
                entry.subject.as_ref().map_or("-", |subject| subject.id.as_str()),
                entry.key.agent.as_deref().unwrap_or("-"),
                entry.key.vendor.as_deref().unwrap_or("-"),
                entry.charges,
                usd(entry.cost_micro_usd)
            ));
        }
        out
    }
}

#[derive(Default)]
struct RunTotals {
    runs: u64,
    model_calls: u64,
    total_tokens: u64,
    prompt_tokens: u64,
    cached_prompt_tokens: u64,
    turns: Vec<TurnTokens>,
}

/// Charges and cost per (key, subject).
type Entries = HashMap<(SpendKey, Option<SubjectRef>), (u64, u64)>;
/// The subject ledger of each (base ledger address, subject).
type Ledgers = Vec<(usize, Option<SubjectRef>, Arc<SubjectLedger>)>;

/// The harness's spend accounts and the subject-scoped ledgers that feed them.
#[derive(Default)]
pub(crate) struct SpendAccounts {
    entries: Mutex<Entries>,
    runs: Mutex<HashMap<Option<SubjectRef>, RunTotals>>,
    ledgers: Mutex<Ledgers>,
}

impl std::fmt::Debug for SpendAccounts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpendAccounts").finish_non_exhaustive()
    }
}

fn address(ledger: &Arc<dyn SpendLedger>) -> usize {
    Arc::as_ptr(ledger).cast::<()>() as usize
}

impl SpendAccounts {
    /// `base` attributing what it commits to `subject` in these accounts; one ledger per
    /// (base, subject), reused across runs.
    fn ledger_for(
        self: &Arc<Self>,
        base: Arc<dyn SpendLedger>,
        subject: Option<SubjectRef>,
    ) -> Arc<dyn SpendLedger> {
        let mut ledgers = self.ledgers.lock().unwrap_or_else(|e| e.into_inner());
        let key = address(&base);
        if let Some((_, _, ledger)) =
            ledgers.iter().find(|(base, owner, _)| *base == key && *owner == subject)
        {
            return ledger.clone();
        }
        if ledgers.len() > 4096 {
            ledgers.clear();
        }
        let ledger = Arc::new(SubjectLedger {
            inner: base,
            subject: subject.clone(),
            accounts: Arc::downgrade(self),
            holds: Mutex::new(HashMap::new()),
        });
        ledgers.push((key, subject, ledger.clone()));
        ledger
    }

    fn is_ours(&self, ledger: &Arc<dyn SpendLedger>) -> bool {
        let key = address(ledger);
        self.ledgers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .any(|(_, _, ours)| Arc::as_ptr(ours).cast::<()>() as usize == key)
    }

    fn charge(&self, key: SpendKey, subject: Option<SubjectRef>, amount: u64) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let entry = entries.entry((key, subject)).or_insert((0, 0));
        entry.0 += 1;
        entry.1 = entry.1.saturating_add(amount);
    }

    fn finish_run(&self, subject: Option<SubjectRef>, usage: &BudgetUsage, turn: TurnTokens) {
        let mut runs = self.runs.lock().unwrap_or_else(|e| e.into_inner());
        let totals = runs.entry(subject).or_default();
        totals.runs += 1;
        totals.model_calls += usage.model_calls;
        totals.total_tokens += usage.total_tokens;
        totals.prompt_tokens += turn.prompt_tokens;
        totals.cached_prompt_tokens += turn.cached_prompt_tokens;
        if totals.turns.len() >= SpendReport::TURNS_KEPT {
            totals.turns.remove(0);
        }
        totals.turns.push(turn);
    }

    /// The report of `subject`, or of every subject.
    pub(crate) fn report(&self, subject: Option<&str>) -> SpendReport {
        let matches = |owner: &Option<SubjectRef>| {
            subject.is_none_or(|id| owner.as_ref().is_some_and(|owner| owner.id == id))
        };
        let mut entries: Vec<SpendEntry> = self
            .entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|((_, owner), _)| matches(owner))
            .map(|((key, owner), (charges, cost))| SpendEntry {
                key: key.clone(),
                subject: owner.clone(),
                charges: *charges,
                cost_micro_usd: *cost,
            })
            .collect();
        entries.sort_by(|a, b| {
            let subject = |entry: &SpendEntry| entry.subject.as_ref().map(|s| s.id.clone());
            subject(a).cmp(&subject(b)).then_with(|| a.key.to_string().cmp(&b.key.to_string()))
        });
        let mut report = SpendReport {
            charges: entries.iter().map(|entry| entry.charges).sum(),
            cost_micro_usd: entries.iter().map(|entry| entry.cost_micro_usd).sum(),
            entries,
            ..SpendReport::default()
        };
        for (owner, totals) in self.runs.lock().unwrap_or_else(|e| e.into_inner()).iter() {
            if matches(owner) {
                report.runs += totals.runs;
                report.model_calls += totals.model_calls;
                report.total_tokens += totals.total_tokens;
                report.prompt_tokens += totals.prompt_tokens;
                report.cached_prompt_tokens += totals.cached_prompt_tokens;
                report.turns.extend(totals.turns.iter().copied());
            }
        }
        let excess = report.turns.len().saturating_sub(SpendReport::TURNS_KEPT);
        report.turns.drain(..excess);
        report
    }
}

/// A ledger that forwards to the run's ledger and attributes each committed charge to the
/// subject the run acts for, since `SpendKey` names the agent but not the subject.
struct SubjectLedger {
    inner: Arc<dyn SpendLedger>,
    subject: Option<SubjectRef>,
    accounts: std::sync::Weak<SpendAccounts>,
    holds: Mutex<HashMap<ReservationId, SpendKey>>,
}

impl std::fmt::Debug for SubjectLedger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubjectLedger")
            .field("inner", &self.inner)
            .field("subject", &self.subject)
            .finish()
    }
}

#[async_trait]
impl SpendLedger for SubjectLedger {
    async fn reserve(&self, key: &SpendKey, amount_micro_usd: u64) -> Result<ReservationId> {
        let id = self.inner.reserve(key, amount_micro_usd).await?;
        let mut holds = self.holds.lock().unwrap_or_else(|e| e.into_inner());
        if holds.len() > 65_536 {
            holds.clear();
        }
        holds.insert(id.clone(), key.clone());
        Ok(id)
    }

    async fn commit(&self, id: ReservationId, actual_micro_usd: u64) -> Result<()> {
        let key = self.holds.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
        self.inner.commit(id, actual_micro_usd).await?;
        if let (Some(key), Some(accounts)) = (key, self.accounts.upgrade()) {
            accounts.charge(key, self.subject.clone(), actual_micro_usd);
        }
        Ok(())
    }

    async fn release(&self, id: ReservationId) -> Result<()> {
        self.holds.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
        self.inner.release(id).await
    }

    async fn spent(&self, key: &SpendKey, period: SpendPeriod) -> Result<u64> {
        self.inner.spent(key, period).await
    }
}

/// Puts a run's spend ledger behind the subject's ledger in `accounts`, and gives the run
/// a budget tracker so its usage is counted even without a budget (spec 3.1 §7.13).
pub(crate) fn meter_run(accounts: &Arc<SpendAccounts>, config: &mut adk_core::RunConfig) {
    if let Some(base) = config.spend_ledger.take() {
        config.spend_ledger = Some(if accounts.is_ours(&base) {
            base
        } else {
            accounts.ledger_for(base, config.acting.on_behalf_of.clone())
        });
    }
    if config.budget_tracker.is_none() {
        config.budget_tracker = Some(Arc::new(BudgetTracker::new(RunBudget::new())));
    }
}

/// Wraps the events of a harness run: the run's span around every poll, and at its end —
/// including an end by error — the `harness.spend` record (spec 3.1 §7.13, §9.3).
pub(crate) fn observe_run(
    events: EventStream,
    ctx: &Arc<dyn InvocationContext>,
    span: Span,
    accounts: Arc<SpendAccounts>,
    trails: Arc<RunTrails>,
    effort: crate::posture::Effort,
) -> EventStream {
    let config = ctx.run_config();
    let acting = ctx.acting_context();
    let audit = config.audit.clone().map(|trail| {
        HarnessAudit::new(trail, AuditActor::for_context(ctx.as_ref()), ctx.session_id())
    });
    let tracker = config.budget_tracker.clone();
    let session_key =
        (ctx.app_name().to_string(), ctx.user_id().to_string(), ctx.session_id().to_string());
    let spend_key = SpendKey::org(ctx.app_name()).with_agent(acting.agent.id.to_string());
    if let Some(trail) = &config.audit {
        trails.set(&session_key, trail.clone());
    }
    if let Some(tracker) = &tracker {
        trails.usage_begin(&session_key, tracker.clone());
    }
    #[cfg(feature = "kernel")]
    let tools = trails.tool_events(acting.agent.id.as_str(), ctx.invocation_id());
    struct Finish {
        audit: Option<HarnessAudit>,
        tracker: Option<Arc<BudgetTracker>>,
        accounts: Arc<SpendAccounts>,
        trails: Arc<RunTrails>,
        session_key: (String, String, String),
        spend_key: SpendKey,
        subject: Option<SubjectRef>,
        effort: crate::posture::Effort,
        tokens: TurnTokens,
        settled: bool,
    }
    impl Finish {
        /// Counts a model response's prompt tokens, cached and not (spec 3.1 §7.23).
        fn observe(&mut self, event: &adk_core::Event) {
            if event.llm_response.partial {
                return;
            }
            if let Some(usage) = &event.llm_response.usage_metadata {
                self.tokens.model_calls += 1;
                self.tokens.prompt_tokens += u64::try_from(usage.prompt_token_count).unwrap_or(0);
                self.tokens.cached_prompt_tokens +=
                    u64::try_from(usage.cache_read_input_token_count.unwrap_or(0)).unwrap_or(0);
            }
        }

        /// Counts the run into the spend accounts, once, and returns its usage.
        fn settle(&mut self) -> BudgetUsage {
            let usage = self.tracker.as_ref().map(|tracker| tracker.usage()).unwrap_or_default();
            if !self.settled {
                self.settled = true;
                let units =
                    self.tracker.as_ref().map(|tracker| tracker.units()).unwrap_or_default();
                self.trails.usage_settle(&self.session_key, &usage, &units);
                self.trails.clear(&self.session_key);
                self.accounts.finish_run(self.subject.clone(), &usage, self.tokens);
            }
            usage
        }

        async fn run(mut self) -> Result<()> {
            let usage = self.settle();
            let Some(audit) = &self.audit else {
                return Ok(());
            };
            let remaining = self.tracker.as_ref().map(|tracker| {
                let budget = tracker.budget();
                json!({
                    "modelCalls": budget.max_model_calls.map(|max| max.saturating_sub(usage.model_calls)),
                    "totalTokens": budget.max_total_tokens.map(|max| max.saturating_sub(usage.total_tokens)),
                    "costMicroUsd": budget.max_cost_micro_usd.map(|max| max.saturating_sub(usage.cost_micro_usd)),
                    "toolCalls": budget.max_tool_calls.map(|max| max.saturating_sub(usage.tool_calls)),
                })
            });
            audit
                .record(
                    "harness.spend",
                    json!({
                        "spendKey": { "org": self.spend_key.org, "agent": self.spend_key.agent, "subject": self.subject },
                        "modelCalls": usage.model_calls,
                        "tokens": usage.total_tokens,
                        "promptTokens": self.tokens.prompt_tokens,
                        "cachedTokens": self.tokens.cached_prompt_tokens,
                        "effort": self.effort,
                        "cost": usage.cost_micro_usd,
                        "toolCalls": usage.tool_calls,
                        "budgetRemaining": remaining,
                    }),
                )
                .await
        }
    }
    // A consumer that stops polling before the end — a person's denial or Ctrl-C in the
    // launcher — drops the stream; its model calls still count (spec 3.1 §9.2 I63). The
    // `harness.spend` record needs an await and is written only when the stream ends.
    impl Drop for Finish {
        fn drop(&mut self) {
            self.settle();
        }
    }
    let finish = Finish {
        audit,
        tracker,
        accounts,
        trails,
        session_key,
        spend_key,
        subject: acting.on_behalf_of,
        effort,
        tokens: TurnTokens::default(),
        settled: false,
    };
    #[cfg(not(feature = "kernel"))]
    let tools: Option<()> = None;
    let state = (events, Some(finish), span, tools, true);
    Box::pin(futures::stream::unfold(
        state,
        |(mut events, mut finish, span, tools, first)| async move {
            #[cfg(feature = "kernel")]
            let mut tools = tools;
            #[cfg(not(feature = "kernel"))]
            let _ = first;
            #[cfg(feature = "kernel")]
            if first && let Some(tools) = &tools {
                tools.intent().await;
            }
            let next = events.next().instrument(span.clone()).await;
            match next {
                Some(Ok(event)) => {
                    if let Some(finish) = finish.as_mut() {
                        finish.observe(&event);
                    }
                    #[cfg(feature = "kernel")]
                    if let Some(tools) = tools.as_mut() {
                        tools.observe(&event).await;
                    }
                    Some((Ok(event), (events, finish, span, tools, false)))
                }
                Some(Err(error)) => {
                    if let Some(finish) = finish
                        && let Err(audit_error) = finish.run().instrument(span.clone()).await
                    {
                        tracing::warn!(error = %audit_error, "could not record harness.spend");
                    }
                    Some((Err(error), (events, None, span, tools, false)))
                }
                None => match finish {
                    Some(finish) => match finish.run().instrument(span.clone()).await {
                        Ok(()) => None,
                        Err(error) => Some((Err(error), (events, None, span, tools, false))),
                    },
                    None => None,
                },
            }
        },
    ))
}

/// A session's usage: what its finished harness runs used, and its run in flight (spec 3.2
/// §7.3, §7.10).
#[derive(Default)]
struct SessionUsage {
    model_calls: u64,
    tool_calls: u64,
    cost_micro_usd: u64,
    total_tokens: u64,
    units: adk_core::UsageVector,
    live: Option<Arc<BudgetTracker>>,
}

/// The audit trail of each session's running harness run, so the loop guard, which sees
/// only a `ReadonlyContext`, records through it — for members too, whose calls belong to
/// the caller's session. It also counts each session's usage and keeps the loop guard's
/// last verdict, for a kernel's process view (spec 3.2 §7.3).
#[derive(Default)]
pub(crate) struct RunTrails {
    trails: Mutex<HashMap<(String, String, String), AuditTrail>>,
    usage: Mutex<HashMap<(String, String, String), SessionUsage>>,
    verdicts: Mutex<HashMap<(String, String, String), String>>,
    /// The kernel's ledger and each tool's effect class, for the rows a run writes
    /// (spec 3.2 §7.10).
    #[cfg(feature = "kernel")]
    ledger: OnceLock<(adk_kernel::proc::LateLedgerSink, Arc<HashMap<String, String>>)>,
}

impl std::fmt::Debug for RunTrails {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunTrails").finish_non_exhaustive()
    }
}

fn session_key(app: &str, user: &str, session: &str) -> (String, String, String) {
    (app.to_string(), user.to_string(), session.to_string())
}

impl RunTrails {
    /// Writes each run's `intent` and tool calls to `ledger` once a kernel binds it, with
    /// the effect class each of `tools` declares (spec 3.2 §7.10).
    #[cfg(feature = "kernel")]
    pub(crate) fn attach_ledger(
        &self,
        ledger: adk_kernel::proc::LateLedgerSink,
        tools: &[Arc<dyn adk_core::Tool>],
    ) {
        let effects = tools
            .iter()
            .filter_map(|tool| {
                let effect = serde_json::to_value(tool.effect()).ok()?.as_str()?.to_string();
                Some((tool.name().to_string(), effect))
            })
            .collect();
        let _ = self.ledger.set((ledger, Arc::new(effects)));
    }

    /// The ledger rows of a run of `agent` in `invocation`, when a kernel's ledger is bound.
    #[cfg(feature = "kernel")]
    fn tool_events(&self, agent: &str, invocation: &str) -> Option<crate::kernel::ToolEvents> {
        let (sink, effects) = self.ledger.get()?;
        crate::kernel::ToolEvents::new(sink, effects, agent, invocation)
    }

    /// Counts `tracker` as the session's run in flight.
    fn usage_begin(&self, key: &(String, String, String), tracker: Arc<BudgetTracker>) {
        let mut usage = self.usage.lock().unwrap_or_else(|e| e.into_inner());
        if usage.len() > 4096 {
            usage.clear();
        }
        usage.entry(key.clone()).or_default().live = Some(tracker);
    }

    /// Adds a finished run's `usage` and `units` to the session's total.
    fn usage_settle(
        &self,
        key: &(String, String, String),
        run: &BudgetUsage,
        units: &adk_core::UsageVector,
    ) {
        let mut usage = self.usage.lock().unwrap_or_else(|e| e.into_inner());
        let entry = usage.entry(key.clone()).or_default();
        entry.model_calls += run.model_calls;
        entry.tool_calls += run.tool_calls;
        entry.cost_micro_usd = entry.cost_micro_usd.saturating_add(run.cost_micro_usd);
        entry.total_tokens = entry.total_tokens.saturating_add(run.total_tokens);
        entry.units.merge(units);
        entry.live = None;
    }

    /// The session's model calls, tool calls, cost in micro-USD, tokens and units since the
    /// harness was built, its run in flight included; `None` before its first run.
    #[cfg(feature = "kernel")]
    pub(crate) fn session_usage(
        &self,
        app: &str,
        user: &str,
        session: &str,
    ) -> Option<adk_kernel::RunUsage> {
        let usage = self.usage.lock().unwrap_or_else(|e| e.into_inner());
        let entry = usage.get(&session_key(app, user, session))?;
        let live = entry.live.as_ref().map(|tracker| tracker.usage()).unwrap_or_default();
        let mut units = entry.units.clone();
        if let Some(tracker) = &entry.live {
            units.merge(&tracker.units());
        }
        Some(
            adk_kernel::RunUsage::new(
                entry.model_calls + live.model_calls,
                entry.tool_calls + live.tool_calls,
                entry.cost_micro_usd.saturating_add(live.cost_micro_usd),
            )
            .with_total_tokens(entry.total_tokens.saturating_add(live.total_tokens))
            .with_units(units),
        )
    }

    fn set_verdict(&self, key: (String, String, String), verdict: String) {
        let mut verdicts = self.verdicts.lock().unwrap_or_else(|e| e.into_inner());
        if verdicts.len() > 4096 {
            verdicts.clear();
        }
        verdicts.insert(key, verdict);
    }

    /// The loop guard's last verdict in the session.
    #[cfg(feature = "kernel")]
    pub(crate) fn verdict(&self, app: &str, user: &str, session: &str) -> Option<String> {
        self.verdicts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&session_key(app, user, session))
            .cloned()
    }

    fn set(&self, key: &(String, String, String), trail: AuditTrail) {
        let mut trails = self.trails.lock().unwrap_or_else(|e| e.into_inner());
        if trails.len() > 4096 {
            trails.clear();
        }
        trails.insert(key.clone(), trail);
    }

    fn clear(&self, key: &(String, String, String)) {
        self.trails.lock().unwrap_or_else(|e| e.into_inner()).remove(key);
    }

    fn get(&self, ctx: &dyn ReadonlyContext) -> Option<AuditTrail> {
        let key =
            (ctx.app_name().to_string(), ctx.user_id().to_string(), ctx.session_id().to_string());
        self.trails.lock().unwrap_or_else(|e| e.into_inner()).get(&key).cloned()
    }
}

// ── Loop guard ─────────────────────────────────────────────────────────────────────

impl Default for LoopGuard {
    /// The last 20 tool calls; a loop is 3 identical calls in a row, or 6 calls the trace
    /// analyzer counts as redundant.
    fn default() -> Self {
        Self { window: 20, max_identical_calls: 3, max_redundant: 6 }
    }
}

/// What the loop guard found in a window of calls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LoopFinding {
    pub(crate) identical: usize,
    pub(crate) redundant: usize,
}

impl LoopGuard {
    /// Analyzes the last `window` of `calls` with `adk-eval`'s `TraceAnalyzer`, each call
    /// named by its tool and canonical arguments, so a repeated pattern means repeated
    /// identical calls, not merely the same tools.
    pub(crate) fn analyze(&self, calls: &[(String, Value)]) -> LoopFinding {
        let start = calls.len().saturating_sub(self.window.max(1));
        let records: Vec<ToolCallRecord> = calls[start..]
            .iter()
            .map(|(name, args)| ToolCallRecord {
                name: adk_core::tool_call_fingerprint(name, args),
                args: args.clone(),
            })
            .collect();
        let analysis = analyzer().analyze_tool_calls(&records);
        let identical = analysis
            .diagnostics
            .iter()
            .filter(|diagnostic| matches!(diagnostic.pattern_type, TracePattern::RedundantCall))
            .map(|diagnostic| diagnostic.occurrence_count + 1)
            .max()
            .unwrap_or(usize::from(!records.is_empty()));
        let redundant = analysis.total_tool_calls.saturating_sub(analysis.useful_tool_calls);
        LoopFinding { identical, redundant }
    }

    /// Whether `finding` is a loop under this guard.
    pub(crate) fn is_loop(&self, finding: LoopFinding) -> bool {
        finding.identical >= self.max_identical_calls.max(2)
            || finding.redundant >= self.max_redundant.max(1)
    }
}

fn analyzer() -> &'static TraceAnalyzer {
    static ANALYZER: OnceLock<TraceAnalyzer> = OnceLock::new();
    ANALYZER.get_or_init(TraceAnalyzer::new)
}

/// The function calls in a model request, in order.
fn function_calls(request: &LlmRequest) -> Vec<(String, Value)> {
    request
        .contents
        .iter()
        .flat_map(|content| content.parts.iter())
        .filter_map(|part| match part {
            Part::FunctionCall { name, args, .. } => Some((name.clone(), args.clone())),
            _ => None,
        })
        .collect()
}

#[derive(Clone, Copy)]
struct Seen {
    /// Calls in the request when the invocation was first seen: earlier invocations'.
    start: usize,
    /// Calls at the last check.
    checked: usize,
}

/// The loop guard as a request observer (spec 3.1 §7.13).
pub(crate) struct LoopGuardObserver {
    guard: LoopGuard,
    mode_store: Arc<dyn ModeStore>,
    default_mode: HarnessMode,
    trails: Arc<RunTrails>,
    seen: Mutex<HashMap<(String, String), Seen>>,
}

impl LoopGuardObserver {
    pub(crate) fn new(
        guard: LoopGuard,
        mode_store: Arc<dyn ModeStore>,
        default_mode: HarnessMode,
        trails: Arc<RunTrails>,
    ) -> Self {
        Self { guard, mode_store, default_mode, trails, seen: Mutex::new(HashMap::new()) }
    }
}

#[async_trait]
impl RequestObserver for LoopGuardObserver {
    async fn before_request(
        &self,
        ctx: &Arc<dyn ReadonlyContext>,
        request: &mut LlmRequest,
    ) -> Result<()> {
        let calls = function_calls(request);
        let window = {
            let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
            if seen.len() > 4096 {
                seen.clear();
            }
            let key = (ctx.invocation_id().to_string(), ctx.agent_name().to_string());
            let entry =
                seen.entry(key).or_insert(Seen { start: calls.len(), checked: calls.len() });
            if calls.len() < entry.start {
                // History was compacted under the invocation; count from here.
                *entry = Seen { start: calls.len(), checked: calls.len() };
            }
            if calls.len() == entry.checked {
                return Ok(());
            }
            entry.checked = calls.len();
            calls[entry.start..].to_vec()
        };
        let finding = self.guard.analyze(&window);
        let looping = self.guard.is_loop(finding);
        self.trails.set_verdict(
            session_key(ctx.app_name(), ctx.user_id(), ctx.session_id()),
            format!(
                "{}: {} identical, {} redundant in the last {}",
                if looping { "loop" } else { "ok" },
                finding.identical,
                finding.redundant,
                window.len().min(self.guard.window)
            ),
        );
        if !looping {
            return Ok(());
        }
        let reason = format!(
            "the loop guard stopped the turn: {} identical tool calls in a row and {} redundant \
             calls in the last {}; the session is now read-only until a person changes the mode",
            finding.identical,
            finding.redundant,
            window.len().min(self.guard.window)
        );
        tracing::warn!(
            harness.loop.identical = finding.identical,
            harness.loop.redundant = finding.redundant,
            agent.name = %ctx.agent_name(),
            "loop detected; tightening the session to read_only"
        );
        let audit = self.trails.get(ctx.as_ref()).map(|trail| {
            HarnessAudit::new(trail, AuditActor::for_context(ctx.as_ref()), ctx.session_id())
        });
        if let Some(audit) = &audit {
            audit
                .record(
                    "harness.loop.detected",
                    json!({
                        "window": self.guard.window,
                        "identicalCalls": finding.identical,
                        "redundantCalls": finding.redundant,
                        "agent": ctx.agent_name(),
                        "action": "tighten_read_only",
                    }),
                )
                .await?;
        }
        write_mode_change(
            self.mode_store.as_ref(),
            self.default_mode,
            (ctx.app_name(), ctx.user_id(), ctx.session_id()),
            HarnessMode::ReadOnly,
            ModeSource::SystemTighten,
            None,
            audit.as_ref(),
            &[],
        )
        .await?;
        Err(AdkError::new(
            ErrorComponent::Agent,
            ErrorCategory::ResourceExhausted,
            LOOP_DETECTED_CODE,
            reason,
        ))
    }
}

/// The mode a run starts under, for its span.
pub(crate) async fn run_mode(
    store: &dyn ModeStore,
    default_mode: HarnessMode,
    ctx: &dyn ReadonlyContext,
) -> HarnessMode {
    match store.get(ctx.app_name(), ctx.user_id(), ctx.session_id()).await {
        Ok(Some(record)) => record.mode_for_invocation(ctx.invocation_id()),
        Ok(None) => default_mode,
        Err(_) => HarnessMode::ReadOnly,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn calls(names: &[(&str, Value)]) -> Vec<(String, Value)> {
        names.iter().map(|(name, args)| ((*name).to_string(), args.clone())).collect()
    }

    #[test]
    fn identical_calls_in_a_row_are_a_loop() {
        let guard = LoopGuard::default();
        let read = ("read_file", json!({ "path": "a.rs" }));
        let finding = guard.analyze(&calls(&[read.clone(), read.clone(), read]));
        assert_eq!(finding.identical, 3);
        assert!(guard.is_loop(finding));
    }

    #[test]
    fn an_edit_and_test_cycle_with_new_edits_is_not_a_loop() {
        let guard = LoopGuard::default();
        let mut cycle = Vec::new();
        for i in 0..5 {
            cycle.push(("edit_file", json!({ "path": "a.rs", "new": format!("v{i}") })));
            cycle.push(("bash", json!({ "command": "cargo test" })));
        }
        let finding = guard.analyze(&calls(&cycle));
        assert!(!guard.is_loop(finding), "{finding:?}");
    }

    #[test]
    fn a_repeated_sequence_of_identical_calls_is_redundant() {
        let guard = LoopGuard { window: 20, max_identical_calls: 10, max_redundant: 4 };
        let a = ("read_file", json!({ "path": "a" }));
        let b = ("grep", json!({ "pattern": "x" }));
        let finding = guard.analyze(&calls(&[a.clone(), b.clone(), a.clone(), b.clone(), a, b]));
        assert_eq!(finding.redundant, 4);
        assert!(guard.is_loop(finding));
    }

    #[test]
    fn spend_is_reported_per_subject() {
        let accounts = SpendAccounts::default();
        let alice = Some(SubjectRef::human("alice"));
        let key =
            SpendKey::org("app").with_agent("urn:adk:agent:app/harness").with_vendor("openai");
        accounts.charge(key.clone(), alice.clone(), 10);
        accounts.charge(key.clone(), alice, 5);
        accounts.charge(key, Some(SubjectRef::human("bob")), 7);
        let report = accounts.report(Some("alice"));
        assert_eq!((report.charges, report.cost_micro_usd), (2, 15));
        assert_eq!(accounts.report(None).cost_micro_usd, 22);
    }
}
