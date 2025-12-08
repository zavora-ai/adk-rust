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
