//! `adk optimize` CLI subcommand.
//!
//! Iteratively improves an agent's system instructions using an evaluation set
//! and an optimizer LLM. Gated behind the `optimize` feature flag.

use std::path::PathBuf;

use clap::Args;

/// Arguments for the `adk optimize` subcommand.
#[derive(Debug, Clone, Args)]
pub struct OptimizeArgs {
    /// Path to agent configuration (YAML or Rust).
    pub agent_path: PathBuf,
    /// Path to evaluation set.
    pub eval_set_path: PathBuf,
    /// Maximum optimization iterations.
    #[arg(long, default_value = "5")]
    pub max_iterations: u32,
    /// Target score threshold.
    #[arg(long, default_value = "0.9")]
    pub target: f64,
    /// Output file for optimized instructions.
    #[arg(long, default_value = "optimized_instructions.txt")]
    pub output: PathBuf,
}
