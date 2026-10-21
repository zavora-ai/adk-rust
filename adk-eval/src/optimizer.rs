//! Prompt optimization engine.
//!
//! Iteratively improves an agent's system instructions using an optimizer LLM
//! and an evaluation set. Used by the `adk optimize` CLI command.
//!
//! # Overview
//!
//! The [`PromptOptimizer`] runs an optimization loop:
//! 1. Evaluate the agent against the eval set to get a baseline score
//! 2. If the score already meets the target threshold, report "no optimization needed"
//! 3. Otherwise, ask the optimizer LLM to propose improved instructions
//! 4. Apply the best improvement and re-evaluate
//! 5. Repeat until max iterations or target threshold is reached
//! 6. Write the best-performing instructions to the output file
//!
//! The optimizer rewrites an agent's instruction, so it needs the instruction and a way to
//! build the agent from a candidate: an [`AgentCandidates`], usually an
//! [`InstructionCandidates`] over the agent's builder. `Arc<dyn Agent>` exposes neither.
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_eval::optimizer::{InstructionCandidates, OptimizerConfig, PromptOptimizer};
//! use std::sync::Arc;
//!
//! let candidates = InstructionCandidates::new("Answer questions", move |instruction| {
//!     Ok(Arc::new(
//!         LlmAgentBuilder::new("tutor").model(model.clone()).instruction(instruction).build()?,
//!     ))
//! });
//! let optimizer = PromptOptimizer::new(optimizer_llm, evaluator, OptimizerConfig::default());
//! let result = optimizer.optimize_candidates(&candidates, &eval_set).await?;
//! println!("{} -> {}: {}", result.initial_score, result.final_score, result.best_instructions);
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use futures::StreamExt;
use tracing::{info, warn};

use adk_core::types::Content;
use adk_core::{Agent, Llm, LlmRequest};

use crate::error::{EvalError, Result};
use crate::evaluator::Evaluator;
use crate::schema::EvalSet;

/// Configuration for the prompt optimization loop.
#[derive(Debug, Clone)]
pub struct OptimizerConfig {
    /// Maximum number of optimization iterations (default: 5).
    pub max_iterations: u32,
    /// Target evaluation score threshold (default: 0.9).
    /// Optimization stops early if this score is reached.
    pub target_threshold: f64,
    /// Path to write the best-performing instructions.
    pub output_path: PathBuf,
}

impl Default for OptimizerConfig {
    fn default() -> Self {
        Self {
            max_iterations: 5,
            target_threshold: 0.9,
            output_path: PathBuf::from("optimized_instructions.txt"),
        }
    }
}

/// Result of a prompt optimization run.
#[derive(Debug, Clone)]
pub struct OptimizationResult {
    /// Evaluation score before optimization.
    pub initial_score: f64,
    /// Best evaluation score achieved.
    pub final_score: f64,
    /// Number of iterations actually executed.
    pub iterations_run: u32,
    /// The best-performing system instructions.
    pub best_instructions: String,
}

/// What the optimizer rewrites: the instruction under test and the agent to evaluate under
/// a candidate instruction.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
///
/// use adk_core::Agent;
/// use adk_eval::optimizer::AgentCandidates;
///
/// struct Fixed(Arc<dyn Agent>);
///
/// impl AgentCandidates for Fixed {
///     fn instruction(&self) -> String {
///         "Answer questions".to_string()
///     }
///
///     fn build(&self, _instruction: &str) -> adk_core::Result<Arc<dyn Agent>> {
///         Ok(self.0.clone())
///     }
/// }
/// ```
pub trait AgentCandidates: Send + Sync {
    /// The instruction the optimizer starts from.
    fn instruction(&self) -> String;

    /// The agent to evaluate under `instruction`.
    ///
    /// # Errors
    ///
    /// Returns the agent's build error.
    fn build(&self, instruction: &str) -> adk_core::Result<Arc<dyn Agent>>;
}

/// Builds the agent to evaluate under an instruction.
type BuildAgent = dyn Fn(&str) -> adk_core::Result<Arc<dyn Agent>> + Send + Sync;

