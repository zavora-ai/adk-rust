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
    /// Directories a file node may touch; empty means the working directory.
    file_roots: Vec<std::path::PathBuf>,
    /// URLs the HTTP, notification, and RSS nodes may request.
    #[cfg(feature = "action-http")]
    http_policy: http::HttpActionPolicy,
}

impl ActionNodeExecutor {
    /// Create a new executor wrapping the given action node config.
    ///
    /// A file node is confined to the current working directory, and the HTTP,
    /// notification, and RSS nodes to `https`/`http` URLs; see
    /// [`Self::with_file_roots`] and `with_http_policy`.
    pub fn new(config: ActionNodeConfig) -> Self {
        Self {
            config,
            file_roots: Vec::new(),
            #[cfg(feature = "action-http")]
            http_policy: http::HttpActionPolicy::default(),
        }
    }

    /// Confines a file node to these directories instead of the working directory.
    ///
    /// A relative path resolves against the first root. A path that resolves
    /// outside every root, through `..` or a symbolic link, fails the node; see
    /// [`mod@file`] for the full rules.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_graph::action::ActionNodeExecutor;
    ///
    /// let executor = ActionNodeExecutor::new(config).with_file_roots(["/srv/workflow-data"]);
    /// ```
    #[must_use]
    pub fn with_file_roots<I, P>(mut self, roots: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<std::path::PathBuf>,
    {
        self.file_roots = roots.into_iter().map(Into::into).collect();
        self
    }

    /// Sets which URLs the HTTP, notification, and RSS nodes may request, and how
    /// redirects are followed.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_graph::action::ActionNodeExecutor;
    /// use adk_graph::action::http::HttpActionPolicy;
    ///
    /// let executor = ActionNodeExecutor::new(config)
    ///     .with_http_policy(HttpActionPolicy::new().allow_hosts(["api.example.com"]));
    /// ```
    #[cfg(feature = "action-http")]
    #[must_use]
    pub fn with_http_policy(mut self, policy: http::HttpActionPolicy) -> Self {
        self.http_policy = policy;
        self
    }

    /// Returns a reference to the inner config.
    pub fn config(&self) -> &ActionNodeConfig {
        &self.config
    }

    /// Why this node cannot execute, if it cannot.
    ///
    /// Some action variants accept and validate a configuration while their backend is
    /// still a placeholder, and some require a feature that is not compiled in. Both
    /// used to surface only when the node ran, which is operationally different from a
    /// configuration rejected before the workflow starts.
    pub fn unavailable_reason(&self) -> Option<String> {
        match &self.config {
            ActionNodeConfig::Database(config) => Some(format!(
                "database node '{}' cannot execute: database action nodes are not \
                 implemented in any feature configuration",
                config.standard.id
            )),
            ActionNodeConfig::Email(config) => Some(format!(
                "email node '{}' cannot execute: email action nodes are not implemented \
                 in any feature configuration",
                config.standard.id
            )),
            ActionNodeConfig::Code(config)
                if matches!(
                    config.language,
                    adk_action::CodeLanguage::Javascript | adk_action::CodeLanguage::Typescript
                ) =>
            {
                Some(format!(
                    "code node '{}' cannot execute: JavaScript and TypeScript code nodes are \
                     not implemented in any feature configuration; use language 'rust'",
                    config.standard.id
                ))
            }
            #[cfg(not(feature = "action-http"))]
            ActionNodeConfig::Http(config) => Some(format!(
                "http node '{}' cannot execute: the 'action-http' feature is not enabled",
                config.standard.id
            )),
            _ => None,
        }
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
            ActionNodeConfig::File(c) => file::execute_file_in(c, ctx, &self.file_roots).await,
            ActionNodeConfig::Code(c) => code::execute_code(c, ctx).await,

            #[cfg(feature = "action-http")]
            ActionNodeConfig::Http(c) => {
                http::execute_http_with_policy(c, ctx, &self.http_policy).await
            }
            #[cfg(not(feature = "action-http"))]
            ActionNodeConfig::Http(_) => Err(GraphError::NodeExecutionFailed {
                node: self.config.standard().id.clone(),
                message: "HTTP node requires the 'action-http' feature".into(),
            }),

            #[cfg(feature = "action-db")]
            ActionNodeConfig::Database(c) => database::execute_database(c, ctx).await,
            // Not "enable the feature": the feature compiles a placeholder that
            // fails the same way, so naming it would send the caller nowhere.
            #[cfg(not(feature = "action-db"))]
            ActionNodeConfig::Database(_) => Err(GraphError::NodeExecutionFailed {
                node: self.config.standard().id.clone(),
                message: "database action nodes are not implemented; no database driver is \
                          integrated"
                    .into(),
            }),

            #[cfg(feature = "action-email")]
            ActionNodeConfig::Email(c) => email::execute_email(c, ctx).await,
            #[cfg(not(feature = "action-email"))]
            ActionNodeConfig::Email(_) => Err(GraphError::NodeExecutionFailed {
                node: self.config.standard().id.clone(),
                message: "email action nodes are not implemented; neither IMAP monitoring nor \
                          SMTP sending is integrated"
                    .into(),
            }),

            #[cfg(feature = "action-http")]
            ActionNodeConfig::Notification(c) => {
                notification::execute_notification_with_policy(c, ctx, &self.http_policy).await
            }
            #[cfg(not(feature = "action-http"))]
            ActionNodeConfig::Notification(_) => Err(GraphError::NodeExecutionFailed {
                node: self.config.standard().id.clone(),
                message: "Notification node requires the 'action-http' feature".into(),
            }),

            #[cfg(feature = "action-rss")]
            ActionNodeConfig::Rss(c) => {
                rss::execute_rss_with_policy(c, ctx, &self.http_policy).await
            }
            #[cfg(not(feature = "action-rss"))]
            ActionNodeConfig::Rss(_) => Err(GraphError::NodeExecutionFailed {
                node: self.config.standard().id.clone(),
                message: "RSS node requires the 'action-rss' feature".into(),
            }),
        }
    }

