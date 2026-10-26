//! The autonomy ladder compiled to harness policy (spec 3.5 §7.11, D-7, A-I13, A-I15).
//!
//! | Level | Harness mode of the job's agent | Website tools of the job |
//! |---|---|---|
//! | Observe | read-only | read-only tools only; output goes to reports |
//! | Propose | plan | read-only tools, drafts and decision cards; everything else is proposed as a draft or an agenda item |
//! | Act with approval | ask | every other call is held through the approval service |
//! | Act, reversible only | auto | drafts, a rollback to a signed release and the edge's read-only switch run; every other call is held |
//! | Act within budget | auto | drafts and reversible acts run; every other call runs within `DeclarativePolicy` rules (`for_agent`, `at_most`) and a `RunBudget`, and is held beyond them |
//!
//! A level above Propose lets a call run only through a standing rule the owner's decision
//! created (A-I13): [`promote_job`] makes those rules from an `OwnerDecision`, and the site
//! can only lower a level ([`apply_demotion`] revokes them). The ladder policy reads each
//! job's level from the kernel's `AutonomyStore` at every call, so a demotion applies to the
//! next call with its reason.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use adk_core::principal::SubjectKind;
use adk_core::{
    ArgPredicate, DeclarativePolicy, PolicyDecision, PolicyRule, RunBudget, STATIC_TOOL_ORIGIN,
    StrictestPolicy, ToolEffect, ToolPolicy, ToolPolicyRequest,
};
use adk_kernel::charter::{
    AutonomyLadder, AutonomyStore, JobAutonomy, JobPauses, OwnerDecision, ladder_rank,
};
use async_trait::async_trait;
use awp_types::{AutonomyLevel, Charter};
use uuid::Uuid;

use super::catalog::{self, BACK_OFFICE_ORIGIN, Hold, ToolSpec, WebsitePartition};
use super::site::{SiteRef, kernel_error};
use crate::approvals::matcher::glob_matches;
use crate::approvals::{OriginPin, RuleScope, StandingDecision, StandingRule, StandingRuleStore};
use crate::mode::HarnessMode;

/// What a level decides for one call, before budgets (spec 3.5 §7.11 table).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LevelDecision {
    /// The call runs.
    Allow,
    /// The call is refused, with why.
    Deny(String),
    /// The call is held for a person, with why.
    Hold(String),
    /// The job's within-budget rules decide.
    WithinBudget,
}

/// The §7.11 table for one call: the decision `level` gives a tool whose catalogue entry is
/// `spec` (`None` for a tool outside the catalogue) and whose declared effect is `effect`.
///
/// # Example
///
/// ```rust
/// use adk_core::ToolEffect;
/// use adk_harness::website::{LevelDecision, SITE_ROLLBACK, level_decision, spec};
/// use awp_types::AutonomyLevel;
///
/// let rollback = spec(SITE_ROLLBACK);
/// assert!(matches!(level_decision(AutonomyLevel::Propose, rollback, ToolEffect::NonIdempotent), LevelDecision::Deny(_)));
/// assert!(matches!(level_decision(AutonomyLevel::ActWithApproval, rollback, ToolEffect::NonIdempotent), LevelDecision::Hold(_)));
/// assert_eq!(level_decision(AutonomyLevel::ActReversibleOnly, rollback, ToolEffect::NonIdempotent), LevelDecision::Allow);
/// ```
pub fn level_decision(
    level: AutonomyLevel,
    spec: Option<&ToolSpec>,
    effect: ToolEffect,
) -> LevelDecision {
    let effect = spec.map_or(effect, |spec| spec.effect);
    if effect == ToolEffect::ReadOnly {
        return LevelDecision::Allow;
    }
    // A decision card proposes to the owner, as a draft does; the website policy holds it.
    let draft = spec.is_some_and(|spec| spec.draft || spec.hold == Hold::Always);
    let reversible = spec.is_some_and(|spec| spec.reversible);
    match level {
        AutonomyLevel::Observe => LevelDecision::Deny(
            "the job is at Observe: it reads and reports, and changes nothing".into(),
        ),
        AutonomyLevel::Propose if draft => LevelDecision::Allow,
        AutonomyLevel::Propose => LevelDecision::Deny(
            "the job is at Propose: draft the change or propose it as an agenda item for the \
             owner"
                .into(),
        ),
        AutonomyLevel::ActWithApproval if draft => LevelDecision::Allow,
        AutonomyLevel::ActWithApproval => {
            LevelDecision::Hold("the job acts with the owner's approval of each change".into())
        }
        AutonomyLevel::ActReversibleOnly if draft || reversible => LevelDecision::Allow,
        AutonomyLevel::ActReversibleOnly => LevelDecision::Hold(
            "the job acts alone only where the act is reversible: drafts, a rollback to a \
             signed release and the edge's read-only switch"
                .into(),
        ),
        AutonomyLevel::ActWithinBudget if draft || reversible => LevelDecision::Allow,
        AutonomyLevel::ActWithinBudget => LevelDecision::WithinBudget,
        _ => LevelDecision::Deny("the job's autonomy level is unknown to this harness".into()),
    }
}

