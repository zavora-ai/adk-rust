//! Health state machine with valid transition enforcement.

use std::sync::Arc;

use awp_types::AwpError;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::events::{AwpEvent, EventSubscriptionService};

/// Health states for an AWP service.
///
/// Valid transitions:
/// - `Healthy → Degrading`
/// - `Degrading → Degraded`
/// - `Degrading → Healthy`
/// - `Degraded → Healthy`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HealthState {
    Healthy,
    Degrading,
    Degraded,
}

impl std::fmt::Display for HealthState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Healthy => write!(f, "healthy"),
            Self::Degrading => write!(f, "degrading"),
            Self::Degraded => write!(f, "degraded"),
        }
    }
}

/// A snapshot of the current health state with a message and timestamp.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthStateSnapshot {
    /// Current health state.
    pub state: HealthState,
    /// Human-readable message about the current state.
    pub message: String,
    /// When this state was entered.
    pub timestamp: DateTime<Utc>,
}

/// State machine that enforces valid health transitions and emits events.
pub struct HealthStateMachine {
    snapshot: RwLock<HealthStateSnapshot>,
    event_service: Arc<dyn EventSubscriptionService>,
}

impl HealthStateMachine {
    /// Create a new health state machine starting in [`HealthState::Healthy`].
    pub fn new(event_service: Arc<dyn EventSubscriptionService>) -> Self {
        Self {
            snapshot: RwLock::new(HealthStateSnapshot {
                state: HealthState::Healthy,
                message: "service started".to_string(),
                timestamp: Utc::now(),
            }),
            event_service,
        }
    }

    /// Get the current health state snapshot.
    pub async fn snapshot(&self) -> HealthStateSnapshot {
        self.snapshot.read().await.clone()
    }

    /// Transition to [`HealthState::Degrading`].
    ///
    /// Only valid from [`HealthState::Healthy`].
    pub async fn report_degrading(&self, reason: &str) -> Result<(), AwpError> {
        self.transition(HealthState::Degrading, reason).await
    }

    /// Transition to [`HealthState::Degraded`].
    ///
    /// Only valid from [`HealthState::Degrading`].
    pub async fn report_degraded(&self, reason: &str) -> Result<(), AwpError> {
        self.transition(HealthState::Degraded, reason).await
    }

    /// Transition to [`HealthState::Healthy`].
    ///
    /// Valid from [`HealthState::Degrading`] or [`HealthState::Degraded`].
    pub async fn report_healthy(&self) -> Result<(), AwpError> {
        self.transition(HealthState::Healthy, "recovered").await
    }

    async fn transition(&self, target: HealthState, reason: &str) -> Result<(), AwpError> {
        let mut snapshot = self.snapshot.write().await;
        let current = snapshot.state;

        if !is_valid_transition(current, target) {
            return Err(AwpError::InvalidRequest(format!(
                "invalid health transition: {current} → {target}"
            )));
        }

        let old_state = current;
        snapshot.state = target;
        snapshot.message = reason.to_string();
        snapshot.timestamp = Utc::now();

        // Emit health.changed event
        let event = AwpEvent {
            id: Uuid::now_v7(),
            event_type: "health.changed".to_string(),
            timestamp: snapshot.timestamp,
            payload: serde_json::json!({
                "previousState": old_state.to_string(),
                "newState": target.to_string(),
                "reason": reason,
            }),
        };

        // Release the lock before delivering events
        drop(snapshot);

        if let Err(e) = self.event_service.deliver(event).await {
            tracing::warn!("failed to deliver health.changed event: {e}");
        }

        Ok(())
    }
}