/// [`AgentCandidates`] over a closure that builds the agent from an instruction.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
///
/// use adk_eval::optimizer::{AgentCandidates, InstructionCandidates};
/// use adk_model::{ScriptedLlm, ScriptedResponse};
///
/// let candidates = InstructionCandidates::new("Answer questions", |instruction| {
///     let reply = if instruction.contains("capital") { "Paris" } else { "Not sure" };
///     let model = Arc::new(ScriptedLlm::new("agent", vec![ScriptedResponse::text(reply)]));
///     # let _ = model;
///     # Err::<Arc<dyn adk_core::Agent>, _>(adk_core::AdkError::config("built elsewhere"))
///     // Ok(Arc::new(LlmAgentBuilder::new("tutor").model(model).instruction(instruction).build()?))
/// });
/// assert_eq!(candidates.instruction(), "Answer questions");
/// ```
pub struct InstructionCandidates {
    instruction: String,
    build: Box<BuildAgent>,
}

impl InstructionCandidates {
    /// Candidates starting from `instruction`, each built by `build`.
    pub fn new(
        instruction: impl Into<String>,
        build: impl Fn(&str) -> adk_core::Result<Arc<dyn Agent>> + Send + Sync + 'static,
    ) -> Self {
        Self { instruction: instruction.into(), build: Box::new(build) }
    }
}

impl AgentCandidates for InstructionCandidates {
    fn instruction(&self) -> String {
        self.instruction.clone()
    }

    fn build(&self, instruction: &str) -> adk_core::Result<Arc<dyn Agent>> {
        (self.build)(instruction)
    }
}

impl std::fmt::Debug for InstructionCandidates {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InstructionCandidates").field("instruction", &self.instruction).finish()
    }
}

/// Iteratively improves an agent's system instructions using an optimizer LLM
/// and an evaluation set.
///
/// The optimizer runs a loop of evaluate → propose improvements → apply best →
/// repeat, logging progress via `tracing` at each iteration.
pub struct PromptOptimizer {
    optimizer_llm: Arc<dyn Llm>,
    evaluator: Evaluator,
    config: OptimizerConfig,
}

impl PromptOptimizer {
    /// Create a new prompt optimizer.
    ///
    /// # Arguments
    ///
    /// * `optimizer_llm` - The LLM used to propose instruction improvements
    ///   (separate from the agent's own LLM).
    /// * `evaluator` - The evaluator used to score the agent against the eval set.
    /// * `config` - Optimization configuration (max iterations, target threshold, output path).
    pub fn new(optimizer_llm: Arc<dyn Llm>, evaluator: Evaluator, config: OptimizerConfig) -> Self {
        Self { optimizer_llm, evaluator, config }
    }

    /// Evaluates `agent` against the eval set.
    ///
    /// An `Arc<dyn Agent>` carries neither its instruction nor a way to rebuild it, so this
    /// method can only score the agent as given: when the baseline meets the threshold it
    /// writes the agent's description to the output file and returns; otherwise it fails,
    /// since it cannot rewrite the instruction. [`optimize_candidates`](Self::optimize_candidates)
    /// is the optimization loop.
    ///
    /// # Errors
    ///
    /// Returns [`EvalError::ConfigError`] when the eval set has no cases or the baseline is
    /// below the target threshold, or the evaluator's error.
    pub async fn optimize(
        &self,
        agent: Arc<dyn Agent>,
        eval_set: &EvalSet,
    ) -> Result<OptimizationResult> {
        let base_path = std::path::Path::new(".");
        let cases = eval_set.get_all_cases(base_path)?;
        if cases.is_empty() {
            return Err(EvalError::ConfigError("eval set contains no cases".to_string()));
        }
        let initial_score = self.evaluate_agent(agent.clone(), eval_set).await?;
        info!(iteration = 0, score = initial_score, "initial evaluation complete");
        if initial_score < self.config.target_threshold {
            return Err(EvalError::ConfigError(format!(
                "the agent scored {initial_score:.2} against a target of {:.2}, and \
                 PromptOptimizer::optimize cannot rewrite the instruction of an Arc<dyn Agent>. \
                 Call optimize_candidates with InstructionCandidates::new(instruction, |candidate| \
                 build the agent) so each proposal is evaluated",
                self.config.target_threshold
            )));
        }
        info!(
            score = initial_score,
            threshold = self.config.target_threshold,
            "no optimization needed — initial score meets target threshold"
        );
        let instructions = agent.description().to_string();
        self.write_output(&instructions)?;
        Ok(OptimizationResult {
            initial_score,
            final_score: initial_score,
            iterations_run: 0,
            best_instructions: instructions,
        })
    }