/// The harness mode of an agent whose loosest job is at `level` (spec 3.5 §7.11).
///
/// # Example
///
/// ```rust
/// use adk_harness::HarnessMode;
/// use adk_harness::website::mode_for_level;
/// use awp_types::AutonomyLevel;
///
/// assert_eq!(mode_for_level(AutonomyLevel::Observe), HarnessMode::ReadOnly);
/// assert_eq!(mode_for_level(AutonomyLevel::Propose), HarnessMode::Plan);
/// assert_eq!(mode_for_level(AutonomyLevel::ActWithApproval), HarnessMode::Ask);
/// assert_eq!(mode_for_level(AutonomyLevel::ActWithinBudget), HarnessMode::Auto);
/// ```
pub fn mode_for_level(level: AutonomyLevel) -> HarnessMode {
    match level {
        AutonomyLevel::Observe => HarnessMode::ReadOnly,
        AutonomyLevel::Propose => HarnessMode::Plan,
        AutonomyLevel::ActWithApproval => HarnessMode::Ask,
        _ => HarnessMode::Auto,
    }
}

/// A job's agent, tools and within-budget bounds (spec 3.5 §7.11, §7.20).
///
/// # Example
///
/// ```rust
/// use adk_core::ArgPredicate;
/// use adk_harness::website::JobBinding;
///
/// let finance = JobBinding::new("running-costs", "finance", ["wallet_pay"])
///     .within("wallet_pay", ArgPredicate::at_most("/amount", 5_000.0));
/// assert_eq!(finance.agent, "finance");
/// ```
#[derive(Clone, Debug, PartialEq)]
pub struct JobBinding {
    /// The job's id, as the charter's authority names it.
    pub job: String,
    /// The agent that does it.
    pub agent: String,
    /// Globs over the names of the tools that are the job's.
    pub tools: Vec<String>,
    /// The area of its work, which an incident's demotion matches (§7.20).
    pub area: Option<String>,
    /// Bounds within budget: a tool glob and a predicate its calls must satisfy to run alone.
    pub predicates: Vec<(String, ArgPredicate)>,
    /// The run budget under "act within budget".
    pub run_budget: Option<RunBudget>,
    /// The origins of tools outside the catalogue, by name; `static` by default.
    pub origins: BTreeMap<String, String>,
}

