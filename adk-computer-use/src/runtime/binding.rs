//! Binds every security-relevant MCP response back to what was actually asked for.
//!
//! Typed deserialization proves shape, not provenance. `ControlLease`, `TargetReservation`,
//! and `ExecutionReceipt` have no invariant-enforcing constructor, so a well-formed object
//! belonging to another session, principal, or action deserializes cleanly and was accepted
//! into graph state. The external runtime stays authoritative; this is the local check that
//! makes a stale, confused, or mismatched response fail here rather than propagate.
//!
//! Each validator compares one response against the envelope that requested it and reports
//! the first mismatch by field name, so a rejection says what did not line up.

use crate::contracts::{ActionEnvelope, ControlLease, ExecutionReceipt, TargetReservation};
use crate::error::ComputerUseError;

/// Verifies that an action envelope is inside its runtime-declared validity window.
///
/// The runtime owns the validity duration. ADK only enforces that both timestamps are
/// readable, that the interval is ordered, and that execution happens before `expires_at`.
/// This check belongs immediately before mutation because an approval interrupt or lease
/// acquisition can consume the rest of an otherwise-valid preview window.
///
/// # Errors
///
/// Returns [`ComputerUseError::IdentityMismatch`] when either timestamp is unreadable,
/// `expires_at` is not later than `proposed_at`, or the envelope has expired.
pub fn validate_envelope_freshness(envelope: &ActionEnvelope) -> Result<(), ComputerUseError> {
    let proposed_at =
        chrono::DateTime::parse_from_rfc3339(&envelope.proposed_at).map_err(|error| {
            ComputerUseError::IdentityMismatch(format!(
                "action envelope {} has an unreadable proposed_at {:?}: {error}",
                envelope.action_id, envelope.proposed_at
            ))
        })?;
    let expires_at =
        chrono::DateTime::parse_from_rfc3339(&envelope.expires_at).map_err(|error| {
            ComputerUseError::IdentityMismatch(format!(
                "action envelope {} has an unreadable expires_at {:?}: {error}",
                envelope.action_id, envelope.expires_at
            ))
        })?;

    if expires_at <= proposed_at {
        return Err(ComputerUseError::IdentityMismatch(format!(
            "action envelope {} has a non-positive validity window: proposed_at is {} and \
             expires_at is {}",
            envelope.action_id, envelope.proposed_at, envelope.expires_at
        )));
    }
    if expires_at <= chrono::Utc::now() {
        return Err(ComputerUseError::IdentityMismatch(format!(
            "action envelope {} expired at {}",
            envelope.action_id, envelope.expires_at
        )));
    }

    Ok(())
}

/// Reports a mismatch between what was requested and what came back.
fn mismatch(object: &str, field: &str, expected: &str, actual: &str) -> ComputerUseError {
    ComputerUseError::IdentityMismatch(format!(
        "{object} returned by the computer-use runtime is not bound to this request: {field} \
         is {actual:?}, expected {expected:?}. The response was rejected rather than stored."
    ))
}

/// Compares two optional values, treating a returned `None` as acceptable.
///
/// The wire contract makes `agent_id` and similar fields optional, so absence is under-
/// specification rather than contradiction. A *present and different* value is a mismatch.
fn check_optional(
    object: &str,
    field: &str,
    expected: Option<&str>,
    actual: Option<&str>,
) -> Result<(), ComputerUseError> {
    match (expected, actual) {
        (Some(expected), Some(actual)) if expected != actual => {
            Err(mismatch(object, field, expected, actual))
        }
        _ => Ok(()),
    }
}

