use crate::ComputerUseRuntime;
use std::sync::Arc;
use thiserror::Error;

/// Minimal boundary implemented by `adk_runner::Runner::interrupt` or another host.
pub trait AgentInterrupter: Send + Sync {
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

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CancellationError {
    #[error("v8 cancellation failed: {0}")]
    Runtime(String),
}

/// Propagates controls in the safe order: revoke desktop authority, then stop reasoning.
pub struct CancellationBridge {
    runtime: Arc<dyn ComputerUseRuntime>,
    interrupter: Arc<dyn AgentInterrupter>,
}

impl CancellationBridge {
    pub fn new(
        runtime: Arc<dyn ComputerUseRuntime>,
        interrupter: Arc<dyn AgentInterrupter>,
    ) -> Self {
        Self { runtime, interrupter }
    }

    pub async fn pause(
        &self,
        v8_session_id: &str,
        adk_session_id: &str,
        reason: &str,
    ) -> Result<bool, CancellationError> {
        self.runtime
            .pause_session(v8_session_id, reason)
            .await
            .map_err(CancellationError::Runtime)?;
        Ok(self.interrupter.interrupt(adk_session_id))
    }

    pub async fn stop(
        &self,
        v8_session_id: &str,
        adk_session_id: &str,
        reason: &str,
    ) -> Result<bool, CancellationError> {
        self.runtime
            .stop_session(v8_session_id, reason)
            .await
            .map_err(CancellationError::Runtime)?;
        Ok(self.interrupter.interrupt(adk_session_id))
    }

    pub async fn emergency_stop(
        &self,
        adk_session_id: &str,
        reason: &str,
    ) -> Result<bool, CancellationError> {
        self.runtime.emergency_stop(reason).await.map_err(CancellationError::Runtime)?;
        Ok(self.interrupter.interrupt(adk_session_id))
    }
}
