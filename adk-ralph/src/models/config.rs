//! Configuration management for Ralph multi-agent autonomous development system.
//!
//! This module provides configuration structures for:
//! - Per-agent model settings (PRD, Architect, Ralph Loop)
//! - Telemetry configuration
//! - Overall system configuration
//!
//! ## Validation
//!
//! All configuration is validated on startup to fail fast with descriptive errors.
//! Use `RalphConfig::from_env()` to load and validate configuration from environment
//! variables, or use the builder pattern with `.build()` for programmatic configuration.

use serde::{Deserialize, Serialize};
use std::env;
use std::path::Path;

/// Supported model providers.
pub const SUPPORTED_PROVIDERS: &[&str] = &["openai", "anthropic", "gemini", "ollama"];

/// Maximum allowed value for max_iterations to prevent runaway loops.
pub const MAX_ITERATIONS_LIMIT: usize = 10000;

/// Maximum allowed value for max_tokens.
pub const MAX_TOKENS_LIMIT: usize = 1_000_000;

/// Maximum allowed value for max_task_retries.
pub const MAX_RETRIES_LIMIT: usize = 100;

/// Validation error with context and suggestions.
#[derive(Debug, Clone)]
pub struct ValidationError {
    /// The field that failed validation
    pub field: String,
    /// Description of the error
    pub message: String,
    /// Suggested fix or valid values
    pub suggestion: Option<String>,
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.field, self.message)?;
        if let Some(ref suggestion) = self.suggestion {
            write!(f, ". {}", suggestion)?;
        }
        Ok(())
    }
}

impl ValidationError {
    /// Create a new validation error.
    pub fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
            suggestion: None,
        }
    }

    /// Add a suggestion to the error.
    pub fn with_suggestion(mut self, suggestion: impl Into<String>) -> Self {
        self.suggestion = Some(suggestion.into());
        self
    }
}

impl std::error::Error for ValidationError {}

/// Configuration for a single LLM model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelConfig {
    /// Model provider ("anthropic", "openai", "gemini", "ollama")
    pub provider: String,
    /// Specific model name (e.g., "claude-opus-4-5", "gpt-4o")
    pub model_name: String,
    /// Whether thinking/reasoning mode is enabled
    #[serde(default)]
    pub thinking_enabled: bool,
    /// Maximum tokens for response
    #[serde(default = "default_max_tokens")]
    pub max_tokens: usize,
    /// Temperature for generation (0.0 - 1.0)
    #[serde(default = "default_temperature")]
    pub temperature: f32,
}

fn default_max_tokens() -> usize {
    4096
}

fn default_temperature() -> f32 {
    0.7
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            provider: "gemini".to_string(),
            model_name: "gemini-2.5-flash".to_string(),
            thinking_enabled: false,
            max_tokens: default_max_tokens(),
            temperature: default_temperature(),
        }
    }
}