impl JobBinding {
    /// Job `job`, done by `agent` with `tools`.
    pub fn new(
        job: impl Into<String>,
        agent: impl Into<String>,
        tools: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            job: job.into(),
            agent: agent.into(),
            tools: tools.into_iter().map(Into::into).collect(),
            area: None,
            predicates: Vec::new(),
            run_budget: None,
            origins: BTreeMap::new(),
        }
    }

    /// Sets the area.
    #[must_use]
    pub fn in_area(mut self, area: impl Into<String>) -> Self {
        self.area = Some(area.into());
        self
    }

    /// Bounds the calls of `tool` (a glob) within budget by `predicate`.
    #[must_use]
    pub fn within(mut self, tool: impl Into<String>, predicate: ArgPredicate) -> Self {
        self.predicates.push((tool.into(), predicate));
        self
    }

    /// Sets the run budget.
    #[must_use]
    pub fn with_run_budget(mut self, budget: RunBudget) -> Self {
        self.run_budget = Some(budget);
        self
    }

    /// Names the origin of `tool`, a tool outside the catalogue, such as a driver's.
    #[must_use]
    pub fn with_origin(mut self, tool: impl Into<String>, origin: impl Into<String>) -> Self {
        self.origins.insert(tool.into(), origin.into());
        self
    }

    /// Whether `tool` is one of the job's.
    pub fn covers(&self, tool: &str) -> bool {
        self.tools.iter().any(|pattern| glob_matches(pattern, tool))
    }

    fn origin_of(&self, tool: &str) -> String {
        match catalog::spec(tool) {
            Some(spec)
                if spec.partition == WebsitePartition::BackOffice
                    && tool != catalog::WALLET_PAY =>
            {
                BACK_OFFICE_ORIGIN.to_string()
            }
            _ => self.origins.get(tool).cloned().unwrap_or_else(|| STATIC_TOOL_ORIGIN.to_string()),
        }
    }
}

/// Compiles a site's jobs to harness policy (spec 3.5 §7.11): the agent's mode, the ladder
/// policy, the within-budget rules, the standing rules an owner's decision creates and the
/// job pauses a guardrail breach applies (A-I15).
#[derive(Clone, Debug)]
pub struct LadderCompiler {
    site: SiteRef,
    jobs: Vec<JobBinding>,
}

impl LadderCompiler {
    /// A compiler for `site` with no jobs.
    pub fn new(site: SiteRef) -> Self {
        Self { site, jobs: Vec::new() }
    }

    /// Adds a job.
    #[must_use]
    pub fn job(mut self, binding: JobBinding) -> Self {
        self.jobs.push(binding);
        self
    }

    /// Adds `bindings` with the limits `charter` gives their jobs: a `spend_cap` bounds the
    /// `/amount` of each of the job's financial calls, and `actions_per_day` caps the tool
    /// calls of a run.
    #[must_use]
    pub fn with_charter(
        mut self,
        charter: &Charter,
        bindings: impl IntoIterator<Item = JobBinding>,
    ) -> Self {
        for mut binding in bindings {
            if let Some(authority) = charter.authority.iter().find(|a| a.job == binding.job) {
                if let Some(cap) = authority.limits.spend_cap {
                    let financial: Vec<&str> = catalog::CATALOG
                        .iter()
                        .filter(|spec| spec.class == adk_kernel::drivers::EffectClass::Financial)
                        .filter(|spec| binding.covers(spec.name))
                        .map(|spec| spec.name)
                        .collect();
                    for tool in financial {
                        binding
                            .predicates
                            .push((tool.to_string(), ArgPredicate::at_most("/amount", cap as f64)));
                    }
                }
                if let Some(actions) = authority.limits.actions_per_day {
                    binding.run_budget = Some(
                        binding.run_budget.unwrap_or_default().max_tool_calls(u64::from(actions)),
                    );
                }
            }
            self.jobs.push(binding);
        }
        self
    }

    /// Pins the origin of each of `tools` outside the catalogue — a driver's, the wallet's —
    /// in every job that covers it, so the standing rules a promotion grants admit exactly
    /// the publisher the agent is given (spec 3.5 §7.11, A-I13).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let tools = email_tools(outbox, Side::BackOffice);
    /// let compiler = LadderCompiler::new(site)
    ///     .with_charter(&charter, bindings)
    ///     .with_tool_origins(&tools);
    /// ```
    #[must_use]
    pub fn with_tool_origins(mut self, tools: &[Arc<dyn adk_core::Tool>]) -> Self {
        for binding in &mut self.jobs {
            for tool in tools {
                let name = tool.name();
                let catalogued = catalog::spec(name).is_some_and(|spec| {
                    spec.partition == WebsitePartition::BackOffice && name != catalog::WALLET_PAY
                });
                if !catalogued && binding.covers(name) {
                    binding.origins.insert(name.to_string(), tool.origin().to_string());
                }
            }
        }
        self
    }

