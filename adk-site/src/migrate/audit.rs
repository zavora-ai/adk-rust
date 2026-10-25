//! Audit records of the migration stages (spec 3.5 §9.3): `site.migration.{finding, decided}`,
//! `site.freeze`, `site.archive`, `site.drift.detected`.

use std::sync::Arc;

use adk_core::ActingContext;
use adk_core::audit_log::{
    AuditActor, AuditDecision, AuditRecordDraft, AuditRecordKind, AuditRecorder,
};

use super::error::{MigrateError, Result};

/// Appends one `site.*` record whose extension `site` holds `value`, a small summary with
/// digests in place of content.
pub(crate) async fn record(
    recorder: &Arc<dyn AuditRecorder>,
    acting: &ActingContext,
    action: &str,
    decision: Option<AuditDecision>,
    value: serde_json::Value,
) -> Result<()> {
    let mut draft = AuditRecordDraft::new(AuditRecordKind::Other("site".into()), action)
        .actor(AuditActor::new(acting, ""))
        .args_digest(recorder.digest(&value))
        .ext("site", value);
    if let Some(decision) = decision {
        draft = draft.decision(decision);
    }
    recorder.record(draft).await.map_err(|e| MigrateError::Service(format!("audit: {e}")))
}