    /// Applies the skip condition, the timeout and the error handling around `run`.
    ///
    /// A governed node's call runs once (spec 3.3 §12): its error handling neither retries
    /// it nor swallows or replaces its failure, so the action ledger records what happened,
    /// and its timeout is a [`GraphError::NodeTimedOut`], an outcome the bracket keeps
    /// unknown because the call may have taken effect.
    async fn run_guarded<F, Fut>(&self, ctx: &NodeContext, run: F) -> Result<NodeOutput>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<NodeOutput>>,
    {
        // 1. Check skip condition
        if self.should_skip(ctx) {
            tracing::debug!(node = %self.config.standard().id, "skipping action node (condition false)");
            return Ok(NodeOutput::new());
        }

        // 2. Apply timeout
        let timeout_ms = self.config.standard().execution.timeout;
        let timeout_duration = std::time::Duration::from_millis(timeout_ms);

        if ctx.governed_call() {
            return match tokio::time::timeout(timeout_duration, run()).await {
                Ok(result) => result,
                Err(_) => Err(GraphError::NodeTimedOut {
                    node: self.config.standard().id.clone(),
                    elapsed: timeout_duration,
                }),
            };
        }

        match tokio::time::timeout(timeout_duration, self.execute_with_error_handling(run)).await {
            Ok(result) => result,
            Err(_) => Err(GraphError::NodeExecutionFailed {
                node: self.config.standard().id.clone(),
                message: format!("action node timed out after {timeout_ms}ms"),
            }),
        }
    }

