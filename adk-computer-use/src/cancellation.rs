//! Cancellation that revokes desktop authority *before* stopping ADK reasoning.
//!
//! [`CancellationBridge`] pairs a [`ComputerUseRuntime`] with an
//! [`AgentInterrupter`] (typically `adk_runner::Runner::interrupt`). On pause,
//! stop, or emergency-stop it first revokes the runtime's desktop authority,
//! then interrupts the agent — never the other way around.

use crate::ComputerUseRuntime;
use std::sync::Arc;
use thiserror::Error;

/// Minimal boundary implemented by `adk_runner::Runner::interrupt` or another host.
///
/// Blanket-implemented for any `Fn(&str) -> bool + Send + Sync`, so a closure
/// can be passed directly.
pub trait AgentInterrupter: Send + Sync {
    /// Interrupt the ADK session, returning whether an active run was cancelled.
    fn interrupt(&self, session_id: &str) -> bool;
}

impl<F> AgentInterrupter for F
where
    F: Fn(&str) -> bool + Send + Sync,
{
    fn interrupt(&self, session_id: &str) -> bool {
        self(session_id)
    }
}

/// Failure propagating a cancellation control to the desktop runtime.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum CancellationError {
    /// The desktop runtime rejected or failed the cancellation control.
    #[error("desktop cancellation failed: {0}")]
    Runtime(String),
}

/// Propagates controls in the safe order: revoke desktop authority, then stop reasoning.
pub struct CancellationBridge {
    runtime: Arc<dyn ComputerUseRuntime>,
    interrupter: Arc<dyn AgentInterrupter>,
}

impl CancellationBridge {
    /// Pair a desktop runtime with an agent interrupter.
    pub fn new(
        runtime: Arc<dyn ComputerUseRuntime>,
        interrupter: Arc<dyn AgentInterrupter>,
    ) -> Self {
        Self { runtime, interrupter }
    }

    /// Pause desktop authority for the runtime session, then interrupt the ADK agent.
    ///
    /// # Errors
    ///
    /// Returns [`CancellationError::Runtime`] if the pause call fails; the
    /// ADK agent is not interrupted in that case.
    pub async fn pause(
        &self,
        runtime_session_id: &str,
        adk_session_id: &str,
        reason: &str,
    ) -> Result<bool, CancellationError> {
        self.runtime
            .pause_session(runtime_session_id, reason)
            .await
            .map_err(|error| CancellationError::Runtime(error.to_string()))?;
        Ok(self.interrupter.interrupt(adk_session_id))
    }

    /// Stop desktop authority for the runtime session, then interrupt the ADK agent.
    ///
    /// # Errors
    ///
    /// Returns [`CancellationError::Runtime`] if the stop call fails.
    pub async fn stop(
        &self,
        runtime_session_id: &str,
        adk_session_id: &str,
        reason: &str,
    ) -> Result<bool, CancellationError> {
        self.runtime
            .stop_session(runtime_session_id, reason)
            .await
            .map_err(|error| CancellationError::Runtime(error.to_string()))?;
        Ok(self.interrupter.interrupt(adk_session_id))
    }

    /// Revoke all desktop authority immediately, then interrupt the ADK agent.
    ///
    /// # Errors
    ///
    /// Returns [`CancellationError::Runtime`] if the emergency-stop call fails.
    pub async fn emergency_stop(
        &self,
        adk_session_id: &str,
        reason: &str,
    ) -> Result<bool, CancellationError> {
        self.runtime
            .emergency_stop(reason)
            .await
            .map_err(|error| CancellationError::Runtime(error.to_string()))?;
        Ok(self.interrupter.interrupt(adk_session_id))
    }
}
