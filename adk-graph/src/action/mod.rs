//! Action node executor for adk-graph.
//!
//! Wraps any `ActionNodeConfig` and implements the `Node` trait, applying
//! `StandardProperties` (error handling, timeout, skip condition) before
//! dispatching to node-specific execution logic.

pub mod code;
pub mod file;
pub mod loop_node;
pub mod merge;
pub mod set;
pub mod switch;
pub mod transform;
pub mod trigger;
#[cfg(feature = "action-trigger")]
pub mod trigger_runtime;
pub mod wait;

#[cfg(feature = "action-http")]
pub mod http;

#[cfg(feature = "action-db")]
pub mod database;

#[cfg(feature = "action-email")]
pub mod email;

#[cfg(feature = "action-http")]
pub mod notification;

#[cfg(feature = "action-rss")]
pub mod rss;

use std::collections::HashMap;

use adk_action::{ActionNodeConfig, ErrorMode, interpolate_variables};
use async_trait::async_trait;
use serde_json::Value;

use crate::error::{GraphError, Result};
use crate::node::{Node, NodeContext, NodeOutput};

/// Executor that wraps an `ActionNodeConfig` and implements the `Node` trait.
///
/// Applies `StandardProperties` uniformly:
/// 1. Check skip condition
/// 2. Apply timeout from `ExecutionControl`
/// 3. Dispatch to the appropriate node executor
/// 4. Apply error handling (stop/continue/retry/fallback)
pub struct ActionNodeExecutor {
    config: ActionNodeConfig,
}

impl ActionNodeExecutor {
    /// Create a new executor wrapping the given action node config.
    pub fn new(config: ActionNodeConfig) -> Self {
        Self { config }
    }

    /// Returns a reference to the inner config.
    pub fn config(&self) -> &ActionNodeConfig {
        &self.config
    }

    /// Build a state map from the `NodeContext` for interpolation.
    fn state_map(ctx: &NodeContext) -> HashMap<String, Value> {
        ctx.state.clone()
    }

    /// Check the skip condition. Returns `true` if the node should be skipped.
    fn should_skip(&self, ctx: &NodeContext) -> bool {
        let std = self.config.standard();
        if let Some(condition) = &std.execution.condition {
            let state = Self::state_map(ctx);
            let resolved = interpolate_variables(condition, &state);
            let trimmed = resolved.trim().to_lowercase();
            trimmed.is_empty() || trimmed == "false"
        } else {
            false
        }
    }

    /// Dispatch to the appropriate node executor based on the config variant.
    async fn dispatch(&self, ctx: &NodeContext) -> Result<NodeOutput> {
        match &self.config {
            ActionNodeConfig::Trigger(c) => trigger::execute_trigger(c, ctx).await,
            ActionNodeConfig::Set(c) => set::execute_set(c, ctx).await,
            ActionNodeConfig::Transform(c) => transform::execute_transform(c, ctx).await,
            ActionNodeConfig::Switch(c) => switch::execute_switch(c, ctx).await,
            ActionNodeConfig::Loop(c) => loop_node::execute_loop(c, ctx).await,
            ActionNodeConfig::Merge(c) => merge::execute_merge(c, ctx).await,
            ActionNodeConfig::Wait(c) => wait::execute_wait(c, ctx).await,
            ActionNodeConfig::File(c) => file::execute_file(c, ctx).await,
            ActionNodeConfig::Code(c) => code::execute_code(c, ctx).await,

            #[cfg(feature = "action-http")]
            ActionNodeConfig::Http(c) => http::execute_http(c, ctx).await,
            #[cfg(not(feature = "action-http"))]
            ActionNodeConfig::Http(_) => Err(GraphError::NodeExecutionFailed {
                node: self.config.standard().id.clone(),
                message: "HTTP node requires the 'action-http' feature".into(),
            }),

            #[cfg(feature = "action-db")]
            ActionNodeConfig::Database(c) => database::execute_database(c, ctx).await,
            #[cfg(not(feature = "action-db"))]
            ActionNodeConfig::Database(_) => Err(GraphError::NodeExecutionFailed {
                node: self.config.standard().id.clone(),
                message: "Database node requires the 'action-db' feature".into(),
            }),

            #[cfg(feature = "action-email")]
            ActionNodeConfig::Email(c) => email::execute_email(c, ctx).await,
            #[cfg(not(feature = "action-email"))]
            ActionNodeConfig::Email(_) => Err(GraphError::NodeExecutionFailed {
                node: self.config.standard().id.clone(),
                message: "Email node requires the 'action-email' feature".into(),
            }),

            #[cfg(feature = "action-http")]
            ActionNodeConfig::Notification(c) => notification::execute_notification(c, ctx).await,
            #[cfg(not(feature = "action-http"))]
            ActionNodeConfig::Notification(_) => Err(GraphError::NodeExecutionFailed {
                node: self.config.standard().id.clone(),
                message: "Notification node requires the 'action-http' feature".into(),
            }),

            #[cfg(feature = "action-rss")]
            ActionNodeConfig::Rss(c) => rss::execute_rss(c, ctx).await,
            #[cfg(not(feature = "action-rss"))]
            ActionNodeConfig::Rss(_) => Err(GraphError::NodeExecutionFailed {
                node: self.config.standard().id.clone(),
                message: "RSS node requires the 'action-rss' feature".into(),
            }),
        }
    }

