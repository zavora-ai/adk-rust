//! # adk-eval
//!
//! Agent evaluation framework for ADK-Rust.
//!
//! This crate provides comprehensive tools for testing and validating agent behavior,
//! enabling developers to ensure their agents perform correctly and consistently.
//!
//! ## Features
//!
//! - **Test Definitions**: Structured format for defining test cases (`.test.json`)
//! - **Trajectory Evaluation**: Validate tool call sequences
//! - **Response Quality**: Assess final output quality with multiple metrics
//! - **Multiple Criteria**: Ground truth, rubric-based, and LLM-judged evaluation
//! - **Automation**: Run evaluations programmatically or via CLI
//!
//! ## Quick Start
//!
//! ```rust,ignore
//! use adk_eval::{Evaluator, EvaluationConfig, EvaluationCriteria};
//! use std::sync::Arc;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // Create your agent
//!     let agent = create_my_agent()?;
//!
//!     // Configure evaluator
//!     let config = EvaluationConfig {
//!         criteria: EvaluationCriteria {
//!             tool_trajectory_score: Some(1.0),  // Exact tool match
//!             response_similarity: Some(0.8),    // 80% text similarity
//!             ..Default::default()
//!         },
//!         ..Default::default()
//!     };
//!
//!     let evaluator = Evaluator::new(config);
//!
//!     // Run evaluation
//!     let result = evaluator
//!         .evaluate_file(agent, "tests/my_agent.test.json")
//!         .await?;
//!
//!     assert!(result.passed, "Evaluation failed: {:?}", result.failures);
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
pub use optimizer::{OptimizationResult, OptimizerConfig, PromptOptimizer};

// New module re-exports
pub use annotation::{AnnotationRecord, AnnotationStore, HumanVerdict};
pub use baseline::{Baseline, BaselineStore, Regression};
pub use conversation_scorer::{ConversationMetrics, ConversationScorer, ConversationScorerConfig};
pub use cost_tracker::{CostMetrics, CostTracker};
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
