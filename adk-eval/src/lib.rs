//! # adk-eval
//!
//! Agent evaluation framework for ADK-Rust.
//!
//! This crate runs agents against eval sets and scores their tool calls and responses, so
//! agent behaviour can be checked in tests and CI.
//!
//! ## Features
//!
//! - **Test definitions**: JSON eval sets (`.test.json`) with multi-turn conversations
//! - **Trajectory evaluation**: tool call sequences with ordered or unordered, exact or subset
//!   argument matching
//! - **Response quality**: text similarity (exact, contains, Levenshtein, Jaccard, ROUGE)
//! - **LLM-judged criteria**: semantic match, rubrics, safety, and hallucination checks, which
//!   fail closed when they cannot be judged
//! - **Analysis**: structured judge, cost and latency tracking, trace analysis, regression
//!   baselines, human annotation, test generation, and conversation metrics
//! - **Ledger statistics**: per-dimension median/MAD baselines and drift detection over a
//!   content-free activity ledger ([`ledger`]), the statistics behind `cargo adk eval --ledger`
//!
//! | Feature | Enables |
//! |---------|---------|
//! | `embedding` | `EmbeddingScorer` |
//! | `ci-helpers` | `JunitReporter` |
//! | `statistics` | `AbComparator` |
//! | `vertex-eval` | Vertex AI Gen AI Evaluation Service bridge |
//! | `personas` | Simulated users for multi-turn evaluation |
//!
//! ## Quick start
//!
//! ```rust,no_run
//! use std::sync::Arc;
//!
//! use adk_core::Agent;
//! use adk_eval::{EvaluationConfig, EvaluationCriteria, Evaluator};
//!
//! async fn evaluate(agent: Arc<dyn Agent>) -> adk_eval::Result<()> {
//!     let criteria = EvaluationCriteria {
//!         tool_trajectory_score: Some(1.0), // exact tool match
//!         response_similarity: Some(0.8),   // 80% text similarity
//!         ..Default::default()
//!     };
//!     let evaluator = Evaluator::new(EvaluationConfig::with_criteria(criteria));
//!
//!     let report = evaluator.evaluate_file(agent, "tests/my_agent.test.json").await?;
//!     assert!(report.all_passed(), "{}", report.format_summary());
//!     Ok(())
//! }
//! ```

pub mod criteria;
pub mod error;
pub mod evaluator;
pub mod llm_judge;
pub mod report;
pub mod schema;
pub mod scoring;

#[cfg(feature = "personas")]
pub mod personas;

pub mod optimizer;

// New unconditional modules
pub mod annotation;
pub mod baseline;
pub mod conversation_scorer;
pub mod cost_tracker;
pub mod ledger;
pub mod pricing;
pub mod structured_judge;
pub mod test_generator;
pub mod trace_analyzer;

// New feature-gated modules
#[cfg(feature = "embedding")]
pub mod embedding_scorer;

#[cfg(feature = "ci-helpers")]
pub mod junit_reporter;

#[cfg(feature = "statistics")]
pub mod ab_comparator;

#[cfg(feature = "vertex-eval")]
pub mod vertex;

// Re-exports
pub use criteria::{
    EvaluationCriteria, ResponseMatchConfig, Rubric, RubricConfig, ToolTrajectoryConfig,
};
pub use error::{EvalError, Result};
pub use evaluator::{EvaluationConfig, Evaluator};
pub use llm_judge::{
    LlmJudge, LlmJudgeConfig, RubricEvaluationResult, RubricScore, SemanticMatchResult,
};
pub use report::{EvaluationReport, EvaluationResult, Failure, TestCaseResult};
pub use schema::{EvalCase, EvalSet, IntermediateData, SessionInput, TestFile, ToolUse, Turn};
pub use scoring::{ResponseScorer, ToolTrajectoryScorer};

// Optimizer re-exports
pub use optimizer::{
    AgentCandidates, InstructionCandidates, OptimizationResult, OptimizerConfig, PromptOptimizer,
};

// New module re-exports
pub use annotation::{AnnotationRecord, AnnotationStore, HumanVerdict};
pub use baseline::{Baseline, BaselineStore, Regression};
pub use conversation_scorer::{ConversationMetrics, ConversationScorer, ConversationScorerConfig};
pub use cost_tracker::{CostMetrics, CostTracker};
pub use ledger::{
    DailyValue, DayClass, DeviationConfig, DeviationReport, DriftState, LedgerAssessment, LedgerRow,
};
pub use pricing::ModelPricing;
pub use structured_judge::{
    JudgeRubric, ScalePoint, StructuredJudge, StructuredJudgeConfig, StructuredVerdict, Verdict,
};
pub use test_generator::{EvalCaseMetadata, GeneratorConfig, TestGenerator};
pub use trace_analyzer::{
    ToolCallRecord, TraceAnalysis, TraceAnalyzer, TraceDiagnostic, TracePattern,
};

#[cfg(feature = "embedding")]
pub use embedding_scorer::EmbeddingScorer;

#[cfg(feature = "ci-helpers")]
pub use junit_reporter::JunitReporter;

#[cfg(feature = "statistics")]
pub use ab_comparator::AbComparator;

#[cfg(feature = "vertex-eval")]
pub use vertex::{
    AutoraterConfig, PointwiseMetricResult, PointwiseMetricSpec, TrajectoryMetric,
    VertexEvalClient, VertexEvalConfig, VertexEvalJudge,
};

/// Prelude for convenient imports
pub mod prelude {
    pub use crate::criteria::{
        EvaluationCriteria, ResponseMatchConfig, Rubric, RubricConfig, ToolTrajectoryConfig,
    };
    pub use crate::error::{EvalError, Result};
    pub use crate::evaluator::{EvaluationConfig, Evaluator};
    pub use crate::llm_judge::{
        LlmJudge, LlmJudgeConfig, RubricEvaluationResult, SemanticMatchResult,
    };
    pub use crate::report::{EvaluationReport, EvaluationResult, Failure, TestCaseResult};
    pub use crate::schema::{EvalCase, EvalSet, TestFile, ToolUse, Turn};
}