    /// Apply error handling around the dispatch.
    async fn execute_with_error_handling(&self, ctx: &NodeContext) -> Result<NodeOutput> {
        let std = self.config.standard();
        let error_handling = &std.error_handling;

        match error_handling.mode {
            ErrorMode::Stop => self.dispatch(ctx).await,

            ErrorMode::Continue => match self.dispatch(ctx).await {
                Ok(output) => Ok(output),
                Err(e) => {
                    tracing::warn!(
                        node = %std.id,
                        error = %e,
                        "action node error swallowed (continue mode)"
                    );
                    Ok(NodeOutput::new())
                }
            },

            ErrorMode::Retry => {
                let max_attempts = error_handling.retry_count.unwrap_or(1) + 1;
                let delay_ms = error_handling.retry_delay.unwrap_or(0);
                let mut last_err = None;

                for attempt in 0..max_attempts {
                    match self.dispatch(ctx).await {
                        Ok(output) => return Ok(output),
                        Err(e) => {
                            tracing::warn!(
                                node = %std.id,
                                attempt = attempt + 1,
                                max_attempts,
                                error = %e,
                                "action node retry"
                            );
                            last_err = Some(e);
                            if attempt + 1 < max_attempts && delay_ms > 0 {
                                tokio::time::sleep(std::time::Duration::from_millis(delay_ms))
                                    .await;
                            }
                        }
                    }
                }

                Err(last_err.unwrap_or_else(|| GraphError::NodeExecutionFailed {
                    node: std.id.clone(),
                    message: "retry exhausted with no error captured".into(),
                }))
            }

            ErrorMode::Fallback => match self.dispatch(ctx).await {
                Ok(output) => Ok(output),
                Err(e) => {
                    tracing::warn!(
                        node = %std.id,
                        error = %e,
                        "action node error caught, using fallback value"
                    );
                    let fallback = error_handling.fallback_value.clone().unwrap_or(Value::Null);
                    Ok(NodeOutput::new().with_update(&std.mapping.output_key, fallback))
                }
            },
        }
    }
}

#[async_trait]
impl Node for ActionNodeExecutor {
    #[allow(clippy::misnamed_getters)]
    fn name(&self) -> &str {
        // Node::name() is used as a unique identifier, which is the `id` field
        &self.config.standard().id
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput> {
        // 1. Check skip condition
        if self.should_skip(ctx) {
            tracing::debug!(node = %self.config.standard().id, "skipping action node (condition false)");
            return Ok(NodeOutput::new());
        }

        // 2. Apply timeout
        let timeout_ms = self.config.standard().execution.timeout;
        let timeout_duration = std::time::Duration::from_millis(timeout_ms);

        match tokio::time::timeout(timeout_duration, self.execute_with_error_handling(ctx)).await {
            Ok(result) => result,
            Err(_) => Err(GraphError::NodeExecutionFailed {
                node: self.config.standard().id.clone(),
                message: format!("action node timed out after {timeout_ms}ms"),
            }),
        }
    }
}