    /// The site.
    pub fn site(&self) -> &SiteRef {
        &self.site
    }

    /// The jobs.
    pub fn jobs(&self) -> &[JobBinding] {
        &self.jobs
    }

    /// The binding of `job`.
    pub fn binding(&self, job: &str) -> Option<&JobBinding> {
        self.jobs.iter().find(|binding| binding.job == job)
    }

    /// The harness mode of `agent` given its jobs' `levels` (spec 3.5 §7.11): the mode of
    /// its loosest job; Propose's plan mode for an agent with no job.
    pub fn mode_for_agent(
        &self,
        agent: &str,
        levels: &BTreeMap<String, AutonomyLevel>,
    ) -> HarnessMode {
        self.jobs
            .iter()
            .filter(|binding| binding.agent == agent)
            .map(|binding| levels.get(&binding.job).copied().unwrap_or(AutonomyLevel::Propose))
            .max_by_key(|level| ladder_rank(*level))
            .map_or(HarnessMode::Plan, mode_for_level)
    }

    /// The run budget of `agent` given its jobs' `levels`: the tightest tool-call cap of its
    /// jobs acting within budget.
    pub fn budget_for_agent(
        &self,
        agent: &str,
        levels: &BTreeMap<String, AutonomyLevel>,
    ) -> Option<RunBudget> {
        self.jobs
            .iter()
            .filter(|binding| {
                binding.agent == agent
                    && levels.get(&binding.job) == Some(&AutonomyLevel::ActWithinBudget)
            })
            .filter_map(|binding| binding.run_budget.clone())
            .min_by_key(|budget| budget.max_tool_calls.unwrap_or(u64::MAX))
    }

    /// The within-budget rules of `job` (spec 3.5 §7.11): for its agent's principal, a call
    /// of a bounded tool runs when every bound holds and is held beyond them; any other tool
    /// of the job runs.
    pub fn declarative(&self, job: &str) -> Option<DeclarativePolicy> {
        let binding = self.binding(job)?;
        let principal = self.site.principal(&binding.agent);
        let mut builder =
            DeclarativePolicy::builder().name(format!("adk-harness.website.budget:{job}"));
        let mut bounded: BTreeMap<&str, Vec<&ArgPredicate>> = BTreeMap::new();
        for (tool, predicate) in &binding.predicates {
            bounded.entry(tool.as_str()).or_default().push(predicate);
        }
        for (tool, predicates) in bounded {
            let mut rule = PolicyRule::allow(tool).for_agent(principal.clone());
            for predicate in predicates {
                rule = rule.when(predicate.clone());
            }
            builder = builder.rule(rule).rule(
                PolicyRule::require_approval(
                    tool,
                    format!("beyond job {job}'s budget; the owner decides this one"),
                )
                .for_agent(principal.clone()),
            );
        }
        for tool in &binding.tools {
            builder = builder.rule(PolicyRule::allow(tool.clone()).for_agent(principal.clone()));
        }
        Some(builder.build())
    }

