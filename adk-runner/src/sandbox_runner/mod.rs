//! Sandbox runner lifecycle management.
//!
//! This module provides [`SandboxRunner`], a wrapper around the standard [`Runner`]
//! that manages the full sandbox lifecycle: provision → start → bind tools → run → snapshot → stop.
//!
//! # Overview
//!
//! The `SandboxRunner` extracts a [`SandboxConfig`] from
//! the agent, provisions a workspace, binds shell and filesystem tools based on enabled
//! capabilities, delegates execution to the inner runner, and guarantees cleanup (stop) even
//! on failure.
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_runner::sandbox_runner::SandboxRunner;
//! use adk_runner::Runner;
//! use adk_sandbox::workspace::SandboxConfig;
//!
//! let runner = Runner::new(config)?;
//! let sandbox_runner = SandboxRunner::new(runner);
//! let content = adk_core::Content::new("user").with_text("list the files");
//! let result = sandbox_runner
//!     .run(&sandbox_config, "user_1", "session_1", content)
//!     .await?;
//! ```

pub mod binding;
pub mod tools;

use crate::Runner;
use adk_core::{AdkError, AppName, Event, SessionId, UserId};
use adk_sandbox::SandboxError;
use adk_sandbox::workspace::{SandboxConfig, SnapshotId};
use serde_json::{Value, json};
use std::sync::Arc;

use futures::StreamExt;
use tracing::{info, warn};

/// Exposes the tools bound to a live sandbox session as a [`Toolset`].
///
/// The tools hold the session handle, so they are valid only for the run that created them and
/// are injected per-invocation rather than attached to the agent.
struct SandboxToolset {
    tools: Vec<Arc<dyn adk_core::Tool>>,
}

#[async_trait::async_trait]
impl adk_core::Toolset for SandboxToolset {
    fn name(&self) -> &str {
        "sandbox"
    }

    async fn tools(
        &self,
        _ctx: Arc<dyn adk_core::ReadonlyContext>,
    ) -> adk_core::Result<Vec<Arc<dyn adk_core::Tool>>> {
        Ok(self.tools.clone())
    }
}

