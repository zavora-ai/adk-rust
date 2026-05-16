//! Permission bridge between ADK ToolConfirmation and ACP RequestPermission.
//!
//! When the ADK Runner encounters a tool requiring confirmation, the bridge:
//! 1. Sends a RequestPermission to the ACP client
//! 2. Waits for the client response (or timeout)
//! 3. Returns the decision to the caller

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, oneshot};

use super::error::AcpServerError;

/// Outcome of a permission request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionOutcome {
    /// The client approved the tool call.
    Approved,
    /// The client denied the tool call.
    Denied,
    /// No response arrived within the configured timeout.
    Timeout,
}

/// A pending permission request waiting for client response.
struct PendingPermission {
    #[allow(dead_code)]
    tool_name: String,
    response_tx: oneshot::Sender<bool>,
}

/// Bridges ADK ToolConfirmation with ACP RequestPermission.
///
/// Manages pending permission requests indexed by `function_call_id`.
/// Each request creates a oneshot channel; the caller awaits the response
/// or times out.
///
/// # Example
///
/// ```rust,ignore
/// let bridge = PermissionBridge::new(Duration::from_secs(120));
///
/// // In the handler task:
/// let outcome = bridge.request_permission("delete_file", "fc-1", &args).await;
///
/// // When client responds:
/// bridge.resolve_permission("fc-1", true).await?;
/// ```
pub struct PermissionBridge {
    timeout: Duration,
    pending: Arc<Mutex<HashMap<String, PendingPermission>>>,
}

impl PermissionBridge {
    /// Create a new permission bridge with the given timeout.
    pub fn new(timeout: Duration) -> Self {
        Self { timeout, pending: Arc::new(Mutex::new(HashMap::new())) }
    }

    /// Request permission for a tool call.
    ///
    /// Creates a oneshot channel, stores the pending request, and waits
    /// for a response or timeout. Returns the outcome.
    pub async fn request_permission(
        &self,
        tool_name: &str,
        function_call_id: &str,
        _args: &serde_json::Value,
    ) -> PermissionOutcome {
        let (tx, rx) = oneshot::channel();

        {
            let mut pending = self.pending.lock().await;
            pending.insert(
                function_call_id.to_string(),
                PendingPermission { tool_name: tool_name.to_string(), response_tx: tx },
            );
        }

        // Wait for response or timeout
        match tokio::time::timeout(self.timeout, rx).await {
            Ok(Ok(approved)) => {
                if approved {
                    PermissionOutcome::Approved
                } else {
                    PermissionOutcome::Denied
                }
            }
            Ok(Err(_)) => {
                // Channel was dropped (sender gone) — treat as denial
                self.cleanup(function_call_id).await;
                PermissionOutcome::Denied
            }
            Err(_) => {
                // Timeout expired
                self.cleanup(function_call_id).await;
                PermissionOutcome::Timeout
            }
        }
    }

    /// Resolve a pending permission request.
    ///
    /// Called when the ACP client responds to a RequestPermission message.
    ///
    /// # Errors
    ///
    /// Returns `SessionNotFound` if the `function_call_id` is not in the pending map.
    pub async fn resolve_permission(
        &self,
        function_call_id: &str,
        approved: bool,
    ) -> Result<(), AcpServerError> {
        let mut pending = self.pending.lock().await;
        let entry = pending.remove(function_call_id).ok_or_else(|| {
            AcpServerError::SessionNotFound(format!(
                "no pending permission for function_call_id: {function_call_id}"
            ))
        })?;

        // Send the decision; ignore error if receiver was dropped
        let _ = entry.response_tx.send(approved);
        Ok(())
    }

    /// Remove a pending request from the map (cleanup after timeout/drop).
    async fn cleanup(&self, function_call_id: &str) {
        let mut pending = self.pending.lock().await;
        pending.remove(function_call_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_approval_flow() {
        let bridge = PermissionBridge::new(Duration::from_secs(5));
        let bridge_clone = Arc::new(bridge);
        let bridge_for_resolve = bridge_clone.clone();

        let handle = tokio::spawn(async move {
            bridge_clone.request_permission("delete_file", "fc-1", &serde_json::json!({})).await
        });

        // Small delay to ensure request is registered
        tokio::time::sleep(Duration::from_millis(10)).await;

        bridge_for_resolve.resolve_permission("fc-1", true).await.unwrap();

        let outcome = handle.await.unwrap();
        assert_eq!(outcome, PermissionOutcome::Approved);
    }

    #[tokio::test]
    async fn test_denial_flow() {
        let bridge = PermissionBridge::new(Duration::from_secs(5));
        let bridge_clone = Arc::new(bridge);
        let bridge_for_resolve = bridge_clone.clone();

        let handle = tokio::spawn(async move {
            bridge_clone.request_permission("delete_file", "fc-2", &serde_json::json!({})).await
        });

        tokio::time::sleep(Duration::from_millis(10)).await;

        bridge_for_resolve.resolve_permission("fc-2", false).await.unwrap();

        let outcome = handle.await.unwrap();
        assert_eq!(outcome, PermissionOutcome::Denied);
    }

    #[tokio::test]
    async fn test_timeout_flow() {
        let bridge = PermissionBridge::new(Duration::from_millis(50));

        let outcome =
            bridge.request_permission("delete_file", "fc-3", &serde_json::json!({})).await;

        assert_eq!(outcome, PermissionOutcome::Timeout);
    }

    #[tokio::test]
    async fn test_resolve_unknown_id() {
        let bridge = PermissionBridge::new(Duration::from_secs(5));

        let result = bridge.resolve_permission("unknown-id", true).await;
        assert!(result.is_err());
    }
}