    /// The standing rules `job` holds at `level` (spec 3.5 §7.11, A-I13): the tools of the
    /// job an owner's decision lets past the harness's mode.
    ///
    /// | Level | Tools |
    /// |---|---|
    /// | Observe, Propose | none: drafts and decision cards need no rule |
    /// | Act with approval | every tool of the job that is not read-only, a draft or a decision card; the ladder still holds each of their calls for the owner, so the rules record what the owner granted |
    /// | Act, reversible only | its reversible acts: a rollback to a signed release and the edge's read-only switch (A-I11) |
    /// | Act within budget | every tool of the job that is not read-only, a draft or a decision card |
    pub fn rule_tools(&self, job: &str, level: AutonomyLevel) -> Vec<String> {
        let Some(binding) = self.binding(job) else { return Vec::new() };
        let every =
            matches!(level, AutonomyLevel::ActWithApproval | AutonomyLevel::ActWithinBudget);
        let mut names: BTreeSet<String> = BTreeSet::new();
        for pattern in &binding.tools {
            let catalogued: Vec<&ToolSpec> = catalog::CATALOG
                .iter()
                .filter(|spec| {
                    spec.partition == WebsitePartition::BackOffice
                        && glob_matches(pattern, spec.name)
                })
                .collect();
            if catalogued.is_empty() && every {
                names.insert(pattern.clone());
            }
            for spec in catalogued {
                let needs_rule =
                    spec.effect != ToolEffect::ReadOnly && !spec.draft && spec.hold != Hold::Always;
                let allowed = match level {
                    AutonomyLevel::ActReversibleOnly => spec.reversible,
                    _ => every,
                };
                if needs_rule && allowed {
                    names.insert(spec.name.to_string());
                }
            }
        }
        names.into_iter().collect()
    }

    fn rule_id(&self, job: &str, tool: &str, kind: SubjectKind) -> String {
        let kind = match kind {
            SubjectKind::Human => "human",
            SubjectKind::Service => "service",
            _ => "other",
        };
        format!("website:{}:{job}:{tool}:{kind}", self.site.name)
    }

    /// The standing rules that let `job` act at `level`, created by the owner's `decision`
    /// for the runs of `user` (spec 3.5 §7.11, A-I13): one per tool and subject kind (the
    /// owner's own turns and the kernel's scheduled ones), pinned to the tool's origin and
    /// the job's agent.
    pub fn standing_rules(
        &self,
        job: &str,
        level: AutonomyLevel,
        decision: &OwnerDecision,
        user: &str,
    ) -> Vec<StandingRule> {
        let Some(binding) = self.binding(job) else { return Vec::new() };
        let mut rules = Vec::new();
        for tool in self.rule_tools(job, level) {
            for kind in [SubjectKind::Human, SubjectKind::Service] {
                rules.push(StandingRule {
                    id: self.rule_id(job, &tool, kind),
                    scope: RuleScope::User {
                        app_name: self.site.name.clone(),
                        user_id: user.to_string(),
                        subject_kind: kind,
                    },
                    tool: tool.clone(),
                    origin: OriginPin::AnyDeclaration { source: binding.origin_of(&tool) },
                    agent: Some(self.site.principal(&binding.agent)),
                    predicates: Vec::new(),
                    decision: StandingDecision::Allow,
                    created_by: decision.owner.clone(),
                    created_at: decision.at,
                    expires_at: None,
                    label: format!(
                        "{tool} by {} for job {job} at {level:?} (owner decision {})",
                        binding.agent, decision.decision_id
                    ),
                    source_fingerprint: None,
                });
            }
        }
        rules
    }

    /// Job pauses with every job mapped to its agent, by name and principal (A-I15).
    pub fn job_pauses(&self) -> JobPauses {
        self.jobs.iter().fold(JobPauses::new(), |pauses, binding| {
            pauses
                .with_agent(binding.job.clone(), binding.agent.clone())
                .with_agent(binding.job.clone(), self.site.principal(&binding.agent))
        })
    }

    /// The ladder policy over the jobs' levels in `store`, strictest with `pauses` (spec 3.5
    /// §7.11, A-I15): put it on every back-office agent.
    pub fn policy(
        &self,
        store: Arc<dyn AutonomyStore>,
        pauses: Arc<JobPauses>,
    ) -> Arc<dyn ToolPolicy> {
        let budgets = self
            .jobs
            .iter()
            .filter_map(|binding| Some((binding.job.clone(), self.declarative(&binding.job)?)))
            .collect();
        Arc::new(StrictestPolicy::new(vec![
            Arc::new(LadderPolicy { compiler: self.clone(), store, budgets }),
            pauses,
        ]))
    }
}