impl ModelConfig {
    /// Create a new model config.
    pub fn new(provider: impl Into<String>, model_name: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            model_name: model_name.into(),
            ..Default::default()
        }
    }

    /// Enable thinking mode.
    pub fn with_thinking(mut self) -> Self {
        self.thinking_enabled = true;
        self
    }

    /// Set max tokens.
    pub fn with_max_tokens(mut self, tokens: usize) -> Self {
        self.max_tokens = tokens;
        self
    }

    /// Set temperature.
    pub fn with_temperature(mut self, temp: f32) -> Self {
        self.temperature = temp;
        self
    }

    /// Validate the model config.
    ///
    /// Checks:
    /// - Provider is non-empty and supported
    /// - Model name is non-empty and has valid format
    /// - Max tokens is within reasonable bounds
    /// - Temperature is within valid range (0.0-2.0)
    pub fn validate(&self) -> Result<(), ValidationError> {
        // Validate provider
        if self.provider.is_empty() {
            return Err(ValidationError::new("provider", "Model provider cannot be empty")
                .with_suggestion(format!("Use one of: {:?}", SUPPORTED_PROVIDERS)));
        }

        let provider_lower = self.provider.to_lowercase();
        if !SUPPORTED_PROVIDERS.contains(&provider_lower.as_str()) {
            return Err(ValidationError::new(
                "provider",
                format!("Unsupported model provider '{}'", self.provider),
            )
            .with_suggestion(format!("Supported providers: {:?}", SUPPORTED_PROVIDERS)));
        }

        // Validate model name
        if self.model_name.is_empty() {
            return Err(ValidationError::new("model_name", "Model name cannot be empty")
                .with_suggestion("Specify a valid model name like 'claude-sonnet-4-20250514' or 'gpt-4o'"));
        }

        // Check for obviously invalid model names (basic sanity check)
        if self.model_name.len() > 256 {
            return Err(ValidationError::new(
                "model_name",
                format!("Model name is too long ({} chars)", self.model_name.len()),
            )
            .with_suggestion("Model names should be under 256 characters"));
        }

        // Check for invalid characters in model name
        if self.model_name.contains(char::is_control) {
            return Err(ValidationError::new(
                "model_name",
                "Model name contains invalid control characters",
            )
            .with_suggestion("Use only printable characters in model names"));
        }

        // Validate max_tokens
        if self.max_tokens == 0 {
            return Err(ValidationError::new("max_tokens", "Max tokens must be greater than 0")
                .with_suggestion("Set max_tokens to at least 1 (recommended: 4096 or higher)"));
        }

        if self.max_tokens > MAX_TOKENS_LIMIT {
            return Err(ValidationError::new(
                "max_tokens",
                format!("Max tokens {} exceeds limit of {}", self.max_tokens, MAX_TOKENS_LIMIT),
            )
            .with_suggestion(format!("Use a value between 1 and {}", MAX_TOKENS_LIMIT)));
        }

        // Validate temperature
        if !(0.0..=2.0).contains(&self.temperature) {
            return Err(ValidationError::new(
                "temperature",
                format!("Temperature {} is out of range", self.temperature),
            )
            .with_suggestion("Temperature must be between 0.0 and 2.0 (recommended: 0.7)"));
        }

        // Check for NaN or infinity
        if self.temperature.is_nan() || self.temperature.is_infinite() {
            return Err(ValidationError::new(
                "temperature",
                "Temperature must be a valid finite number",
            )
            .with_suggestion("Use a value between 0.0 and 2.0"));
        }

        Ok(())
    }
}

/// Configuration for all agents in the multi-agent system.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentModelConfig {
    /// Model config for PRD Agent (requirements generation)
    pub prd_model: ModelConfig,
    /// Model config for Architect Agent (design & task breakdown)
    pub architect_model: ModelConfig,
    /// Model config for Ralph Loop Agent (implementation)
    pub ralph_model: ModelConfig,
}

impl Default for AgentModelConfig {
    fn default() -> Self {
        Self {
            // PRD Agent: Use Gemini Pro for requirements generation
            prd_model: ModelConfig::new("gemini", "gemini-2.5-pro-preview-05-06"),
            // Architect Agent: Use Gemini Pro for design decisions
            architect_model: ModelConfig::new("gemini", "gemini-2.5-pro-preview-05-06"),
            // Ralph Loop Agent: Use Gemini Flash for implementation
            ralph_model: ModelConfig::new("gemini", "gemini-2.5-flash-preview-05-20"),
        }
    }
}

impl AgentModelConfig {
    /// Create from environment variables.
    pub fn from_env() -> Result<Self, ValidationError> {
        let mut config = Self::default();

        // PRD Agent config
        if let Ok(provider) = env::var("RALPH_PRD_PROVIDER") {
            config.prd_model.provider = provider;
        }
        if let Ok(model) = env::var("RALPH_PRD_MODEL") {
            config.prd_model.model_name = model;
        }
        if let Ok(thinking) = env::var("RALPH_PRD_THINKING") {
            config.prd_model.thinking_enabled = thinking.to_lowercase() == "true";
        }

        // Architect Agent config
        if let Ok(provider) = env::var("RALPH_ARCHITECT_PROVIDER") {
            config.architect_model.provider = provider;
        }
        if let Ok(model) = env::var("RALPH_ARCHITECT_MODEL") {
            config.architect_model.model_name = model;
        }
        if let Ok(thinking) = env::var("RALPH_ARCHITECT_THINKING") {
            config.architect_model.thinking_enabled = thinking.to_lowercase() == "true";
        }

        // Ralph Loop Agent config
        if let Ok(provider) = env::var("RALPH_LOOP_PROVIDER") {
            config.ralph_model.provider = provider;
        }
        if let Ok(model) = env::var("RALPH_LOOP_MODEL") {
            config.ralph_model.model_name = model;
        }
        if let Ok(thinking) = env::var("RALPH_LOOP_THINKING") {
            config.ralph_model.thinking_enabled = thinking.to_lowercase() == "true";
        }

        // Also support legacy single-model config
        if let Ok(provider) = env::var("RALPH_MODEL_PROVIDER") {
            config.prd_model.provider = provider.clone();
            config.architect_model.provider = provider.clone();
            config.ralph_model.provider = provider;
        }
        if let Ok(model) = env::var("RALPH_MODEL_NAME") {
            config.ralph_model.model_name = model;
        }

        config.validate()?;
        Ok(config)
    }