/// Verifies a lease belongs to this session, principal, agent, and mode, and is usable.
///
/// # Errors
///
/// Returns [`ComputerUseError::IdentityMismatch`] when any bound field disagrees with
/// `envelope`, when the lease is not active, or when its action budget is exhausted.
///
/// # Example
///
/// ```rust
/// use adk_computer_use::runtime::binding::validate_lease;
/// use adk_computer_use::{ActionEnvelope, ControlLease};
///
/// # fn envelope() -> ActionEnvelope { unimplemented!() }
/// # fn lease_for_another_session() -> ControlLease { unimplemented!() }
/// # fn check() -> Result<(), Box<dyn std::error::Error>> {
/// let envelope = envelope();
/// let lease = lease_for_another_session();
///
/// // A well-formed lease bound to a different session is refused here rather than
/// // entering graph state.
/// assert!(validate_lease(&lease, &envelope).is_err());
/// # Ok(())
/// # }
/// ```
pub fn validate_lease(
    lease: &ControlLease,
    envelope: &ActionEnvelope,
) -> Result<(), ComputerUseError> {
    const OBJECT: &str = "control lease";

    if lease.session_id != envelope.session_id {
        return Err(mismatch(OBJECT, "session_id", &envelope.session_id, &lease.session_id));
    }
    if lease.principal_id != envelope.principal_id {
        return Err(mismatch(OBJECT, "principal_id", &envelope.principal_id, &lease.principal_id));
    }
    check_optional(OBJECT, "agent_id", envelope.agent_id.as_deref(), lease.agent_id.as_deref())?;

    if lease.execution_mode != envelope.requested_mode {
        return Err(mismatch(
            OBJECT,
            "execution_mode",
            &format!("{:?}", envelope.requested_mode),
            &format!("{:?}", lease.execution_mode),
        ));
    }

    // A lease that is not active grants nothing, and a zero budget cannot cover the action
    // it was acquired for. Both are usable-looking objects that must not proceed.
    if !lease.state.eq_ignore_ascii_case("active") {
        return Err(ComputerUseError::IdentityMismatch(format!(
            "control lease {} is in state {:?}, not active, so it authorizes nothing",
            lease.lease_id, lease.state
        )));
    }
    // Remaining budget, not total. Checking `action_budget == 0` accepted a lease whose budget
    // was fully consumed — `action_budget: 1, actions_used: 1` passed while authorizing nothing.
    if lease.actions_used >= lease.action_budget {
        return Err(ComputerUseError::IdentityMismatch(format!(
            "control lease {} has no remaining action budget: {} of {} used",
            lease.lease_id, lease.actions_used, lease.action_budget
        )));
    }

    // An expired lease is a well-formed object that authorizes nothing. Rejecting an
    // unparseable timestamp is deliberate: a lease whose expiry cannot be read is a lease
    // whose validity cannot be established.
    match chrono::DateTime::parse_from_rfc3339(&lease.expires_at) {
        Ok(expires_at) => {
            if expires_at <= chrono::Utc::now() {
                return Err(ComputerUseError::IdentityMismatch(format!(
                    "control lease {} expired at {}",
                    lease.lease_id, lease.expires_at
                )));
            }
        }
        Err(e) => {
            return Err(ComputerUseError::IdentityMismatch(format!(
                "control lease {} has an unreadable expiry {:?}: {e}",
                lease.lease_id, lease.expires_at
            )));
        }
    }

    // Target boundaries. A lease scoped to one application must not authorize an action against
    // another, which is the whole point of scoping it.
    if let Some(target) = &envelope.target {
        if !lease.boundaries.app_ids.is_empty()
            && !lease.boundaries.app_ids.contains(&target.app_id)
        {
            return Err(mismatch(
                OBJECT,
                "boundaries.app_ids",
                &target.app_id,
                &format!("{:?}", lease.boundaries.app_ids),
            ));
        }

        if let Some(window_id) = &target.window_id
            && !lease.boundaries.window_ids.is_empty()
            && !lease.boundaries.window_ids.contains(window_id)
        {
            return Err(mismatch(
                OBJECT,
                "boundaries.window_ids",
                &format!("{window_id}"),
                &format!("{:?}", lease.boundaries.window_ids),
            ));
        }
    }

    Ok(())
}

