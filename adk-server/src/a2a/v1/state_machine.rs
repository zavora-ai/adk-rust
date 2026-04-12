//! A2A v1.0.0 task state machine per spec §5.5.
//!
//! Validates task state transitions according to the A2A Protocol v1.0.0
//! specification. Terminal states cannot transition to any other state,
//! and each non-terminal state has a defined set of allowed target states.

use a2a_protocol_types::TaskState;

use super::error::A2aError;

/// Returns a human-readable name for the given [`TaskState`].
pub fn state_name(state: &TaskState) -> &'static str {
    match state {
        TaskState::Submitted => "SUBMITTED",
        TaskState::Working => "WORKING",
        TaskState::InputRequired => "INPUT_REQUIRED",
        TaskState::AuthRequired => "AUTH_REQUIRED",
        TaskState::Completed => "COMPLETED",
        TaskState::Failed => "FAILED",
        TaskState::Canceled => "CANCELED",
        TaskState::Rejected => "REJECTED",
        TaskState::Unspecified => "UNSPECIFIED",
        _ => "UNKNOWN",
    }
}

/// Validates whether a task state transition from `from` to `to` is allowed
/// per the A2A v1.0.0 specification.
///
/// # Allowed transitions
///
/// | From | To |
/// |---|---|
/// | SUBMITTED | WORKING, REJECTED, FAILED, CANCELED |
/// | WORKING | COMPLETED, FAILED, CANCELED, INPUT_REQUIRED, AUTH_REQUIRED |
/// | INPUT_REQUIRED | WORKING, FAILED, CANCELED |
/// | AUTH_REQUIRED | WORKING, FAILED, CANCELED |
/// | Terminal states | _(none)_ |
/// | UNSPECIFIED | _(none)_ |
///
/// # Errors
///
/// Returns [`A2aError::InvalidStateTransition`] with both state names when
/// the transition is not in the allowed set.
pub fn can_transition_to(from: TaskState, to: TaskState) -> Result<(), A2aError> {
    let allowed: &[TaskState] = match from {
        TaskState::Submitted => {
            &[TaskState::Working, TaskState::Rejected, TaskState::Failed, TaskState::Canceled]
        }
        TaskState::Working => &[
            TaskState::Completed,
            TaskState::Failed,
            TaskState::Canceled,
            TaskState::InputRequired,
            TaskState::AuthRequired,
        ],
        TaskState::InputRequired => &[TaskState::Working, TaskState::Failed, TaskState::Canceled],
        TaskState::AuthRequired => &[TaskState::Working, TaskState::Failed, TaskState::Canceled],
        // Terminal states: no transitions allowed
        TaskState::Completed | TaskState::Failed | TaskState::Canceled | TaskState::Rejected => &[],
        // Unspecified: no transitions allowed
        TaskState::Unspecified => &[],
        // Any future unknown variant: no transitions allowed
        _ => &[],
    };

    if allowed.contains(&to) {
        Ok(())
    } else {
        Err(A2aError::InvalidStateTransition {
            from: state_name(&from).to_string(),
            to: state_name(&to).to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Valid transitions ────────────────────────────────────────────────

    #[test]
    fn test_submitted_to_working() {
        assert!(can_transition_to(TaskState::Submitted, TaskState::Working).is_ok());
    }

    #[test]
    fn test_submitted_to_rejected() {
        assert!(can_transition_to(TaskState::Submitted, TaskState::Rejected).is_ok());
    }

    #[test]
    fn test_submitted_to_failed() {
        assert!(can_transition_to(TaskState::Submitted, TaskState::Failed).is_ok());
    }

    #[test]
    fn test_submitted_to_canceled() {
        assert!(can_transition_to(TaskState::Submitted, TaskState::Canceled).is_ok());
    }

    #[test]
    fn test_working_to_completed() {
        assert!(can_transition_to(TaskState::Working, TaskState::Completed).is_ok());
    }

    #[test]
    fn test_working_to_failed() {
        assert!(can_transition_to(TaskState::Working, TaskState::Failed).is_ok());
    }

    #[test]
    fn test_working_to_canceled() {
        assert!(can_transition_to(TaskState::Working, TaskState::Canceled).is_ok());
    }

    #[test]
    fn test_working_to_input_required() {
        assert!(can_transition_to(TaskState::Working, TaskState::InputRequired).is_ok());
    }

    #[test]
    fn test_working_to_auth_required() {
        assert!(can_transition_to(TaskState::Working, TaskState::AuthRequired).is_ok());
    }

    #[test]
    fn test_input_required_to_working() {
        assert!(can_transition_to(TaskState::InputRequired, TaskState::Working).is_ok());
    }

    #[test]
    fn test_input_required_to_failed() {
        assert!(can_transition_to(TaskState::InputRequired, TaskState::Failed).is_ok());
    }

    #[test]
    fn test_input_required_to_canceled() {
        assert!(can_transition_to(TaskState::InputRequired, TaskState::Canceled).is_ok());
    }

    #[test]
    fn test_auth_required_to_working() {
        assert!(can_transition_to(TaskState::AuthRequired, TaskState::Working).is_ok());
    }

    #[test]
    fn test_auth_required_to_failed() {
        assert!(can_transition_to(TaskState::AuthRequired, TaskState::Failed).is_ok());
    }

    #[test]
    fn test_auth_required_to_canceled() {
        assert!(can_transition_to(TaskState::AuthRequired, TaskState::Canceled).is_ok());
    }

    // ── Terminal states reject all transitions ───────────────────────────

    #[test]
    fn test_completed_rejects_all() {
        let targets = all_states();
        for target in targets {
            let result = can_transition_to(TaskState::Completed, target);
            assert!(result.is_err(), "COMPLETED should not transition to {target:?}");
        }
    }

    #[test]
    fn test_failed_rejects_all() {
        let targets = all_states();
        for target in targets {
            let result = can_transition_to(TaskState::Failed, target);
            assert!(result.is_err(), "FAILED should not transition to {target:?}");
        }
    }

    #[test]
    fn test_canceled_rejects_all() {
        let targets = all_states();
        for target in targets {
            let result = can_transition_to(TaskState::Canceled, target);
            assert!(result.is_err(), "CANCELED should not transition to {target:?}");
        }
    }

    #[test]
    fn test_rejected_rejects_all() {
        let targets = all_states();
        for target in targets {
            let result = can_transition_to(TaskState::Rejected, target);
            assert!(result.is_err(), "REJECTED should not transition to {target:?}");
        }
    }

    #[test]
    fn test_unspecified_rejects_all() {
        let targets = all_states();
        for target in targets {
            let result = can_transition_to(TaskState::Unspecified, target);
            assert!(result.is_err(), "UNSPECIFIED should not transition to {target:?}");
        }
    }

    // ── Invalid transitions return error with both state names ───────────

    #[test]
    fn test_invalid_transition_error_contains_both_states() {
        let err = can_transition_to(TaskState::Completed, TaskState::Working).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("COMPLETED"), "error should contain source state name, got: {msg}");
        assert!(msg.contains("WORKING"), "error should contain target state name, got: {msg}");
    }

    #[test]
    fn test_submitted_to_completed_is_invalid() {
        let err = can_transition_to(TaskState::Submitted, TaskState::Completed).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("SUBMITTED"));
        assert!(msg.contains("COMPLETED"));
    }

    #[test]
    fn test_working_to_submitted_is_invalid() {
        let err = can_transition_to(TaskState::Working, TaskState::Submitted).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("WORKING"));
        assert!(msg.contains("SUBMITTED"));
    }

    // ── state_name helper ────────────────────────────────────────────────

    #[test]
    fn test_state_name_all_variants() {
        assert_eq!(state_name(&TaskState::Submitted), "SUBMITTED");
        assert_eq!(state_name(&TaskState::Working), "WORKING");
        assert_eq!(state_name(&TaskState::InputRequired), "INPUT_REQUIRED");
        assert_eq!(state_name(&TaskState::AuthRequired), "AUTH_REQUIRED");
        assert_eq!(state_name(&TaskState::Completed), "COMPLETED");
        assert_eq!(state_name(&TaskState::Failed), "FAILED");
        assert_eq!(state_name(&TaskState::Canceled), "CANCELED");
        assert_eq!(state_name(&TaskState::Rejected), "REJECTED");
        assert_eq!(state_name(&TaskState::Unspecified), "UNSPECIFIED");
    }

    // ── Helpers ──────────────────────────────────────────────────────────

    fn all_states() -> Vec<TaskState> {
        vec![
            TaskState::Submitted,
            TaskState::Working,
            TaskState::InputRequired,
            TaskState::AuthRequired,
            TaskState::Completed,
            TaskState::Failed,
            TaskState::Canceled,
            TaskState::Rejected,
            TaskState::Unspecified,
        ]
    }
}