    /// Apply error handling around `run`, the dispatch or an approved call.
    ///
    /// A governed refusal is never swallowed or retried: the call was not sent.
    async fn execute_with_error_handling<F, Fut>(&self, run: F) -> Result<NodeOutput>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<NodeOutput>>,
    {
        let std = self.config.standard();
        let error_handling = &std.error_handling;
        let governed = |e: &GraphError| crate::governed::GovernedError::code_of(e).is_some();

        match error_handling.mode {
            ErrorMode::Stop => run().await,

            ErrorMode::Continue => match run().await {
                Ok(output) => Ok(output),
                Err(e) if governed(&e) => Err(e),
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
                    match run().await {
                        Ok(output) => return Ok(output),
                        Err(e) if governed(&e) => return Err(e),
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

            ErrorMode::Fallback => match run().await {
                Ok(output) => Ok(output),
                Err(e) if governed(&e) => Err(e),
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
    fn validate(&self) -> Result<()> {
        match self.unavailable_reason() {
            Some(message) => Err(GraphError::InvalidGraph(message)),
            None => Ok(()),
        }
    }

    #[allow(clippy::misnamed_getters)]
    fn name(&self) -> &str {
        // Node::name() is used as a unique identifier, which is the `id` field
        &self.config.standard().id
    }

    fn kind(&self) -> crate::shape::NodeKind {
        use crate::shape::NodeKind;
        match &self.config {
            ActionNodeConfig::Trigger(_) => NodeKind::Trigger,
            ActionNodeConfig::Http(_) => NodeKind::HttpCall,
            ActionNodeConfig::Set(_) => NodeKind::Set,
            ActionNodeConfig::Transform(_) => NodeKind::Transform,
            ActionNodeConfig::Switch(_) => NodeKind::Switch,
            ActionNodeConfig::Loop(_) => NodeKind::Loop,
            ActionNodeConfig::Merge(_) => NodeKind::Merge,
            ActionNodeConfig::Wait(_) => NodeKind::Wait,
            ActionNodeConfig::Code(_) => NodeKind::Code,
            ActionNodeConfig::Database(_) => NodeKind::Database,
            ActionNodeConfig::Email(_) => NodeKind::Email,
            ActionNodeConfig::Notification(_) => NodeKind::Notification,
            ActionNodeConfig::Rss(_) => NodeKind::Rss,
            ActionNodeConfig::File(_) => NodeKind::File,
        }
    }

    /// `false` when the node's condition is false, so a governed node asks for no decision;
    /// an error when an HTTP node's request cannot be built (spec 3.3 §12).
    fn will_act(&self, ctx: &NodeContext) -> Result<bool> {
        if self.should_skip(ctx) {
            return Ok(false);
        }
        match (&self.config, ctx) {
            #[cfg(feature = "action-http")]
            (ActionNodeConfig::Http(config), ctx) => {
                http::buildable(config, &ctx.state, &self.http_policy).map(|()| true)
            }
            _ => Ok(true),
        }
    }

    /// The HTTP node's call with its credentials as digests (spec 3.3 §12); `None` for
    /// other action nodes, whose call is bound to the state they read.
    fn call_args(&self, ctx: &NodeContext) -> Option<Value> {
        match (&self.config, ctx) {
            #[cfg(feature = "action-http")]
            (ActionNodeConfig::Http(config), ctx) => {
                http::call_args(config, &ctx.state, &self.http_policy)
            }
            _ => None,
        }
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput> {
        self.run_guarded(ctx, || self.dispatch(ctx)).await
    }

    /// Sends exactly the approved HTTP call, with each credential digest replaced by the
    /// current credential (spec 3.3 §7.1). Other action nodes cannot run an edited call.
    async fn run_call(&self, ctx: &NodeContext, args: Value) -> Result<NodeOutput> {
        match (&self.config, ctx, args) {
            #[cfg(feature = "action-http")]
            (ActionNodeConfig::Http(config), ctx, args) => {
                self.run_guarded(ctx, || {
                    http::run_call(
                        config,
                        &ctx.state,
                        &self.http_policy,
                        &args,
                        ctx.governed_call(),
                    )
                })
                .await
            }
            _ => Err(crate::governed::GovernedError::EditUnsupported {
                node: self.config.standard().id.clone(),
            }
            .into()),
        }
    }
}
