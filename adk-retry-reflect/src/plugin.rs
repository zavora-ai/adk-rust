//! Core plugin implementation.

use std::sync::Arc;

use adk_core::{CallbackContext, Result, Tool, ToolEffect, async_trait};
use adk_plugin::{AfterToolCallResult, EnhancedPlugin, PluginContext};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::backoff::compute_backoff;
use crate::config::RetryReflectConfig;
use crate::detection::{is_authorization_denial, is_error_result, is_timeout};
use crate::filter::is_tool_eligible;
use crate::template::render_reflection;
use crate::tracker::{GlobalRetryTracker, RetryTracker};

/// The Retry & Reflect plugin.
///
/// Intercepts tool call failures via the `after_tool_call` hook, tracks retry state,
/// computes backoff delays, and injects structured reflection prompts to help the
/// agent self-correct on the next turn.
///
/// Two failures pass through unchanged, with no retry suggested:
///
/// - **Authorization or approval refusals** — repeating the call is refused again.
/// - **Timeouts of a [`ToolEffect::NonIdempotent`] tool** — the call may have taken
///   effect, so repeating it could repeat a payment or other side effect.
///
/// # Architecture
///
/// The plugin implements [`EnhancedPlugin`] from `adk-plugin` and uses:
/// - A per-invocation [`RetryTracker`] (wrapped in `Arc<Mutex<...>>`) for failure counts
/// - An optional [`GlobalRetryTracker`] for cross-invocation circuit-breaker patterns
///
/// # Example
///
/// ```rust
/// use adk_retry_reflect::RetryReflectPluginBuilder;
/// use std::time::Duration;
///
/// let plugin = RetryReflectPluginBuilder::new()
///     .max_retries(3)
///     .backoff_exponential(Duration::from_millis(100))
///     .build()
///     .expect("valid config");
/// ```
pub struct RetryReflectPlugin {
    config: RetryReflectConfig,
    tracker: Arc<Mutex<RetryTracker>>,
    global_tracker: Option<Arc<Mutex<GlobalRetryTracker>>>,
}

impl std::fmt::Debug for RetryReflectPlugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RetryReflectPlugin").field("config", &self.config).finish_non_exhaustive()
    }
}

impl RetryReflectPlugin {
    /// Create a plugin from a validated configuration.
    pub(crate) fn from_config(config: RetryReflectConfig) -> Self {
        let global_tracker = if config.global_tracking {
            Some(Arc::new(Mutex::new(GlobalRetryTracker::new())))
        } else {
            None
        };

        Self { config, tracker: Arc::new(Mutex::new(RetryTracker::new())), global_tracker }
    }

    /// Get a reference to the plugin's configuration.
    pub fn config(&self) -> &RetryReflectConfig {
        &self.config
    }

    /// Reset per-invocation state (called between agent runs).
    ///
    /// Clears the per-invocation retry tracker. Does NOT reset the global tracker.
    pub async fn reset(&self) {
        let mut tracker = self.tracker.lock().await;
        tracker.reset();
    }

    /// Get the effective retry limit for a specific tool.
    ///
    /// Returns the per-tool override if configured, otherwise the default max_retries.
    fn effective_limit(&self, tool_name: &str) -> u32 {
        self.config.per_tool_limits.get(tool_name).copied().unwrap_or(self.config.max_retries)
    }

    /// Extract an error message from a tool result value.
    fn extract_error_message(result: &Value) -> String {
        match result {
            Value::Object(map) => {
                if let Some(err) = map.get("error") {
                    match err {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    }
                } else {
                    result.to_string()
                }
            }
            Value::String(s) => s.clone(),
            _ => result.to_string(),
        }
    }
}

#[async_trait]
impl EnhancedPlugin for RetryReflectPlugin {
    fn name(&self) -> &str {
        "retry-reflect"
    }

    fn priority(&self) -> i32 {
        self.config.priority as i32
    }