    /// Validate all model configs.
    pub fn validate(&self) -> Result<(), ValidationError> {
        self.prd_model
            .validate()
            .map_err(|e| ValidationError::new(
                format!("agents.prd_model.{}", e.field),
                e.message,
            ).with_suggestion(e.suggestion.unwrap_or_default()))?;
        
        self.architect_model
            .validate()
            .map_err(|e| ValidationError::new(
                format!("agents.architect_model.{}", e.field),
                e.message,
            ).with_suggestion(e.suggestion.unwrap_or_default()))?;
        
        self.ralph_model
            .validate()
            .map_err(|e| ValidationError::new(
                format!("agents.ralph_model.{}", e.field),
                e.message,
            ).with_suggestion(e.suggestion.unwrap_or_default()))?;
        
        Ok(())
    }
}

/// Telemetry configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TelemetryConfig {
    /// Whether telemetry is enabled
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Service name for telemetry
    #[serde(default = "default_service_name")]
    pub service_name: String,
    /// Whether tracing is enabled
    #[serde(default = "default_true")]
    pub enable_tracing: bool,
    /// Whether metrics are enabled
    #[serde(default = "default_true")]
    pub enable_metrics: bool,
    /// OTLP endpoint for exporting telemetry
    #[serde(default)]
    pub otlp_endpoint: Option<String>,
    /// Log level (trace, debug, info, warn, error)
    #[serde(default = "default_log_level")]
    pub log_level: String,
}

fn default_true() -> bool {
    true
}

fn default_service_name() -> String {
    "ralph".to_string()
}

fn default_log_level() -> String {
    "info".to_string()
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            service_name: default_service_name(),
            enable_tracing: true,
            enable_metrics: true,
            otlp_endpoint: None,
            log_level: default_log_level(),
        }
    }
}

impl TelemetryConfig {
    /// Create from environment variables.
    pub fn from_env() -> Result<Self, ValidationError> {
        let mut config = Self::default();

        if let Ok(enabled) = env::var("RALPH_TELEMETRY_ENABLED") {
            config.enabled = enabled.to_lowercase() == "true";
        }
        if let Ok(name) = env::var("RALPH_SERVICE_NAME") {
            config.service_name = name;
        }
        if let Ok(tracing) = env::var("RALPH_ENABLE_TRACING") {
            config.enable_tracing = tracing.to_lowercase() == "true";
        }
        if let Ok(metrics) = env::var("RALPH_ENABLE_METRICS") {
            config.enable_metrics = metrics.to_lowercase() == "true";
        }
        if let Ok(endpoint) = env::var("RALPH_OTLP_ENDPOINT") {
            config.otlp_endpoint = Some(endpoint);
        }
        if let Ok(level) = env::var("RALPH_LOG_LEVEL") {
            config.log_level = level;
        }

        config.validate()?;
        Ok(config)
    }

    /// Validate telemetry config.
    pub fn validate(&self) -> Result<(), ValidationError> {
        let valid_levels = ["trace", "debug", "info", "warn", "error"];
        if !valid_levels.contains(&self.log_level.to_lowercase().as_str()) {
            return Err(ValidationError::new(
                "log_level",
                format!("Invalid log level '{}'", self.log_level),
            )
            .with_suggestion(format!("Valid log levels: {:?}", valid_levels)));
        }

        // Validate service name
        if self.service_name.is_empty() {
            return Err(ValidationError::new(
                "service_name",
                "Service name cannot be empty",
            )
            .with_suggestion("Set RALPH_SERVICE_NAME or use default 'ralph'"));
        }

        // Validate OTLP endpoint if provided
        if let Some(ref endpoint) = self.otlp_endpoint {
            if endpoint.is_empty() {
                return Err(ValidationError::new(
                    "otlp_endpoint",
                    "OTLP endpoint cannot be empty if specified",
                )
                .with_suggestion("Either remove RALPH_OTLP_ENDPOINT or provide a valid URL like 'http://localhost:4317'"));
            }

            // Basic URL validation
            if !endpoint.starts_with("http://") && !endpoint.starts_with("https://") {
                return Err(ValidationError::new(
                    "otlp_endpoint",
                    format!("OTLP endpoint '{}' must start with http:// or https://", endpoint),
                )
                .with_suggestion("Use a valid URL like 'http://localhost:4317'"));
            }
        }

        Ok(())
    }
}