/// Decides each call by its job's level, read at every call (spec 3.5 §7.11).
struct LadderPolicy {
    compiler: LadderCompiler,
    store: Arc<dyn AutonomyStore>,
    budgets: BTreeMap<String, DeclarativePolicy>,
}

impl std::fmt::Debug for LadderPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LadderPolicy")
            .field("site", &self.compiler.site.name)
            .finish_non_exhaustive()
    }
}

impl LadderPolicy {
    fn agent_of(&self, request: &ToolPolicyRequest) -> Option<String> {
        let id = request.acting.as_ref().map(|acting| acting.agent.id.as_str().to_string());
        self.compiler
            .jobs
            .iter()
            .find(|binding| {
                binding.agent == request.agent_name
                    || id.as_deref() == Some(self.compiler.site.principal(&binding.agent).as_str())
            })
            .map(|binding| binding.agent.clone())
    }

    async fn level(&self, job: &str) -> (AutonomyLevel, String) {
        match self.store.get(self.compiler.site.id, job).await {
            Ok(Some(record)) => {
                let why = match self.store.history(self.compiler.site.id, job).await {
                    Ok(history) => {
                        history.last().map(|change| change.reason.describe()).unwrap_or_default()
                    }
                    Err(_) => String::new(),
                };
                (record.level, why)
            }
            Ok(None) => (
                AutonomyLevel::Propose,
                "no level recorded; every job proposes until the charter says otherwise".into(),
            ),
            Err(error) => {
                tracing::warn!(error = %error, website.job = job, "autonomy store unavailable; deciding as Observe");
                (AutonomyLevel::Observe, "the autonomy store is unavailable".into())
            }
        }
    }
}

#[async_trait]
impl ToolPolicy for LadderPolicy {
    async fn evaluate(&self, request: &ToolPolicyRequest) -> PolicyDecision {
        let spec = catalog::spec(&request.tool_name)
            .filter(|spec| spec.partition == WebsitePartition::BackOffice);
        let Some(agent) = self.agent_of(request) else {
            return PolicyDecision::Allow;
        };
        let jobs: Vec<&JobBinding> = self
            .compiler
            .jobs
            .iter()
            .filter(|binding| binding.agent == agent && binding.covers(&request.tool_name))
            .collect();
        if jobs.is_empty() {
            return match spec {
                Some(spec) if spec.effect != ToolEffect::ReadOnly => PolicyDecision::deny(format!(
                    "no job of {agent} covers {}; the charter gives it no authority for it",
                    request.tool_name
                )),
                _ => PolicyDecision::Allow,
            };
        }
        // A call that names its job is decided at that job's level; otherwise at the
        // strictest level of the jobs it could belong to. A promotion request's `job` is the
        // job it asks about, not the one asking.
        let named = request
            .args
            .get("job")
            .and_then(serde_json::Value::as_str)
            .filter(|_| request.tool_name != catalog::JOB_PROMOTION_REQUEST);
        let mut decided: Option<(AutonomyLevel, &JobBinding, String)> = None;
        for binding in jobs.iter().filter(|b| named.is_none_or(|job| b.job == job)) {
            let (level, why) = self.level(&binding.job).await;
            if decided.as_ref().is_none_or(|(held, _, _)| ladder_rank(level) < ladder_rank(*held)) {
                decided = Some((level, binding, why));
            }
        }
        let Some((level, binding, why)) = decided else {
            return PolicyDecision::deny(format!(
                "job {} is not one of {agent}'s jobs that cover {}",
                named.unwrap_or_default(),
                request.tool_name
            ));
        };
        let context = |reason: String| {
            let why = if why.is_empty() { String::new() } else { format!(" ({why})") };
            format!("{reason}: job {} is at {level:?}{why}", binding.job)
        };
        match level_decision(level, spec, request.effect) {
            LevelDecision::Allow => PolicyDecision::Allow,
            LevelDecision::Deny(reason) => PolicyDecision::deny(context(reason)),
            LevelDecision::Hold(reason) => PolicyDecision::require_approval(context(reason)),
            LevelDecision::WithinBudget => match self.budgets.get(&binding.job) {
                Some(rules) => rules.decide(request),
                None => PolicyDecision::require_approval(context("no budget is compiled".into())),
            },
        }
    }

