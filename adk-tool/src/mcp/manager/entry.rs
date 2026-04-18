//! Internal per-server state.
//!
//! Defines [`BackoffState`] for tracking exponential backoff and [`McpServerEntry`]
//! for holding all per-server state within the manager.

use super::super::elicitation::AdkClientHandler;
use super::super::toolset::McpToolset;
use super::config::{McpServerConfig, RestartPolicy};
use super::status::ServerStatus;

/// Tracks exponential backoff state for auto-restart attempts.
///
/// The backoff delay doubles after each failure (up to `max_delay_ms`) and resets
/// on a successful restart.
#[derive(Debug, Clone)]
#[allow(dead_code)] // Used by McpServerManager in manager.rs (Task 3+)
pub(crate) struct BackoffState {
    /// Number of consecutive restart failures.
    pub consecutive_failures: u32,
    /// Current delay in milliseconds before the next restart attempt.
    pub current_delay_ms: u64,
}

#[allow(dead_code)] // Used by McpServerManager in manager.rs (Task 3+)
impl BackoffState {
    /// Create a new `BackoffState` initialized from a restart policy.
    ///
    /// If no policy is provided, uses a default initial delay of 1000ms.
    pub fn new(policy: &Option<RestartPolicy>) -> Self {
        Self {
            consecutive_failures: 0,
            current_delay_ms: policy.as_ref().map_or(1000, |p| p.initial_delay_ms),
        }
    }

    /// Compute the next backoff delay and increment the failure counter.
    ///
    /// Returns the delay to wait before the next restart attempt.
    /// The delay is capped at `max_delay_ms` from the policy.
    pub fn next_delay(&mut self, policy: &RestartPolicy) -> u64 {
        let delay = self.current_delay_ms;
        self.consecutive_failures += 1;
        self.current_delay_ms = ((self.current_delay_ms as f64 * policy.backoff_multiplier) as u64)
            .min(policy.max_delay_ms);
        delay
    }

    /// Reset the backoff state after a successful restart.
    pub fn reset(&mut self, policy: &RestartPolicy) {
        self.consecutive_failures = 0;
        self.current_delay_ms = policy.initial_delay_ms;
    }

    /// Check whether the maximum number of restart attempts has been exceeded.
    pub fn exceeded_max_attempts(&self, policy: &RestartPolicy) -> bool {
        self.consecutive_failures >= policy.max_restart_attempts
    }
}

/// Internal per-server state held by the manager.
///
/// Contains the server configuration, current status, optional MCP connection,
/// optional child process handle, and backoff tracking.
#[allow(dead_code)] // Used by McpServerManager in manager.rs (Task 3+)
pub(crate) struct McpServerEntry {
    /// The server's configuration.
    pub config: McpServerConfig,
    /// Current lifecycle status.
    pub status: ServerStatus,
    /// Active MCP toolset connection, present when the server is `Running`.
    pub toolset: Option<McpToolset<AdkClientHandler>>,
    /// Child process handle, present when the server process is alive.
    pub child: Option<tokio::process::Child>,
    /// Exponential backoff state for auto-restart.
    pub backoff: BackoffState,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backoff_state_new_with_policy() {
        let policy = Some(RestartPolicy {
            initial_delay_ms: 500,
            max_delay_ms: 10000,
            backoff_multiplier: 2.0,
            max_restart_attempts: 5,
        });
        let state = BackoffState::new(&policy);
        assert_eq!(state.consecutive_failures, 0);
        assert_eq!(state.current_delay_ms, 500);
    }

    #[test]
    fn test_backoff_state_new_without_policy() {
        let state = BackoffState::new(&None);
        assert_eq!(state.consecutive_failures, 0);
        assert_eq!(state.current_delay_ms, 1000);
    }

    #[test]
    fn test_backoff_next_delay() {
        let policy = RestartPolicy {
            initial_delay_ms: 1000,
            max_delay_ms: 30000,
            backoff_multiplier: 2.0,
            max_restart_attempts: 10,
        };
        let mut state = BackoffState::new(&Some(policy.clone()));

        // First attempt: delay = 1000, next = 2000
        let delay = state.next_delay(&policy);
        assert_eq!(delay, 1000);
        assert_eq!(state.consecutive_failures, 1);
        assert_eq!(state.current_delay_ms, 2000);

        // Second attempt: delay = 2000, next = 4000
        let delay = state.next_delay(&policy);
        assert_eq!(delay, 2000);
        assert_eq!(state.consecutive_failures, 2);
        assert_eq!(state.current_delay_ms, 4000);

        // Third attempt: delay = 4000, next = 8000
        let delay = state.next_delay(&policy);
        assert_eq!(delay, 4000);
        assert_eq!(state.consecutive_failures, 3);
        assert_eq!(state.current_delay_ms, 8000);
    }

    #[test]
    fn test_backoff_delay_capped_at_max() {
        let policy = RestartPolicy {
            initial_delay_ms: 10000,
            max_delay_ms: 15000,
            backoff_multiplier: 2.0,
            max_restart_attempts: 10,
        };
        let mut state = BackoffState::new(&Some(policy.clone()));

        // First: delay = 10000, next would be 20000 but capped to 15000
        let delay = state.next_delay(&policy);
        assert_eq!(delay, 10000);
        assert_eq!(state.current_delay_ms, 15000);

        // Second: delay = 15000, next would be 30000 but capped to 15000
        let delay = state.next_delay(&policy);
        assert_eq!(delay, 15000);
        assert_eq!(state.current_delay_ms, 15000);
    }

    #[test]
    fn test_backoff_reset() {
        let policy = RestartPolicy {
            initial_delay_ms: 1000,
            max_delay_ms: 30000,
            backoff_multiplier: 2.0,
            max_restart_attempts: 10,
        };
        let mut state = BackoffState::new(&Some(policy.clone()));

        // Simulate some failures
        state.next_delay(&policy);
        state.next_delay(&policy);
        assert_eq!(state.consecutive_failures, 2);
        assert_eq!(state.current_delay_ms, 4000);

        // Reset
        state.reset(&policy);
        assert_eq!(state.consecutive_failures, 0);
        assert_eq!(state.current_delay_ms, 1000);
    }

    #[test]
    fn test_backoff_exceeded_max_attempts() {
        let policy = RestartPolicy {
            initial_delay_ms: 1000,
            max_delay_ms: 30000,
            backoff_multiplier: 2.0,
            max_restart_attempts: 3,
        };
        let mut state = BackoffState::new(&Some(policy.clone()));

        assert!(!state.exceeded_max_attempts(&policy));

        state.next_delay(&policy); // 1
        assert!(!state.exceeded_max_attempts(&policy));

        state.next_delay(&policy); // 2
        assert!(!state.exceeded_max_attempts(&policy));

        state.next_delay(&policy); // 3
        assert!(state.exceeded_max_attempts(&policy));
    }

    #[test]
    fn test_mcp_server_entry_creation() {
        let config = McpServerConfig {
            command: "echo".to_string(),
            args: vec![],
            env: std::collections::HashMap::new(),
            disabled: false,
            auto_approve: vec![],
            restart_policy: None,
        };
        let entry = McpServerEntry {
            backoff: BackoffState::new(&config.restart_policy),
            config,
            status: ServerStatus::Stopped,
            toolset: None,
            child: None,
        };
        assert_eq!(entry.status, ServerStatus::Stopped);
        assert!(entry.toolset.is_none());
        assert!(entry.child.is_none());
    }
}