/// Main configuration for the Ralph multi-agent autonomous development system.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RalphConfig {
    /// Per-agent model configuration
    pub agents: AgentModelConfig,
    /// Telemetry configuration
    pub telemetry: TelemetryConfig,
    /// Maximum number of iterations before terminating
    #[serde(default = "default_max_iterations")]
    pub max_iterations: usize,
    /// Path to the PRD file (markdown or JSON)
    #[serde(default = "default_prd_path")]
    pub prd_path: String,
    /// Path to the design file
    #[serde(default = "default_design_path")]
    pub design_path: String,
    /// Path to the tasks file
    #[serde(default = "default_tasks_path")]
    pub tasks_path: String,
    /// Path to the progress file
    #[serde(default = "default_progress_path")]
    pub progress_path: String,
    /// Base directory for the project
    #[serde(default = "default_project_path")]
    pub project_path: String,
    /// Completion promise text
    #[serde(default = "default_completion_promise")]
    pub completion_promise: String,
    /// Maximum retries for failed tasks
    #[serde(default = "default_max_retries")]
    pub max_task_retries: usize,
}

fn default_max_iterations() -> usize {
    50
}

fn default_prd_path() -> String {
    "prd.md".to_string()
}

fn default_design_path() -> String {
    "design.md".to_string()
}

fn default_tasks_path() -> String {
    "tasks.json".to_string()
}

fn default_progress_path() -> String {
    "progress.json".to_string()
}

fn default_project_path() -> String {
    ".".to_string()
}

fn default_completion_promise() -> String {
    "All tasks completed successfully!".to_string()
}

fn default_max_retries() -> usize {
    3
}

impl Default for RalphConfig {
    fn default() -> Self {
        Self {
            agents: AgentModelConfig::default(),
            telemetry: TelemetryConfig::default(),
            max_iterations: default_max_iterations(),
            prd_path: default_prd_path(),
            design_path: default_design_path(),
            tasks_path: default_tasks_path(),
            progress_path: default_progress_path(),
            project_path: default_project_path(),
            completion_promise: default_completion_promise(),
            max_task_retries: default_max_retries(),
        }
    }
}

impl RalphConfig {
    /// Create a new configuration builder.
    pub fn builder() -> RalphConfigBuilder {
        RalphConfigBuilder::default()
    }

    /// Load configuration from environment variables with defaults.
    ///
    /// This method loads configuration from environment variables and validates
    /// all settings. If validation fails, it returns a descriptive error with
    /// suggestions for fixing the issue.
    ///
    /// ## Environment Variables
    ///
    /// - `RALPH_MAX_ITERATIONS` - Maximum loop iterations (default: 50)
    /// - `RALPH_PRD_PATH` - Path to PRD file (default: prd.md)
    /// - `RALPH_DESIGN_PATH` - Path to design file (default: design.md)
    /// - `RALPH_TASKS_PATH` - Path to tasks file (default: tasks.json)
    /// - `RALPH_PROGRESS_PATH` - Path to progress file (default: progress.json)
    /// - `RALPH_PROJECT_PATH` - Base project directory (default: .)
    /// - `RALPH_COMPLETION_PROMISE` - Message on completion
    /// - `RALPH_MAX_TASK_RETRIES` - Max retries per task (default: 3)
    pub fn from_env() -> Result<Self, ValidationError> {
        let config = Self {
            agents: AgentModelConfig::from_env()?,
            telemetry: TelemetryConfig::from_env()?,
            ..Default::default()
        };

        // Load other settings with validation
        let mut config = config;
        if let Ok(iterations) = env::var("RALPH_MAX_ITERATIONS") {
            config.max_iterations = iterations.parse().map_err(|e| {
                ValidationError::new(
                    "max_iterations",
                    format!("Invalid RALPH_MAX_ITERATIONS '{}': {}", iterations, e),
                )
                .with_suggestion("Use a positive integer like 50 or 100")
            })?;
        }

        if let Ok(path) = env::var("RALPH_PRD_PATH") {
            config.prd_path = path;
        }

        if let Ok(path) = env::var("RALPH_DESIGN_PATH") {
            config.design_path = path;
        }

        if let Ok(path) = env::var("RALPH_TASKS_PATH") {
            config.tasks_path = path;
        }

        if let Ok(path) = env::var("RALPH_PROGRESS_PATH") {
            config.progress_path = path;
        }

        if let Ok(path) = env::var("RALPH_PROJECT_PATH") {
            config.project_path = path;
        }

        if let Ok(promise) = env::var("RALPH_COMPLETION_PROMISE") {
            config.completion_promise = promise;
        }

        if let Ok(retries) = env::var("RALPH_MAX_TASK_RETRIES") {
            config.max_task_retries = retries.parse().map_err(|e| {
                ValidationError::new(
                    "max_task_retries",
                    format!("Invalid RALPH_MAX_TASK_RETRIES '{}': {}", retries, e),
                )
                .with_suggestion("Use a positive integer like 3 or 5")
            })?;
        }

        config.validate()?;
        Ok(config)
    }

