//! The product's audit records (spec 3.5 §9.3).
//!
//! | Action | Written by |
//! |---|---|
//! | `website.onboarding.started`, `website.onboarding.stage`, `website.onboarding.completed` | [`crate::onboarding::OnboardingProgress`] |
//! | `website.charter.drafted` | [`crate::onboarding::draft_charter`] |
//! | `website.alarm.raised`, `website.alarm.acknowledged` | [`crate::alarms::AlarmRouter`] |
//! | `website.review.sent` | [`crate::review::ReviewComposer::send`] |
//! | `site.status.changed`, `site.rollback` | [`crate::incident::IncidentPath`] |
//!
//! Each is an `AuditRecordKind::Other("website")` record whose fields carry the site, the
//! charter version and the release sequence where there is one; the kernel and the harness
//! write the charter, autonomy, signal and release records.

use adk_core::audit_log::{AuditActor, AuditRecordDraft, AuditRecordKind, AuditTrail};
use serde_json::Value;

/// Onboarding began (spec 3.5 §9.3).
pub const ONBOARDING_STARTED: &str = "website.onboarding.started";
/// An onboarding stage completed.
pub const ONBOARDING_STAGE: &str = "website.onboarding.stage";
/// Onboarding completed: the site is live.
pub const ONBOARDING_COMPLETED: &str = "website.onboarding.completed";
/// A charter version was drafted.
pub const CHARTER_DRAFTED: &str = "website.charter.drafted";
/// An alarm was raised to the owner.
pub const ALARM_RAISED: &str = "website.alarm.raised";
/// A person acknowledged an alarm.
pub const ALARM_ACKNOWLEDGED: &str = "website.alarm.acknowledged";
/// A weekly review was sent.
pub const REVIEW_SENT: &str = "website.review.sent";
/// The site's status changed.
pub const STATUS_CHANGED: &str = "site.status.changed";
/// The site rolled back to a signed release.
pub const ROLLBACK: &str = "site.rollback";

/// Writes one record when `trail` is set; a failed write is logged and never undoes the act
/// it records.
pub(crate) async fn record(
    trail: Option<&AuditTrail>,
    action: &str,
    actor: AuditActor,
    fields: Value,
) {
    let Some(trail) = trail else { return };
    let mut draft = AuditRecordDraft::new(
        AuditRecordKind::Other(adk_kernel::charter::WEBSITE_AUDIT_KIND.to_string()),
        action,
    )
    .actor(actor);
    if let Value::Object(fields) = fields {
        for (key, value) in fields {
            draft = draft.ext(key, value);
        }
    }
    if let Err(error) = trail.record(draft).await {
        tracing::warn!(audit.action = action, error = %error, "could not write a website audit record");
    }
}