/// Check whether a state transition is valid.
fn is_valid_transition(from: HealthState, to: HealthState) -> bool {
    matches!(
        (from, to),
        (HealthState::Healthy, HealthState::Degrading)
            | (HealthState::Degrading, HealthState::Degraded)
            | (HealthState::Degrading, HealthState::Healthy)
            | (HealthState::Degraded, HealthState::Healthy)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::InMemoryEventSubscriptionService;

    fn make_sm() -> HealthStateMachine {
        let events = Arc::new(InMemoryEventSubscriptionService::new());
        HealthStateMachine::new(events)
    }

    #[tokio::test]
    async fn test_initial_state_is_healthy() {
        let sm = make_sm();
        assert_eq!(sm.snapshot().await.state, HealthState::Healthy);
    }

    #[tokio::test]
    async fn test_healthy_to_degrading() {
        let sm = make_sm();
        sm.report_degrading("high latency").await.unwrap();
        assert_eq!(sm.snapshot().await.state, HealthState::Degrading);
    }

    #[tokio::test]
    async fn test_degrading_to_degraded() {
        let sm = make_sm();
        sm.report_degrading("high latency").await.unwrap();
        sm.report_degraded("service down").await.unwrap();
        assert_eq!(sm.snapshot().await.state, HealthState::Degraded);
    }

    #[tokio::test]
    async fn test_degrading_to_healthy() {
        let sm = make_sm();
        sm.report_degrading("high latency").await.unwrap();
        sm.report_healthy().await.unwrap();
        assert_eq!(sm.snapshot().await.state, HealthState::Healthy);
    }

    #[tokio::test]
    async fn test_degraded_to_healthy() {
        let sm = make_sm();
        sm.report_degrading("high latency").await.unwrap();
        sm.report_degraded("service down").await.unwrap();
        sm.report_healthy().await.unwrap();
        assert_eq!(sm.snapshot().await.state, HealthState::Healthy);
    }

    #[tokio::test]
    async fn test_invalid_healthy_to_degraded() {
        let sm = make_sm();
        let result = sm.report_degraded("skip degrading").await;
        assert!(result.is_err());
        assert_eq!(sm.snapshot().await.state, HealthState::Healthy);
    }

    #[tokio::test]
    async fn test_invalid_degraded_to_degrading() {
        let sm = make_sm();
        sm.report_degrading("x").await.unwrap();
        sm.report_degraded("y").await.unwrap();
        let result = sm.report_degrading("back to degrading").await;
        assert!(result.is_err());
        assert_eq!(sm.snapshot().await.state, HealthState::Degraded);
    }

    #[tokio::test]
    async fn test_invalid_self_transition_healthy() {
        let sm = make_sm();
        let result = sm.report_healthy().await;
        assert!(result.is_err());
        assert_eq!(sm.snapshot().await.state, HealthState::Healthy);
    }

    #[tokio::test]
    async fn test_snapshot_message_updated() {
        let sm = make_sm();
        sm.report_degrading("database slow").await.unwrap();
        let snap = sm.snapshot().await;
        assert_eq!(snap.message, "database slow");
    }

    #[test]
    fn test_valid_transitions() {
        assert!(is_valid_transition(HealthState::Healthy, HealthState::Degrading));
        assert!(is_valid_transition(HealthState::Degrading, HealthState::Degraded));
        assert!(is_valid_transition(HealthState::Degrading, HealthState::Healthy));
        assert!(is_valid_transition(HealthState::Degraded, HealthState::Healthy));
    }

    #[test]
    fn test_invalid_transitions() {
        assert!(!is_valid_transition(HealthState::Healthy, HealthState::Degraded));
        assert!(!is_valid_transition(HealthState::Healthy, HealthState::Healthy));
        assert!(!is_valid_transition(HealthState::Degraded, HealthState::Degrading));
        assert!(!is_valid_transition(HealthState::Degraded, HealthState::Degraded));
        assert!(!is_valid_transition(HealthState::Degrading, HealthState::Degrading));
    }

    #[test]
    fn test_health_state_display() {
        assert_eq!(HealthState::Healthy.to_string(), "healthy");
        assert_eq!(HealthState::Degrading.to_string(), "degrading");
        assert_eq!(HealthState::Degraded.to_string(), "degraded");
    }

    #[test]
    fn test_health_state_snapshot_serialization() {
        let snap = HealthStateSnapshot {
            state: HealthState::Healthy,
            message: "ok".to_string(),
            timestamp: Utc::now(),
        };
        let json = serde_json::to_string(&snap).unwrap();
        let parsed: HealthStateSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.state, HealthState::Healthy);
        assert_eq!(parsed.message, "ok");
    }
}