    fn policy_ref(&self) -> Option<String> {
        Some(format!("adk-harness.website.ladder:{}", self.compiler.site.name))
    }
}

/// A job's promotion: its new level and the standing rules the owner's decision created.
#[derive(Clone, Debug, PartialEq)]
pub struct Promotion {
    /// The job's record after the promotion.
    pub job: JobAutonomy,
    /// The standing rules granted.
    pub rules: Vec<StandingRule>,
}

/// Promotes `job` one rung by the owner's `decision` and grants the standing rules its new
/// level holds ([`LadderCompiler::rule_tools`]), each created by the owner, revoking those
/// of the old level the new one does not hold (spec 3.5 §7.11, D-7, A-I13). The kernel's
/// ladder refuses a decision no person made, and one by a person who is not an owner when it
/// knows the site's charters; the site itself can never call this to raise its own level.
///
/// # Errors
///
/// The ladder's refusal, as `Forbidden`; the rule store's error.
pub async fn promote_job(
    ladder: &AutonomyLadder,
    compiler: &LadderCompiler,
    rules: &dyn StandingRuleStore,
    job: &str,
    decision: &OwnerDecision,
    user: &str,
) -> adk_core::Result<Promotion> {
    let record = ladder.promote(compiler.site.id, job, decision).await.map_err(kernel_error)?;
    let mut granted = Vec::new();
    for rule in compiler.standing_rules(job, record.level, decision, user) {
        granted.push(rules.grant(rule).await?);
    }
    apply_demotion(compiler, rules, job, record.level).await?;
    tracing::info!(website.job = job, website.level = ?record.level, decision = %decision.decision_id, "website.job.promoted");
    Ok(Promotion { job: record, rules: granted })
}

/// Revokes the standing rules of `job` that its `level` no longer warrants (spec 3.5
/// §7.11): call it after the kernel's ladder lowers a job, so the harness holds what the job
/// may no longer do alone. Returns the ids revoked.
///
/// # Errors
///
/// The rule store's error.
pub async fn apply_demotion(
    compiler: &LadderCompiler,
    rules: &dyn StandingRuleStore,
    job: &str,
    level: AutonomyLevel,
) -> adk_core::Result<Vec<String>> {
    let keep: BTreeSet<String> = compiler.rule_tools(job, level).into_iter().collect();
    let Some(binding) = compiler.binding(job) else { return Ok(Vec::new()) };
    let mut revoked = Vec::new();
    let candidates: BTreeSet<String> =
        compiler.rule_tools(job, AutonomyLevel::ActWithinBudget).into_iter().collect();
    for tool in candidates.difference(&keep) {
        for kind in [SubjectKind::Human, SubjectKind::Service] {
            let id = compiler.rule_id(&binding.job, tool, kind);
            if rules.revoke(&compiler.site.name, &id).await? {
                revoked.push(id);
            }
        }
    }
    Ok(revoked)
}

/// The levels of every job of the compiler's site in `store` (Propose for a job with none).
///
/// # Errors
///
/// The store's error.
pub async fn job_levels(
    compiler: &LadderCompiler,
    store: &dyn AutonomyStore,
    site: Uuid,
) -> adk_core::Result<BTreeMap<String, AutonomyLevel>> {
    let mut levels = BTreeMap::new();
    for binding in &compiler.jobs {
        let level = store
            .get(site, &binding.job)
            .await
            .map_err(kernel_error)?
            .map_or(AutonomyLevel::Propose, |r| r.level);
        levels.insert(binding.job.clone(), level);
    }
    Ok(levels)
}
