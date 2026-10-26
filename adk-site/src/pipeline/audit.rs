//! The pipeline's audit records and its span (spec 3.5 §7.24, §9.3).
//!
//! | Record | Written when |
//! |---|---|
//! | [`CONTENT_DRAFTED`] | [`PublishingPipeline::draft`](super::PublishingPipeline::draft) holds a change |
//! | [`CONTENT_APPROVED`] | The approval gate approves a draft |
//! | [`CONTENT_SCHEDULED`] | An approved change waits for its publication time |
//! | [`CONTENT_PUBLISHED`], [`CONTENT_CORRECTED`], [`CONTENT_WITHDRAWN`] | A release carrying items in those states is published |
//! | [`RELEASE_REFUSED`] | The launch gate refuses a change whole (A-I1) |
//! | [`RELEASE_BUILT`] | A release's manifest is built over the build's files |
//! | [`RELEASE_SIGNED`] | Its digest is in the site log under a signed checkpoint |
//! | [`RELEASE_PUBLISHED`] | The host serves it |
//!
//! Each is an `AuditRecordKind::Other("site")` record whose actor is the proposer — an agent
//! for an `urn:adk:agent:` principal, else a person — and whose fields carry the site's
//! domain, the principal, the charter version and the release sequence where there is one.
//! The update engine writes the batch records, `site.content.reverted` and its own
//! `site.release.refused`.
//!
//! Releases run inside a [`RELEASE_SPAN`] span carrying `site.release.seq`,
//! `site.release.source` and `site.release.items`. A release made outside the pipeline, such
//! as `adk-rust site publish` of the working copy, writes the same release records with
//! [`record_release_logged`] and [`record_release_published`].

use adk_core::audit_log::{AuditActor, AuditRecordDraft, AuditRecordKind, AuditTrail};
use adk_core::principal::{AgentId, SubjectRef};
use awp_types::{ContentItem, ContentState, ReleaseSource};
use serde_json::{Value, json};

/// The span every release runs in (spec 3.5 §7.24).
pub const RELEASE_SPAN: &str = "site.release";

/// The kind of the pipeline's records.
pub const AUDIT_KIND: &str = "site";

/// A change was drafted (spec 3.5 §9.3).
pub const CONTENT_DRAFTED: &str = "site.content.drafted";
/// A draft was approved.
pub const CONTENT_APPROVED: &str = "site.content.approved";
/// An approved change waits for its publication time.
pub const CONTENT_SCHEDULED: &str = "site.content.scheduled";
/// Items went live in a release.
pub const CONTENT_PUBLISHED: &str = "site.content.published";
/// Corrected items went live in a release.
pub const CONTENT_CORRECTED: &str = "site.content.corrected";
/// Items were withdrawn in a release.
pub const CONTENT_WITHDRAWN: &str = "site.content.withdrawn";
/// A release was built.
pub const RELEASE_BUILT: &str = "site.release.built";
/// A release's digest was logged under a signed checkpoint.
pub const RELEASE_SIGNED: &str = "site.release.signed";
/// A release was published.
pub const RELEASE_PUBLISHED: &str = "site.release.published";
/// A change was refused whole.
pub const RELEASE_REFUSED: &str = "site.release.refused";

/// The most item ids one record lists; `total` counts them all.
const IDS_MAX: usize = 1_000;

/// The actor of a record proposed by `principal`: an agent for an agent principal, else a
/// person.
pub(crate) fn actor(principal: &str) -> AuditActor {
    if principal.starts_with("urn:adk:agent:")
        && let Ok(agent) = AgentId::new(principal)
    {
        return AuditActor { agent: Some(agent), ..AuditActor::default() };
    }
    if principal.is_empty() {
        return AuditActor::default();
    }
    AuditActor::subject(SubjectRef::human(principal))
}

/// The release source's job, for the span.
pub(crate) fn source_name(source: &ReleaseSource) -> String {
    match source {
        ReleaseSource::Edit { job, .. } => job.clone(),
        other => serde_json::to_value(other)
            .ok()
            .and_then(|value| value.get("kind").and_then(Value::as_str).map(str::to_string))
            .unwrap_or_else(|| "release".to_string()),
    }
}

/// Item ids, at most [`IDS_MAX`], and how many there are.
pub(crate) fn ids<'a>(ids: impl IntoIterator<Item = &'a awp_types::ContentId>) -> (Value, usize) {
    let all: Vec<&str> = ids.into_iter().map(|id| id.0.as_str()).collect();
    let total = all.len();
    (json!(all.into_iter().take(IDS_MAX).collect::<Vec<_>>()), total)
}