fn attach_cleanup_failure(primary: &mut AdkError, stage: &'static str, cleanup: &AdkError) {
    let failure = json!({
        "stage": stage,
        "component": cleanup.component.to_string(),
        "category": cleanup.category.to_string(),
        "code": cleanup.code,
        "message": cleanup.message,
    });
    let entry = primary
        .details
        .metadata
        .entry("sandbox.cleanupErrors".to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    if let Value::Array(failures) = entry {
        failures.push(failure);
    } else {
        *entry = Value::Array(vec![failure]);
    }
}

/// Runner wrapper that manages the sandbox lifecycle around agent execution.
///
/// Provisions the workspace, binds tools, delegates to the inner Runner, snapshots when
/// configured, and stops the sandbox on completion or failure.
pub struct SandboxRunner {
    inner: Runner,
}

impl SandboxRunner {
    /// Creates a new `SandboxRunner` wrapping the given [`Runner`].
    pub fn new(inner: Runner) -> Self {
        Self { inner }
    }

    /// Returns a reference to the inner [`Runner`].
    pub fn inner(&self) -> &Runner {
        &self.inner
    }

    /// Runs the agent with full sandbox lifecycle management.
    ///
    /// Manages the complete sandbox lifecycle:
    /// 1. Provisions workspace from the config's manifest
    /// 2. Starts the sandbox session
    /// 3. Binds tools based on enabled capabilities
    /// 4. Runs the agent loop via the inner Runner
    /// 5. Optionally snapshots the workspace while the session is live
    /// 6. Stops the session (always after a handle has been provisioned)
    ///
    /// # Stop Guarantee
    ///
    /// The `stop` method is called on the sandbox client after every successful provision,
    /// regardless of whether start, execution, or snapshotting succeeds. This ensures resources
    /// are cleaned up in error scenarios.
    ///
    /// Error precedence follows lifecycle order: an execution error takes precedence over a
    /// snapshot error, which takes precedence over a stop error. Later cleanup failures are
    /// retained in the returned error's `sandbox.cleanupErrors` metadata.
    ///
    /// # Errors
    ///
    /// Returns an error if identity validation, session lookup or creation, provisioning,
    /// starting, agent execution, snapshotting, or stopping fails. Cleanup completes before an
    /// execution error is returned.
    pub async fn run(
        &self,
        config: &SandboxConfig,
        user_id: &str,
        session_id: &str,
        user_content: adk_core::Content,
    ) -> Result<SandboxRunResult, adk_core::AdkError> {
        // Validate caller-controlled identity before any session or sandbox side effect.
        let app_name = AppName::try_from(self.inner.app_name())?;
        let user_id = UserId::try_from(user_id)?;
        let session_id = SessionId::try_from(session_id)?;

        let get_request = adk_session::GetRequest {
            app_name: app_name.to_string(),
            user_id: user_id.to_string(),
            session_id: session_id.to_string(),
            num_recent_events: None,
            after: None,
        };
        let create_session = match self.inner.session_service().get(get_request).await {
            Ok(_) => false,
            Err(error) if error.is_not_found() => true,
            Err(error) => return Err(error),
        };

        // 1. Provision workspace from manifest
        info!("provisioning sandbox workspace");
        let handle =
            config.client.provision(&config.manifest).await.map_err(adk_core::AdkError::from)?;

        // 2. Start session
        info!(session_handle = %handle.0, "starting sandbox session");
        let session = match config.client.start(&handle).await {
            Ok(s) => s,
            Err(e) => {
                let mut primary = AdkError::from(e);
                if let Err(cleanup) = config.client.stop(&handle).await {
                    warn!(
                        session_handle = %handle.0,
                        error = %cleanup,
                        "failed to stop sandbox after start failure"
                    );
                    attach_cleanup_failure(&mut primary, "stop", &AdkError::from(cleanup));
                }
                return Err(primary);
            }
        };

        // 3. Bind tools based on capabilities
        let session_arc = Arc::from(session);
        let bound_tools =
            binding::bind_tools(session_arc, &config.capabilities, config.command_timeout);
        info!(
            capabilities = ?config.capabilities,
            tool_count = bound_tools.len(),
            "bound sandbox tools"
        );

        // 4. Run the agent loop with the sandbox tools injected, under the session timeout.
        //
        // The tools exist only while this session is live, so they are supplied per-invocation
        // through `runtime_toolsets` rather than baked into the agent.
        let mut run_config = self.inner.run_config().clone();
        run_config
            .runtime_toolsets
            .push(adk_core::RuntimeToolset::new(Arc::new(SandboxToolset { tools: bound_tools })));

        let agent_loop_future = async {
            if create_session {
                self.inner
                    .session_service()
                    .create(adk_session::CreateRequest {
                        app_name: app_name.to_string(),
                        user_id: user_id.to_string(),
                        session_id: Some(session_id.to_string()),
                        state: std::collections::HashMap::new(),
                    })
                    .await?;
            }

            let mut events = self
                .inner
                .run_with_config(user_id, session_id, user_content, Some(run_config))
                .await?;
            // Drain the stream so the agent runs to completion before the session is stopped;
            // returning early would tear the sandbox down underneath the agent.
            let mut buffered_events = Vec::new();
            while let Some(event) = events.next().await {
                buffered_events.push(event?);
            }
            Ok::<Vec<Event>, adk_core::AdkError>(buffered_events)
        };

        let agent_loop_result =
            tokio::time::timeout(config.session_timeout, agent_loop_future).await;

        // Convert timeout to SandboxError::SessionTimeout
        let agent_loop_result = match agent_loop_result {
            Ok(result) => result,
            Err(_elapsed) => {
                warn!(
                    session_handle = %handle.0,
                    timeout = ?config.session_timeout,
                    "sandbox session timed out"
                );
                Err::<Vec<Event>, adk_core::AdkError>(adk_core::AdkError::from(
                    SandboxError::SessionTimeout { timeout: config.session_timeout },
                ))
            }
        };

        // 5. Snapshot while the live session still owns the workspace.
        let (snapshot_id, snapshot_error) = if config.snapshot_on_stop {
            info!(session_handle = %handle.0, "snapshotting sandbox workspace");
            match config.client.snapshot(&handle).await {
                Ok(id) => {
                    info!(snapshot_id = %id.0, "sandbox snapshot created");
                    (Some(id), None)
                }
                Err(error) => {
                    warn!(
                        session_handle = %handle.0,
                        error = %error,
                        "failed to snapshot sandbox during cleanup"
                    );
                    (None, Some(AdkError::from(error)))
                }
            }
        } else {
            (None, None)
        };

        // 6. Stop session — always called, regardless of agent or snapshot outcome.
        info!(session_handle = %handle.0, "stopping sandbox session");
        let stop_error = match config.client.stop(&handle).await {
            Ok(()) => None,
            Err(error) => {
                warn!(
                    session_handle = %handle.0,
                    error = %error,
                    "failed to stop sandbox session during cleanup"
                );
                Some(AdkError::from(error))
            }
        };

        match (agent_loop_result, snapshot_error, stop_error) {
            (Err(mut primary), snapshot_error, stop_error) => {
                if let Some(error) = snapshot_error.as_ref() {
                    attach_cleanup_failure(&mut primary, "snapshot", error);
                }
                if let Some(error) = stop_error.as_ref() {
                    attach_cleanup_failure(&mut primary, "stop", error);
                }
                Err(primary)
            }
            (Ok(_), Some(mut primary), stop_error) => {
                if let Some(error) = stop_error.as_ref() {
                    attach_cleanup_failure(&mut primary, "stop", error);
                }
                Err(primary)
            }
            (Ok(_), None, Some(primary)) => Err(primary),
            (Ok(events), None, None) => Ok(SandboxRunResult { snapshot_id, events }),
        }
    }
}

/// Result of a sandbox-managed agent run.
#[derive(Debug)]
pub struct SandboxRunResult {
    /// The snapshot ID if snapshot-on-stop was enabled.
    pub snapshot_id: Option<SnapshotId>,
    /// Agent events in the order they were emitted.
    ///
    /// Events are buffered because the sandbox is stopped before this result is returned. This
    /// keeps tool-bearing event streams from outliving the sandbox session they depend on.
    pub events: Vec<Event>,
}
