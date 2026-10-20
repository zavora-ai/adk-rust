//! Configuration types for the Retry & Reflect plugin.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

/// Strategy for computing delay between retry attempts.
#[derive(Debug, Clone, PartialEq)]
pub enum BackoffStrategy {
    /// No delay between retries.
    None,
    /// Fixed delay between retries.
    Fixed(Duration),
    /// Exponential backoff: `base_delay * 2^(attempt - 1)`.
    Exponential {
        /// The base delay used for exponential computation.
        base_delay: Duration,
    },
}

/// Filter determining which tools are eligible for retry behavior.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolFilter {
    /// All tools are eligible for retry.
    None,
    /// Only tools in this set are eligible.
    Allowlist(HashSet<String>),
    /// All tools except those in this set are eligible.
    Denylist(HashSet<String>),
}

/// Immutable configuration for the Retry & Reflect plugin.
#[derive(Debug, Clone)]
pub struct RetryReflectConfig {
    /// Default max retries for any tool (default: 3).
    pub max_retries: u32,
    /// Per-tool retry limit overrides.
    pub per_tool_limits: HashMap<String, u32>,
    /// Global retry limit across all tools in one invocation (None = unlimited).
    pub global_limit: Option<u32>,
    /// Backoff strategy between retries.
    pub backoff: BackoffStrategy,
    /// Maximum backoff duration ceiling (default: 30s).
    pub max_backoff: Duration,
    /// Tool eligibility filter.
    pub tool_filter: ToolFilter,
    /// Reflection prompt template.
    pub template: String,
    /// Plugin priority for execution ordering (default: 200).
    pub priority: u32,
    /// Whether to persist failure counts across invocations.
    pub global_tracking: bool,
    /// Global failure threshold for circuit-breaker (default: 10).
    pub global_failure_threshold: u32,
    /// Whether log lines carry the tool's error text (default: `false`).
    ///
    /// The text can quote arguments and credentials, so it follows the rule of
    /// `RunConfig::record_payloads`: off, a log line carries the error's category and
    /// code only. Set it to the value the runs use.
    pub record_payloads: bool,
}

impl Default for RetryReflectConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            per_tool_limits: HashMap::new(),
            global_limit: None,
            backoff: BackoffStrategy::None,
            max_backoff: Duration::from_secs(30),
            tool_filter: ToolFilter::None,
            template: crate::template::DEFAULT_TEMPLATE.to_string(),
            priority: 200,
            global_tracking: false,
            global_failure_threshold: 10,
            record_payloads: false,
        }
    }
}