    /// Validate the configuration settings.
    ///
    /// Performs comprehensive validation including:
    /// - All agent model configurations
    /// - Telemetry configuration
    /// - Numeric bounds (max_iterations, max_task_retries)
    /// - Path validity (non-empty, valid characters)
    ///
    /// Returns a descriptive error with suggestions on failure.
    pub fn validate(&self) -> Result<(), ValidationError> {
        // Validate nested configs
        self.agents.validate()?;
        self.telemetry.validate()?;

        // Validate max_iterations
        if self.max_iterations == 0 {
            return Err(ValidationError::new(
                "max_iterations",
                "Max iterations must be greater than 0",
            )
            .with_suggestion("Set RALPH_MAX_ITERATIONS to at least 1 (recommended: 50)"));
        }

        if self.max_iterations > MAX_ITERATIONS_LIMIT {
            return Err(ValidationError::new(
                "max_iterations",
                format!(
                    "Max iterations {} exceeds safety limit of {}",
                    self.max_iterations, MAX_ITERATIONS_LIMIT
                ),
            )
            .with_suggestion(format!(
                "Use a value between 1 and {} to prevent runaway loops",
                MAX_ITERATIONS_LIMIT
            )));
        }

        // Validate max_task_retries
        if self.max_task_retries == 0 {
            return Err(ValidationError::new(
                "max_task_retries",
                "Max task retries must be greater than 0",
            )
            .with_suggestion("Set RALPH_MAX_TASK_RETRIES to at least 1 (recommended: 3)"));
        }

        if self.max_task_retries > MAX_RETRIES_LIMIT {
            return Err(ValidationError::new(
                "max_task_retries",
                format!(
                    "Max task retries {} exceeds limit of {}",
                    self.max_task_retries, MAX_RETRIES_LIMIT
                ),
            )
            .with_suggestion(format!(
                "Use a value between 1 and {}",
                MAX_RETRIES_LIMIT
            )));
        }

        // Validate paths
        validate_path("prd_path", &self.prd_path)?;
        validate_path("design_path", &self.design_path)?;
        validate_path("tasks_path", &self.tasks_path)?;
        validate_path("progress_path", &self.progress_path)?;
        validate_path("project_path", &self.project_path)?;

        // Validate completion_promise (can be empty but not too long)
        if self.completion_promise.len() > 1000 {
            return Err(ValidationError::new(
                "completion_promise",
                format!(
                    "Completion promise is too long ({} chars)",
                    self.completion_promise.len()
                ),
            )
            .with_suggestion("Keep completion promise under 1000 characters"));
        }

        Ok(())
    }
}