    /// Runs the optimization loop over `candidates`.
    ///
    /// Evaluates the agent built from the starting instruction, asks the optimizer LLM for
    /// an improved instruction, evaluates the agent built from it, keeps the best, and
    /// repeats until the target threshold is met or max iterations are exhausted. On
    /// completion, writes the best-performing instruction to the configured output file.
    ///
    /// # Errors
    ///
    /// Returns [`EvalError::ConfigError`] when the eval set has no cases, the candidate's
    /// build error, the evaluator's error, or [`EvalError::ExecutionError`] when the
    /// optimizer LLM fails or answers nothing.
    pub async fn optimize_candidates(
        &self,
        candidates: &dyn AgentCandidates,
        eval_set: &EvalSet,
    ) -> Result<OptimizationResult> {
        let base_path = std::path::Path::new(".");
        let cases = eval_set.get_all_cases(base_path)?;

        if cases.is_empty() {
            return Err(EvalError::ConfigError("eval set contains no cases".to_string()));
        }

        let mut current_instructions = candidates.instruction();
        let agent = candidates.build(&current_instructions)?;

        // Run initial evaluation
        let initial_score = self.evaluate_agent(agent, eval_set).await?;
        info!(iteration = 0, score = initial_score, "initial evaluation complete");

        // Check if initial score already meets threshold
        if initial_score >= self.config.target_threshold {
            info!(
                score = initial_score,
                threshold = self.config.target_threshold,
                "no optimization needed — initial score meets target threshold"
            );

            self.write_output(&current_instructions)?;

            return Ok(OptimizationResult {
                initial_score,
                final_score: initial_score,
                iterations_run: 0,
                best_instructions: current_instructions,
            });
        }

        let mut best_score = initial_score;
        let mut best_instructions = current_instructions.clone();
        let mut iterations_run = 0;

        for iteration in 1..=self.config.max_iterations {
            iterations_run = iteration;

            // Propose improved instructions via optimizer LLM
            let proposed = self.propose_improvements(&current_instructions, best_score).await?;

            info!(
                iteration,
                current_score = best_score,
                proposed_changes = %proposed,
                "proposed instruction improvements"
            );

            // Evaluate the agent built from the proposed instructions
            current_instructions = proposed;
            let candidate = candidates.build(&current_instructions)?;
            let score = self.evaluate_agent(candidate, eval_set).await?;

            info!(iteration, score, previous_best = best_score, "evaluation complete");

            if score > best_score {
                best_score = score;
                best_instructions = current_instructions.clone();
            } else {
                // Revert to best instructions if score didn't improve
                warn!(
                    iteration,
                    score, best_score, "score did not improve, reverting to best instructions"
                );
                current_instructions = best_instructions.clone();
            }

            // Check if target threshold is met
            if best_score >= self.config.target_threshold {
                info!(
                    iteration,
                    score = best_score,
                    threshold = self.config.target_threshold,
                    "target threshold reached — stopping early"
                );
                break;
            }
        }

        // Write best instructions to output file
        self.write_output(&best_instructions)?;

        info!(
            initial_score,
            final_score = best_score,
            iterations_run,
            output_path = %self.config.output_path.display(),
            "optimization complete"
        );

        Ok(OptimizationResult {
            initial_score,
            final_score: best_score,
            iterations_run,
            best_instructions,
        })
    }