/// Verifies a reservation belongs to this action and is active, current, and target-bound.
///
/// # Errors
///
/// Returns [`ComputerUseError::IdentityMismatch`] when any bound field disagrees with
/// `envelope`, the reservation is not active, its expiry cannot be established, or it has
/// expired.
pub fn validate_reservation(
    reservation: &TargetReservation,
    envelope: &ActionEnvelope,
) -> Result<(), ComputerUseError> {
    const OBJECT: &str = "target reservation";

    if reservation.session_id != envelope.session_id {
        return Err(mismatch(OBJECT, "session_id", &envelope.session_id, &reservation.session_id));
    }
    if reservation.principal_id != envelope.principal_id {
        return Err(mismatch(
            OBJECT,
            "principal_id",
            &envelope.principal_id,
            &reservation.principal_id,
        ));
    }
    check_optional(
        OBJECT,
        "agent_id",
        envelope.agent_id.as_deref(),
        reservation.agent_id.as_deref(),
    )?;
    check_optional(
        OBJECT,
        "execution_group_id",
        envelope.execution_group_id.as_deref(),
        reservation.execution_group_id.as_deref(),
    )?;

    if reservation.intent_id != envelope.action_id {
        return Err(mismatch(OBJECT, "intent_id", &envelope.action_id, &reservation.intent_id));
    }
    if !reservation.state.eq_ignore_ascii_case("active") {
        return Err(ComputerUseError::IdentityMismatch(format!(
            "target reservation {} is in state {:?}, not active",
            reservation.reservation_id, reservation.state
        )));
    }
    match chrono::DateTime::parse_from_rfc3339(&reservation.expires_at) {
        Ok(expires_at) => {
            if expires_at <= chrono::Utc::now() {
                return Err(ComputerUseError::IdentityMismatch(format!(
                    "target reservation {} expired at {}",
                    reservation.reservation_id, reservation.expires_at
                )));
            }
        }
        Err(error) => {
            return Err(ComputerUseError::IdentityMismatch(format!(
                "target reservation {} has an unreadable expiry {:?}: {error}",
                reservation.reservation_id, reservation.expires_at
            )));
        }
    }

    let target = envelope.target.as_ref().ok_or_else(|| {
        ComputerUseError::IdentityMismatch(format!(
            "target reservation {} was returned for action {} without target evidence",
            reservation.reservation_id, envelope.action_id
        ))
    })?;
    if reservation.scope.app_id != target.app_id {
        return Err(mismatch(OBJECT, "scope.app_id", &target.app_id, &reservation.scope.app_id));
    }
    if reservation.scope.window_id != target.window_id {
        return Err(mismatch(
            OBJECT,
            "scope.window_id",
            &format!("{:?}", target.window_id),
            &format!("{:?}", reservation.scope.window_id),
        ));
    }

    Ok(())
}

/// Verifies a receipt describes the action that was actually submitted.
///
/// The digest is the strongest binding available: `ActionEnvelope::args_digest` is what
/// approval was granted against, so a receipt carrying a different digest describes different
/// work regardless of matching identifiers. An empty expected digest is treated as
/// unavailable rather than as a match against an empty value.
///
/// # Errors
///
/// Returns [`ComputerUseError::IdentityMismatch`] when the session, action ID, or action
/// digest disagrees with `envelope`.
pub fn validate_receipt(
    receipt: &ExecutionReceipt,
    envelope: &ActionEnvelope,
    expected_digest: &str,
) -> Result<(), ComputerUseError> {
    const OBJECT: &str = "execution receipt";

    if receipt.session_id != envelope.session_id {
        return Err(mismatch(OBJECT, "session_id", &envelope.session_id, &receipt.session_id));
    }
    if receipt.action_id != envelope.action_id {
        return Err(mismatch(OBJECT, "action_id", &envelope.action_id, &receipt.action_id));
    }
    if !expected_digest.is_empty() && receipt.action_digest != expected_digest {
        return Err(mismatch(OBJECT, "action_digest", expected_digest, &receipt.action_digest));
    }

    Ok(())
}