/// Validate a file path configuration value.
///
/// Checks:
/// - Path is not empty
/// - Path doesn't contain null bytes
/// - Path doesn't contain invalid characters for the OS
fn validate_path(field: &str, path: &str) -> Result<(), ValidationError> {
    if path.is_empty() {
        return Err(ValidationError::new(field, format!("{} cannot be empty", field))
            .with_suggestion(format!("Set RALPH_{} to a valid file path", field.to_uppercase())));
    }

    // Check for null bytes (invalid in all paths)
    if path.contains('\0') {
        return Err(ValidationError::new(
            field,
            format!("{} contains invalid null byte", field),
        )
        .with_suggestion("Remove null characters from the path"));
    }

    // Check for control characters
    if path.chars().any(|c| c.is_control() && c != '\t') {
        return Err(ValidationError::new(
            field,
            format!("{} contains invalid control characters", field),
        )
        .with_suggestion("Use only printable characters in paths"));
    }

    // Check path length (most filesystems have limits)
    if path.len() > 4096 {
        return Err(ValidationError::new(
            field,
            format!("{} is too long ({} chars)", field, path.len()),
        )
        .with_suggestion("Path should be under 4096 characters"));
    }

    // Validate path syntax using std::path
    let path_obj = Path::new(path);
    
    // Check for empty components (e.g., "foo//bar")
    // This is a warning-level issue, not an error, so we allow it
    
    // Check that the path has at least one component
    if path_obj.components().next().is_none() && path != "." {
        return Err(ValidationError::new(
            field,
            format!("{} is not a valid path", field),
        )
        .with_suggestion("Provide a valid relative or absolute path"));
    }

    Ok(())
}

/// Builder for RalphConfig with fluent API.
#[derive(Debug, Clone, Default)]
pub struct RalphConfigBuilder {
    config: RalphConfig,
}

impl RalphConfigBuilder {
    /// Set the agent model configuration.
    pub fn agents(mut self, agents: AgentModelConfig) -> Self {
        self.config.agents = agents;
        self
    }

    /// Set the telemetry configuration.
    pub fn telemetry(mut self, telemetry: TelemetryConfig) -> Self {
        self.config.telemetry = telemetry;
        self
    }

    /// Set the maximum iterations.
    pub fn max_iterations(mut self, iterations: usize) -> Self {
        self.config.max_iterations = iterations;
        self
    }

    /// Set the PRD file path.
    pub fn prd_path(mut self, path: impl Into<String>) -> Self {
        self.config.prd_path = path.into();
        self
    }

    /// Set the design file path.
    pub fn design_path(mut self, path: impl Into<String>) -> Self {
        self.config.design_path = path.into();
        self
    }

    /// Set the tasks file path.
    pub fn tasks_path(mut self, path: impl Into<String>) -> Self {
        self.config.tasks_path = path.into();
        self
    }

    /// Set the progress file path.
    pub fn progress_path(mut self, path: impl Into<String>) -> Self {
        self.config.progress_path = path.into();
        self
    }

    /// Set the project base path.
    pub fn project_path(mut self, path: impl Into<String>) -> Self {
        self.config.project_path = path.into();
        self
    }

    /// Set the completion promise text.
    pub fn completion_promise(mut self, promise: impl Into<String>) -> Self {
        self.config.completion_promise = promise.into();
        self
    }

    /// Set the max task retries.
    pub fn max_task_retries(mut self, retries: usize) -> Self {
        self.config.max_task_retries = retries;
        self
    }

    /// Build the configuration, validating it first.
    pub fn build(self) -> Result<RalphConfig, ValidationError> {
        self.config.validate()?;
        Ok(self.config)
    }