/// The content records of a published release: one for each of `published`, `corrected`
/// and `withdrawn` whose state an item carries.
pub(crate) fn content_actions(items: &[ContentItem]) -> Vec<(&'static str, Vec<&ContentItem>)> {
    [
        (CONTENT_PUBLISHED, ContentState::Published),
        (CONTENT_CORRECTED, ContentState::Corrected),
        (CONTENT_WITHDRAWN, ContentState::Withdrawn),
    ]
    .into_iter()
    .filter_map(|(action, state)| {
        let matching: Vec<&ContentItem> = items.iter().filter(|item| item.state == state).collect();
        (!matching.is_empty()).then_some((action, matching))
    })
    .collect()
}

/// Who released what, for the release records.
#[derive(Debug, Clone, Copy)]
pub struct ReleaseContext<'a> {
    /// The site's domain.
    pub site: &'a str,
    /// The charter version the release was built under.
    pub charter_version: u32,
    /// The principal that proposed it, or `schedule`; empty for a rollback.
    pub principal: &'a str,
}

/// Writes `site.release.built` for `staged`, and `site.release.signed` once its digest is in
/// the site log under a checkpoint `verifier_key` verifies (spec 3.5 §9.3).
pub async fn record_release_logged(
    trail: Option<&AuditTrail>,
    context: ReleaseContext<'_>,
    staged: &crate::release::StagedRelease,
    verifier_key: &str,
) {
    let seq = staged.manifest.seq;
    let fields = json!({
        "releaseSeq": seq,
        "manifest": staged.digest,
        "files": staged.files.len(),
        "changed": staged.changed.len(),
        "removed": staged.removed.len(),
        "snapshot": staged.manifest.is_snapshot(),
        "source": staged.manifest.source,
    });
    record(trail, RELEASE_BUILT, context.principal, context.site, context.charter_version, fields)
        .await;
    let fields = json!({
        "releaseSeq": seq,
        "manifest": staged.digest,
        "logSize": staged.log.as_ref().map_or(seq, |log| log.size),
        "checkpoint": staged.log.as_ref().and_then(|log| log.checkpoint.as_ref()).map(|(path, _)| path),
        "verifierKey": verifier_key,
    });
    record(trail, RELEASE_SIGNED, context.principal, context.site, context.charter_version, fields)
        .await;
}

/// Writes `site.release.published` for `staged` once the host serves it, with how many files
/// were uploaded and how many items it changed (spec 3.5 §9.3).
pub async fn record_release_published(
    trail: Option<&AuditTrail>,
    context: ReleaseContext<'_>,
    staged: &crate::release::StagedRelease,
    uploaded: usize,
    items: usize,
) {
    let fields = json!({
        "releaseSeq": staged.manifest.seq,
        "manifest": staged.digest,
        "uploaded": uploaded,
        "items": items,
    });
    record(
        trail,
        RELEASE_PUBLISHED,
        context.principal,
        context.site,
        context.charter_version,
        fields,
    )
    .await;
}

/// Writes one record when `trail` is set. A record that cannot be written is logged and
/// never undoes the act it records.
pub(crate) async fn record(
    trail: Option<&AuditTrail>,
    action: &str,
    principal: &str,
    site: &str,
    charter_version: u32,
    fields: Value,
) {
    let Some(trail) = trail else { return };
    let mut draft = AuditRecordDraft::new(AuditRecordKind::Other(AUDIT_KIND.to_string()), action)
        .actor(actor(principal))
        .args_digest(trail.digest(&fields))
        .ext("site", json!(site))
        .ext("principal", json!(principal))
        .ext("charterVersion", json!(charter_version));
    if let Value::Object(fields) = fields {
        for (key, value) in fields {
            draft = draft.ext(key, value);
        }
    }
    if let Err(error) = trail.record(draft).await {
        tracing::warn!(audit.action = action, error = %error, "could not write a site audit record");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agents_and_people_are_told_apart() {
        let agent = actor("urn:adk:agent:bakery/steward");
        assert_eq!(
            agent.agent.map(|id| id.as_str().to_string()).as_deref(),
            Some("urn:adk:agent:bakery/steward")
        );
        assert_eq!(actor("amina").on_behalf_of, Some(SubjectRef::human("amina")));
        assert_eq!(actor(""), AuditActor::default());
    }

    #[test]
    fn sources_name_their_job() {
        assert_eq!(source_name(&ReleaseSource::Edit { job: "seo".into(), decision: None }), "seo");
        assert_eq!(source_name(&ReleaseSource::Rollback { to_seq: 3 }), "rollback");
    }
}
