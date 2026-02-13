//! Configuration management for Ralph autonomous agent.
//!
//! This module handles loading configuration from environment variables
//! and provides sensible defaults for all settings.

use crate::error::{RalphError, Result};
use serde::{Deserialize, Serialize};
use std::env;

/// Main configuration for the Ralph autonomous agent system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RalphConfig {
    /// Model provider to use ("openai", "anthropic", "gemini")
    pub model_provider: String,

    /// Specific model name to use
    pub model_name: String,

    /// Maximum number of iterations before terminating
    pub max_iterations: usize,

    /// Path to the PRD JSON file
    pub prd_path: String,

    /// Path to the progress log file
    pub progress_path: String,

    /// Path to the AGENTS.md file for git commits
    pub agents_md_path: String,

    /// Base directory for the project
    pub project_path: String,
}

impl Default for RalphConfig {
    fn default() -> Self {
        Self {
            model_provider: "openai".to_string(),
            model_name: "gpt-4".to_string(),
            max_iterations: 50,
            prd_path: "prd.json".to_string(),
            progress_path: "progress.md".to_string(),
            agents_md_path: "AGENTS.md".to_string(),
            project_path: ".".to_string(),
        }
    }
}

impl RalphConfig {
    /// Load configuration from environment variables with defaults.
    pub fn from_env() -> Result<Self> {
        let mut config = Self::default();

        // Load from environment variables
        if let Ok(provider) = env::var("RALPH_MODEL_PROVIDER") {
            config.model_provider = provider;
        }

        if let Ok(model) = env::var("RALPH_MODEL_NAME") {
            config.model_name = model;
        }

        if let Ok(iterations) = env::var("RALPH_MAX_ITERATIONS") {
            config.max_iterations = iterations.parse().map_err(|e| {
                RalphError::Configuration(format!("Invalid RALPH_MAX_ITERATIONS: {}", e))
            })?;
        }

        if let Ok(path) = env::var("RALPH_PRD_PATH") {
            config.prd_path = path;
        }

        if let Ok(path) = env::var("RALPH_PROGRESS_PATH") {
            config.progress_path = path;
        }

        if let Ok(path) = env::var("RALPH_AGENTS_MD_PATH") {
            config.agents_md_path = path;
        }

        if let Ok(path) = env::var("RALPH_PROJECT_PATH") {
            config.project_path = path;
        }

        // Validate configuration
        config.validate()?;

        Ok(config)
    }

    /// Validate the configuration settings.
    pub fn validate(&self) -> Result<()> {
        if self.model_provider.is_empty() {
            return Err(RalphError::Configuration("Model provider cannot be empty".to_string()));
        }

        if self.model_name.is_empty() {
            return Err(RalphError::Configuration("Model name cannot be empty".to_string()));
        }

        if self.max_iterations == 0 {
            return Err(RalphError::Configuration(
                "Max iterations must be greater than 0".to_string(),
            ));
        }

        if self.prd_path.is_empty() {
            return Err(RalphError::Configuration("PRD path cannot be empty".to_string()));
        }

        Ok(())
    }
}
