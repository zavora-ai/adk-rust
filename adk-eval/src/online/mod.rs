//! Online evaluation: judge a sampled fraction of production runs after they finish.
//!
//! Offline evaluation replays an eval set; online evaluation scores the runs real users
//! started. An [`OnlineEvaluator`] decides at the start of each run whether to sample it
//! ([`SamplingPolicy`]: a seeded rate plus filters by agent, request metadata and outcome),
//! buffers the sampled run's events, and once the run has finished hands a [`RunTrace`] to a
//! background worker. The worker scores it with reference-free criteria, records an
//! [`OnlineEvalRecord`] in an [`OnlineEvalStore`] and as `gen_ai.evaluation.result` telemetry,
//! and never touches the request path: the run's own stream only pays for a channel send.
//!
//! | Criterion | Source | Needs a judge |
//! |-----------|--------|---------------|
//! | `rubric_quality_score` with `rubric_config` | [`LlmJudge::evaluate_rubrics`](crate::LlmJudge::evaluate_rubrics), the user message as context | yes |
//! | `safety_score` | [`LlmJudge::evaluate_safety`](crate::LlmJudge::evaluate_safety) | yes |
//! | `hallucination_score` | [`LlmJudge::detect_hallucinations`](crate::LlmJudge::detect_hallucinations), no ground truth | yes |
//! | [`OnlineEvalConfig::min_trajectory_efficiency`] | [`TraceAnalyzer`](crate::TraceAnalyzer) over the run's tool calls | no |
//! | [`OnlineEvalConfig::thread`] | [`ConversationScorer`](crate::ConversationScorer) over the whole session: context retention, goal completion, coherence, topic drift | yes |
//!
//! Criteria that compare against an expected answer (`tool_trajectory_score`,
//! `response_similarity`, `semantic_match_score`) cannot apply to production traffic and
//! are refused when the evaluator is built.
//!
//! # Governance
//!
//! - **Opt-in.** Nothing is sampled until an application builds an evaluator and installs
//!   its [`plugin`](OnlineEvaluator::plugin) on a runner, or calls
//!   [`submit`](OnlineEvaluator::submit) itself.
//! - **Its own budget.** Every judge call counts against the evaluator's
//!   [`OnlineEvalConfig::budget`] (one tracker for the evaluator's lifetime) and, when one is
//!   set, a spend ledger under the agent [`ONLINE_EVAL_AGENT_NAME`]; a refused call stops that
//!   evaluation and records nothing. Once the budget is spent, runs are no longer sampled.
//! - **Kill switch.** The evaluator's own controls and the run's
//!   ([`RunConfig::governance`](adk_core::RunConfig::governance)) are checked when a run is
//!   sampled, before it is evaluated, and before every judge call.
//!
//! # Example
//!
//! ```rust,no_run
//! use std::sync::Arc;
//!
//! use adk_core::{Llm, RunBudget};
//! use adk_eval::EvaluationCriteria;
//! use adk_eval::online::{
//!     InMemoryOnlineEvalStore, OnlineEvalConfig, OnlineEvaluator, RunOutcome, SamplingPolicy,
//! };
//! use adk_plugin::PluginManager;
//!
//! async fn install(judge: Arc<dyn Llm>) -> adk_eval::Result<Arc<PluginManager>> {
//!     // Judge 10% of the support agent's completed runs, at most 200 judge calls.
//!     let sampling = SamplingPolicy::rate(0.10)?
//!         .with_seed(7)
//!         .agent("support")
//!         .outcome(RunOutcome::Completed);
//!     let config = OnlineEvalConfig::new(sampling)
//!         .with_criteria(EvaluationCriteria { safety_score: Some(0.9), ..Default::default() })
//!         .with_min_trajectory_efficiency(0.5)
//!         .with_budget(RunBudget::new().max_model_calls(200));
//!     let evaluator = OnlineEvaluator::builder(config)
//!         .judge(judge)
//!         .store(Arc::new(InMemoryOnlineEvalStore::new(1_000)))
//!         .start()?;
//!     // Runner::builder().plugin_manager(manager) ...
//!     Ok(Arc::new(PluginManager::new(vec![evaluator.plugin()])))
//! }
//! ```

mod evaluator;
mod sampling;
mod store;

pub use evaluator::{
    DEFAULT_ONLINE_EVAL_MAX_MODEL_CALLS, DropReason, ONLINE_EVAL_AGENT_NAME, OnlineEvalConfig,
    OnlineEvalStats, OnlineEvaluator, OnlineEvaluatorBuilder, Submission,
};
pub use sampling::{MetadataFilter, RunOutcome, RunTrace, SamplingPolicy};
pub use store::{
    CriterionSummary, InMemoryOnlineEvalStore, JsonlOnlineEvalStore, OnlineEvalQuery,
    OnlineEvalRecord, OnlineEvalStore, OnlineEvalSummary,
};

/// Telemetry attribute names of the OpenTelemetry GenAI evaluation event
/// (`gen_ai.evaluation.result`), which online evaluation emits once per scored criterion.
pub mod attributes {
    /// The event name.
    pub const GEN_AI_EVALUATION_RESULT: &str = "gen_ai.evaluation.result";
    /// The criterion's name, such as `safety` or `trajectory_efficiency`.
    pub const GEN_AI_EVALUATION_NAME: &str = "gen_ai.evaluation.name";
    /// The criterion's score, from 0.0 to 1.0.
    pub const GEN_AI_EVALUATION_SCORE_VALUE: &str = "gen_ai.evaluation.score.value";
    /// `pass` or `fail`.
    pub const GEN_AI_EVALUATION_SCORE_LABEL: &str = "gen_ai.evaluation.score.label";
    /// The judge's reasoning for a failed criterion.
    pub const GEN_AI_EVALUATION_EXPLANATION: &str = "gen_ai.evaluation.explanation";
}
