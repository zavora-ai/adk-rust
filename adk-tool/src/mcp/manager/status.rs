//! Server status tracking.
//!
//! Defines the [`ServerStatus`] enum representing the current lifecycle state
//! of a managed MCP server.

use serde::{Deserialize, Serialize};

/// Current lifecycle state of a managed MCP server.
///
/// Transitions:
/// - `Stopped`, `Crashed`, `FailedToStart`, or `Restarting` → `Starting` (a start begins; the
///   process is spawned and the MCP handshake runs in this state)
/// - `Starting` → `Running` (the handshake completed)
/// - `Starting` → `FailedToStart` (spawn or handshake failure, or the startup timeout)
/// - `Starting` → `Stopped` (stopped while it was starting)
/// - `Running` → `Stopped` (on manual stop or shutdown)
/// - `Running` → `Crashed` (the health check found the connection closed)
/// - `Running` → `Restarting` (on `restart_server`)
/// - `Crashed` or `FailedToStart` → `Restarting` (on auto-restart, after the backoff)
/// - `Crashed` → `FailedToStart` (the restart policy's attempts are exhausted)
/// - any → `Disabled` (on `disable_server`, or a start of a server configured `disabled`);
///   `Disabled` → `Stopped` (on `enable_server`)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ServerStatus {
    /// The server process has been spawned and its MCP handshake is in progress.
    Starting,
    /// The server process is running and the MCP connection is active.
    Running,
    /// The server has been stopped (manually or via shutdown).
    Stopped,
    /// The server process exited unexpectedly or a health check failed.
    Crashed,
    /// The server is in the process of being restarted.
    Restarting,
    /// The server configuration has `disabled: true`; it will not be started.
    Disabled,
    /// The server failed to start (spawn failure, handshake failure or timeout, or max
    /// restarts exceeded).
    FailedToStart,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_server_status_clone() {
        let status = ServerStatus::Running;
        let cloned = status;
        assert_eq!(status, cloned);
    }

    #[test]
    fn test_server_status_debug() {
        let status = ServerStatus::Crashed;
        let debug_str = format!("{status:?}");
        assert_eq!(debug_str, "Crashed");
    }

    #[test]
    fn test_server_status_eq() {
        assert_eq!(ServerStatus::Running, ServerStatus::Running);
        assert_ne!(ServerStatus::Running, ServerStatus::Stopped);
    }

    #[test]
    fn test_server_status_hash() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(ServerStatus::Running);
        set.insert(ServerStatus::Stopped);
        set.insert(ServerStatus::Running); // duplicate
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn test_server_status_serde_round_trip() {
        let statuses = [
            ServerStatus::Starting,
            ServerStatus::Running,
            ServerStatus::Stopped,
            ServerStatus::Crashed,
            ServerStatus::Restarting,
            ServerStatus::Disabled,
            ServerStatus::FailedToStart,
        ];
        for status in &statuses {
            let json = serde_json::to_string(status).unwrap();
            let deserialized: ServerStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(*status, deserialized);
        }
    }
}