    async fn after_tool_call(
        &self,
        tool: Arc<dyn Tool>,
        args: &Value,
        result: Value,
        _ctx: Arc<dyn CallbackContext>,
        _plugin_ctx: &PluginContext,
    ) -> Result<AfterToolCallResult> {
        // Step 1: Check if the result is an error
        if !is_error_result(&result) {
            return Ok(AfterToolCallResult::Continue(result));
        }

        let tool_name = tool.name();

        // A refusal or a possibly-applied side effect is never worth a retry suggestion.
        let error_msg = Self::extract_error_message(&result);
        if is_authorization_denial(&error_msg) {
            tracing::info!(tool_name = %tool_name, error = %error_msg, "retry_reflect.skipped_authorization_denial");
            return Ok(AfterToolCallResult::Continue(result));
        }
        if tool.effect() == ToolEffect::NonIdempotent && is_timeout(&error_msg) {
            tracing::warn!(tool_name = %tool_name, error = %error_msg, "retry_reflect.skipped_non_idempotent_timeout");
            return Ok(AfterToolCallResult::Continue(result));
        }

        // Step 2: Check tool eligibility
        if !is_tool_eligible(&self.config.tool_filter, tool_name) {
            return Ok(AfterToolCallResult::Continue(result));
        }

        // Step 3: Check global circuit-breaker (if enabled)
        if let Some(ref global_tracker) = self.global_tracker {
            let gt = global_tracker.lock().await;
            if gt.is_circuit_broken(tool_name, self.config.global_failure_threshold) {
                tracing::warn!(
                    tool_name = %tool_name,
                    threshold = self.config.global_failure_threshold,
                    "retry_reflect.circuit_broken"
                );
                return Ok(AfterToolCallResult::Continue(result));
            }
        }

        // Use a synthetic call_id based on args hash for tracking
        let call_id = format!("{:x}", {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            args.to_string().hash(&mut hasher);
            hasher.finish()
        });
        let tracker_key = format!("{tool_name}:{call_id}");

        let effective_limit = self.effective_limit(tool_name);

        // Step 4: Check per-tool and global retry limits
        let mut tracker = self.tracker.lock().await;
        let current_count = tracker.get(&tracker_key);

        if current_count >= effective_limit {
            // Retry limit exceeded — emit exhaustion event and propagate error
            tracing::warn!(
                tool_name = %tool_name,
                total_attempts = current_count,
                error = %error_msg,
                "retry_reflect.exhausted"
            );
            return Ok(AfterToolCallResult::Continue(result));
        }

        // Check global limit
        if let Some(global_limit) = self.config.global_limit
            && tracker.total() >= global_limit
        {
            tracing::warn!(
                tool_name = %tool_name,
                total_attempts = tracker.total(),
                error = %error_msg,
                "retry_reflect.global_limit_exhausted"
            );
            return Ok(AfterToolCallResult::Continue(result));
        }

        // Step 5: Increment failure counters
        let attempt = tracker.increment(&tracker_key);
        drop(tracker); // Release lock before sleeping

        // Record in global tracker if enabled
        if let Some(ref global_tracker) = self.global_tracker {
            let mut gt = global_tracker.lock().await;
            gt.record_failure(tool_name);
        }

        // Step 6: Compute backoff and sleep
        let backoff = compute_backoff(&self.config.backoff, attempt, self.config.max_backoff);
        if !backoff.is_zero() {
            tokio::time::sleep(backoff).await;
        }

        // Step 7: Render reflection prompt
        let args_str = serde_json::to_string_pretty(args).unwrap_or_else(|_| args.to_string());
        let reflection = render_reflection(
            &self.config.template,
            tool_name,
            &args_str,
            &error_msg,
            attempt,
            effective_limit,
            "",
        );

        // Step 8: Emit retry tracing event
        tracing::info!(
            tool_name = %tool_name,
            attempt = attempt,
            max_retries = effective_limit,
            backoff_ms = backoff.as_millis() as u64,
            error = %error_msg,
            "retry_reflect.retry"
        );

        // Step 9: Return modified result with reflection
        let reflection_value = serde_json::json!({
            "reflection": reflection
        });

        Ok(AfterToolCallResult::Continue(reflection_value))
    }
}