    /// Evaluate the agent against the eval set and return an aggregate score.
    async fn evaluate_agent(&self, agent: Arc<dyn Agent>, eval_set: &EvalSet) -> Result<f64> {
        let base_path = std::path::Path::new(".");
        let cases = eval_set.get_all_cases(base_path)?;

        if cases.is_empty() {
            return Ok(0.0);
        }

        let mut total_score = 0.0;
        let mut case_count = 0u32;

        for case in &cases {
            let result = self.evaluator.evaluate_case(agent.clone(), case).await?;
            // Compute average of all criterion scores for this case
            let case_score = if result.scores.is_empty() {
                if result.passed { 1.0 } else { 0.0 }
            } else {
                result.scores.values().sum::<f64>() / result.scores.len() as f64
            };
            total_score += case_score;
            case_count += 1;
        }

        Ok(if case_count > 0 { total_score / f64::from(case_count) } else { 0.0 })
    }

    /// Ask the optimizer LLM to propose improved instructions.
    async fn propose_improvements(
        &self,
        current_instructions: &str,
        current_score: f64,
    ) -> Result<String> {
        let prompt = format!(
            "You are a prompt optimization assistant. Your task is to improve the following \
             system instructions for an AI agent.\n\n\
             Current instructions:\n{current_instructions}\n\n\
             Current evaluation score: {current_score:.2} (target: {target:.2})\n\n\
             Please provide improved instructions that will help the agent perform better \
             on its evaluation set. Return ONLY the improved instructions text, nothing else.",
            target = self.config.target_threshold,
        );

        let request = LlmRequest::new(
            self.optimizer_llm.name(),
            vec![Content::new("user").with_text(prompt)],
        );

        let mut stream =
            self.optimizer_llm.generate_content(request, false).await.map_err(|e| {
                EvalError::ExecutionError(format!("optimizer LLM call failed: {e}"))
            })?;

        let mut result_text = String::new();
        while let Some(response) = stream.next().await {
            let response = response.map_err(|e| {
                EvalError::ExecutionError(format!("optimizer LLM stream error: {e}"))
            })?;
            if let Some(content) = &response.content {
                for part in &content.parts {
                    if let Some(text) = part.text() {
                        result_text.push_str(text);
                    }
                }
            }
        }

        if result_text.is_empty() {
            return Err(EvalError::ExecutionError(
                "optimizer LLM returned empty response".to_string(),
            ));
        }

        Ok(result_text)
    }

    /// Write the best instructions to the output file.
    fn write_output(&self, instructions: &str) -> Result<()> {
        std::fs::write(&self.config.output_path, instructions)?;
        info!(
            path = %self.config.output_path.display(),
            "wrote optimized instructions to output file"
        );
        Ok(())
    }
}

/// Run the core optimization loop with injectable evaluation and proposal functions.
///
/// This is the pure logic extracted for testability. Given a sequence of scores
/// (one per iteration, plus the initial score), it runs the loop respecting
/// `max_iterations` and `target_threshold`.
///
/// Returns `(iterations_run, best_score)`.
pub fn run_optimization_loop(
    scores: &[f64],
    max_iterations: u32,
    target_threshold: f64,
) -> (u32, f64) {
    if scores.is_empty() {
        return (0, 0.0);
    }

    let initial_score = scores[0];

    // Early exit if initial score meets threshold
    if initial_score >= target_threshold {
        return (0, initial_score);
    }

    let mut best_score = initial_score;
    let mut iterations_run = 0u32;

    for iteration in 1..=max_iterations {
        iterations_run = iteration;

        // Get the score for this iteration (cycle through available scores)
        let score_idx = iteration as usize;
        let score = if score_idx < scores.len() {
            scores[score_idx]
        } else {
            // If we run out of scores, repeat the last one
            scores[scores.len() - 1]
        };

        if score > best_score {
            best_score = score;
        }

        // Check if target threshold is met
        if best_score >= target_threshold {
            break;
        }
    }

    (iterations_run, best_score)
}
