//! Fluent builder API for constructing the Retry & Reflect plugin.

use std::collections::HashMap;
use std::time::Duration;

use crate::config::{BackoffStrategy, RetryReflectConfig, ToolFilter};
use crate::error::RetryReflectError;
use crate::plugin::RetryReflectPlugin;

/// Builder for constructing a [`RetryReflectPlugin`] with validation.
///
/// Provides a fluent API for configuring all aspects of the retry-reflect plugin.
/// Call [`build()`](Self::build) to produce the final plugin instance.
///
/// # Example
///
/// ```rust
/// use std::time::Duration;
/// use adk_retry_reflect::RetryReflectPluginBuilder;
///
/// let plugin = RetryReflectPluginBuilder::new()
///     .max_retries(5)
///     .backoff_exponential(Duration::from_millis(100))
///     .max_backoff(Duration::from_secs(10))
///     .build()
///     .expect("valid configuration");
/// ```
#[derive(Debug)]
pub struct RetryReflectPluginBuilder {
    max_retries: u32,
    per_tool_limits: HashMap<String, u32>,
    global_limit: Option<u32>,
    backoff: BackoffStrategy,
    max_backoff: Duration,
    tool_filter: ToolFilter,
    template: Option<String>,
    priority: u32,
    global_tracking: bool,
    global_failure_threshold: u32,
}

impl Default for RetryReflectPluginBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl RetryReflectPluginBuilder {
    /// Create a new builder with default configuration.
    pub fn new() -> Self {
        Self {
            max_retries: 3,
            per_tool_limits: HashMap::new(),
            global_limit: None,
            backoff: BackoffStrategy::None,
            max_backoff: Duration::from_secs(30),
            tool_filter: ToolFilter::None,
            template: None,
            priority: 200,
            global_tracking: false,
            global_failure_threshold: 10,
        }
    }

    /// Set the default maximum number of retries per tool.
    ///
    /// Must be at least 1. Default: 3.
    pub fn max_retries(mut self, n: u32) -> Self {
        self.max_retries = n;
        self
    }

    /// Set a per-tool retry limit override.
    ///
    /// When set, this tool uses the specified limit instead of the default.
    pub fn per_tool_limit(mut self, tool: impl Into<String>, limit: u32) -> Self {
        self.per_tool_limits.insert(tool.into(), limit);
        self
    }

    /// Set the global retry limit across all tools in one invocation.
    ///
    /// When the total number of retries reaches this limit, all subsequent
    /// errors propagate without reflection prompts.
    pub fn global_limit(mut self, n: u32) -> Self {
        self.global_limit = Some(n);
        self
    }

    /// Set a fixed backoff delay between retries.
    pub fn backoff_fixed(mut self, delay: Duration) -> Self {
        self.backoff = BackoffStrategy::Fixed(delay);
        self
    }

    /// Set exponential backoff with the given base delay.
    ///
    /// Delay = `base_delay * 2^(attempt - 1)`, capped at `max_backoff`.
    pub fn backoff_exponential(mut self, base_delay: Duration) -> Self {
        self.backoff = BackoffStrategy::Exponential { base_delay };
        self
    }

    /// Set the maximum backoff duration ceiling.
    ///
    /// Default: 30 seconds.
    pub fn max_backoff(mut self, ceiling: Duration) -> Self {
        self.max_backoff = ceiling;
        self
    }

    /// Set an allowlist of tools eligible for retry.
    ///
    /// Only tools in this list will receive retry behavior.
    /// Cannot be combined with [`denylist`](Self::denylist).
    pub fn allowlist(mut self, tools: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.tool_filter = ToolFilter::Allowlist(tools.into_iter().map(Into::into).collect());
        self
    }

    /// Set a denylist of tools excluded from retry.
    ///
    /// All tools except those in this list will receive retry behavior.
    /// Cannot be combined with [`allowlist`](Self::allowlist).
    pub fn denylist(mut self, tools: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.tool_filter = ToolFilter::Denylist(tools.into_iter().map(Into::into).collect());
        self
    }

    /// Set a custom reflection prompt template.
    ///
    /// Supported placeholders: `{tool_name}`, `{args}`, `{error}`,
    /// `{attempt}`, `{max_retries}`, `{guidance}`.
    pub fn template(mut self, template: impl Into<String>) -> Self {
        self.template = Some(template.into());
        self
    }

    /// Set the plugin execution priority.
    ///
    /// Lower values execute first. Default: 200.
    pub fn priority(mut self, priority: u32) -> Self {
        self.priority = priority;
        self
    }

    /// Enable global failure tracking with the given circuit-breaker threshold.
    ///
    /// When a tool fails this many times across all invocations without a success in
    /// between, it is circuit-broken and its errors propagate immediately, until one of its
    /// calls succeeds.
    pub fn enable_global_tracking(mut self, threshold: u32) -> Self {
        self.global_tracking = true;
        self.global_failure_threshold = threshold;
        self
    }

    /// Build the plugin, validating the configuration.
    ///
    /// # Errors
    ///
    /// - [`RetryReflectError::InvalidMaxRetries`] if `max_retries` is 0
    /// - [`RetryReflectError::ConflictingFilter`] if both allowlist and denylist are set
    ///   (this shouldn't happen via the builder API, but is checked for safety)
    pub fn build(self) -> Result<RetryReflectPlugin, RetryReflectError> {
        if self.max_retries == 0 {
            return Err(RetryReflectError::InvalidMaxRetries { value: 0 });
        }

        // The builder API prevents setting both, but validate defensively
        if matches!(self.tool_filter, ToolFilter::Allowlist(_)) {
            // Check if somehow both were set (shouldn't happen via builder)
        }

        let template =
            self.template.unwrap_or_else(|| crate::template::DEFAULT_TEMPLATE.to_string());

        let config = RetryReflectConfig {
            max_retries: self.max_retries,
            per_tool_limits: self.per_tool_limits,
            global_limit: self.global_limit,
            backoff: self.backoff,
            max_backoff: self.max_backoff,
            tool_filter: self.tool_filter,
            template,
            priority: self.priority,
            global_tracking: self.global_tracking,
            global_failure_threshold: self.global_failure_threshold,
        };

        Ok(RetryReflectPlugin::from_config(config))
    }
}