    /// Build the configuration without validation.
    pub fn build_unchecked(self) -> RalphConfig {
        self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_config_validation() {
        let valid = ModelConfig::new("anthropic", "claude-sonnet-4-20250514");
        assert!(valid.validate().is_ok());

        let invalid_provider = ModelConfig::new("invalid", "model");
        let err = invalid_provider.validate().unwrap_err();
        assert_eq!(err.field, "provider");
        assert!(err.suggestion.is_some());

        let empty_model = ModelConfig::new("anthropic", "");
        let err = empty_model.validate().unwrap_err();
        assert_eq!(err.field, "model_name");
    }

    #[test]
    fn test_model_config_max_tokens_validation() {
        let mut config = ModelConfig::default();
        config.max_tokens = 0;
        let err = config.validate().unwrap_err();
        assert_eq!(err.field, "max_tokens");

        config.max_tokens = MAX_TOKENS_LIMIT + 1;
        let err = config.validate().unwrap_err();
        assert_eq!(err.field, "max_tokens");
    }

    #[test]
    fn test_model_config_temperature_validation() {
        let mut config = ModelConfig::default();
        
        // Valid temperatures
        config.temperature = 0.0;
        assert!(config.validate().is_ok());
        
        config.temperature = 2.0;
        assert!(config.validate().is_ok());
        
        // Invalid temperatures
        config.temperature = -0.1;
        assert!(config.validate().is_err());
        
        config.temperature = 2.1;
        assert!(config.validate().is_err());
        
        config.temperature = f32::NAN;
        assert!(config.validate().is_err());
        
        config.temperature = f32::INFINITY;
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_model_name_validation() {
        // Valid model names
        let config = ModelConfig::new("anthropic", "claude-sonnet-4-20250514");
        assert!(config.validate().is_ok());

        // Model name too long
        let long_name = "a".repeat(257);
        let config = ModelConfig::new("anthropic", &long_name);
        let err = config.validate().unwrap_err();
        assert_eq!(err.field, "model_name");
        assert!(err.message.contains("too long"));

        // Model name with control characters
        let config = ModelConfig::new("anthropic", "model\x00name");
        let err = config.validate().unwrap_err();
        assert_eq!(err.field, "model_name");
    }

    #[test]
    fn test_agent_model_config_defaults() {
        let config = AgentModelConfig::default();

        // All agents should use Gemini by default
        assert_eq!(config.prd_model.provider, "gemini");
        assert_eq!(config.architect_model.provider, "gemini");
        assert_eq!(config.ralph_model.provider, "gemini");
    }

    #[test]
    fn test_ralph_config_defaults() {
        let config = RalphConfig::default();
        assert_eq!(config.max_iterations, 50);
        assert_eq!(config.prd_path, "prd.md");
        assert_eq!(config.tasks_path, "tasks.json");
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_ralph_config_builder() {
        let config = RalphConfig::builder()
            .max_iterations(100)
            .prd_path("custom_prd.md")
            .completion_promise("Done!")
            .build()
            .unwrap();

        assert_eq!(config.max_iterations, 100);
        assert_eq!(config.prd_path, "custom_prd.md");
        assert_eq!(config.completion_promise, "Done!");
    }

    #[test]
    fn test_ralph_config_max_iterations_validation() {
        // Zero max iterations
        let result = RalphConfig::builder().max_iterations(0).build();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.field, "max_iterations");

        // Exceeds limit
        let result = RalphConfig::builder()
            .max_iterations(MAX_ITERATIONS_LIMIT + 1)
            .build();
        assert!(result.is_err());
    }

    #[test]
    fn test_ralph_config_path_validation() {
        // Empty paths
        let result = RalphConfig::builder().prd_path("").build();
        assert!(result.is_err());

        let result = RalphConfig::builder().design_path("").build();
        assert!(result.is_err());

        let result = RalphConfig::builder().tasks_path("").build();
        assert!(result.is_err());

        let result = RalphConfig::builder().progress_path("").build();
        assert!(result.is_err());

        // Path with null byte
        let result = RalphConfig::builder().prd_path("test\x00.md").build();
        assert!(result.is_err());

        // Valid paths
        let result = RalphConfig::builder()
            .prd_path("./docs/prd.md")
            .build();
        assert!(result.is_ok());
    }

    #[test]
    fn test_telemetry_config_validation() {
        let valid = TelemetryConfig::default();
        assert!(valid.validate().is_ok());

        let mut invalid = TelemetryConfig::default();
        invalid.log_level = "invalid_level".to_string();
        let err = invalid.validate().unwrap_err();
        assert_eq!(err.field, "log_level");

        // Empty service name
        invalid = TelemetryConfig::default();
        invalid.service_name = "".to_string();
        let err = invalid.validate().unwrap_err();
        assert_eq!(err.field, "service_name");

        // Invalid OTLP endpoint
        invalid = TelemetryConfig::default();
        invalid.otlp_endpoint = Some("not-a-url".to_string());
        let err = invalid.validate().unwrap_err();
        assert_eq!(err.field, "otlp_endpoint");

        // Valid OTLP endpoint
        let mut valid = TelemetryConfig::default();
        valid.otlp_endpoint = Some("http://localhost:4317".to_string());
        assert!(valid.validate().is_ok());
    }

    #[test]
    fn test_validation_error_display() {
        let err = ValidationError::new("field", "message");
        assert_eq!(err.to_string(), "field: message");

        let err = ValidationError::new("field", "message")
            .with_suggestion("try this");
        assert_eq!(err.to_string(), "field: message. try this");
    }
}
