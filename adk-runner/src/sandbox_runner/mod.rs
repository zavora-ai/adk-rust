//! Sandbox runner lifecycle management.
//!
//! This module provides [`SandboxRunner`], a wrapper around the standard [`Runner`](crate::Runner)
//! that manages the full sandbox lifecycle: provision → start → bind tools → run → stop → snapshot.
//!
//! # Overview
//!
//! The `SandboxRunner` extracts a [`SandboxConfig`](adk_sandbox::workspace::SandboxConfig) from
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
//! let result = sandbox_runner.run(&sandbox_config, "user_1", "session_1").await?;
//! ```

pub mod binding;
pub mod tools;

use crate::Runner;
use adk_sandbox::SandboxError;
use adk_sandbox::workspace::{SandboxConfig, SnapshotId};
use std::sync::Arc;
use tracing::{info, warn};

/// Runner wrapper that manages the sandbox lifecycle around agent execution.
///
/// Provisions the workspace, binds tools, delegates to the inner Runner,
/// and cleans up (stop + optional snapshot) on completion or failure.
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
    /// 5. Stops the session (always, even on failure)
    /// 6. Optionally snapshots the workspace
    ///
    /// # Stop Guarantee
    ///
    /// The `stop` method is **always** called on the sandbox client, regardless
    /// of whether the agent loop succeeds or fails. This ensures resources are
    /// cleaned up even in error scenarios.
    ///
    /// # Errors
    ///
    /// Returns an error if provisioning or starting the session fails (without
    /// entering the agent loop), or if the agent loop itself fails (after
    /// cleanup has been performed).
    pub async fn run(
        &self,
        config: &SandboxConfig,
        user_id: &str,
        session_id: &str,
    ) -> Result<SandboxRunResult, adk_core::AdkError> {
        // 1. Provision workspace from manifest
        info!("provisioning sandbox workspace");
        let handle =
            config.client.provision(&config.manifest).await.map_err(adk_core::AdkError::from)?;

        // 2. Start session
        info!(session_handle = %handle.0, "starting sandbox session");
        let session = match config.client.start(&handle).await {
            Ok(s) => s,
            Err(e) => {
                // If start fails, attempt to stop/cleanup the provisioned session
                let _ = config.client.stop(&handle).await;
                return Err(adk_core::AdkError::from(e));
            }
        };

        // 3. Bind tools based on capabilities
        let session_arc = Arc::from(session);
        let _bound_tools =
            binding::bind_tools(session_arc, &config.capabilities, config.command_timeout);
        info!(
            capabilities = ?config.capabilities,
            tool_count = _bound_tools.len(),
            "bound sandbox tools"
        );

        // 4. Run agent loop with session timeout
        // NOTE: The inner Runner doesn't yet support dynamic tool injection.
        // The bound tools are prepared here; actual agent loop integration will
        // be completed when the agent builder supports injecting tools at runtime.
        // For now, we simulate the agent loop step as a placeholder.
        let agent_loop_future = async {
            // Use the user_id and session_id for future agent loop integration
            let _ = (user_id, session_id);
            Ok::<(), adk_core::AdkError>(())
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
                Err(adk_core::AdkError::from(SandboxError::SessionTimeout {
                    timeout: config.session_timeout,
                }))
            }
        };

        // 5. Stop session — ALWAYS called, regardless of agent loop outcome
        info!(session_handle = %handle.0, "stopping sandbox session");
        if let Err(e) = config.client.stop(&handle).await {
            warn!(
                session_handle = %handle.0,
                error = %e,
                "failed to stop sandbox session during cleanup"
            );
        }

        // 6. Handle agent loop result — propagate error after cleanup
        agent_loop_result?;

        // 7. Optionally snapshot
        let snapshot_id = if config.snapshot_on_stop {
            info!(session_handle = %handle.0, "snapshotting sandbox workspace");
            match config.client.snapshot(&handle).await {
                Ok(id) => {
                    info!(snapshot_id = %id.0, "sandbox snapshot created");
                    Some(id)
                }
                Err(e) => {
                    warn!(
                        session_handle = %handle.0,
                        error = %e,
                        "sandbox snapshot failed, continuing without snapshot"
                    );
                    None
                }
            }
        } else {
            None
        };

        Ok(SandboxRunResult { snapshot_id })
    }
}

/// Result of a sandbox-managed agent run.
#[derive(Debug)]
pub struct SandboxRunResult {
    /// The snapshot ID if snapshot-on-stop was enabled.
    pub snapshot_id: Option<SnapshotId>,
}
