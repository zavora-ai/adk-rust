//! The web editor (spec 3.5 §7.8.2 "Entry points", §13 Q13, N4, A-I23, A-I24): a structured
//! form per content kind, Markdown-backed rich text, media through the media pipeline, a
//! preview with page diffs, scheduling, corrections and withdrawals — and no page builder.
//!
//! Every change enters the publishing pipeline as `EntryPoint::WebEditor` with the person as
//! its proposer, so only people holding an editing role author content here (A-I24). A
//! release under an approval chain waits for each role, each a distinct person who did not
//! propose it (A-I23); the release is complete only when the last one approves.

use std::collections::BTreeMap;

use adk_core::Result;
use adk_core::principal::SubjectRef;
use adk_kernel::charter::RoleApproval;
use adk_site::content::kind_directory;
use adk_site::gate::{CheckStatus, GateReport};
use adk_site::media::MediaUpload;
use adk_site::pipeline::{
    ApprovalDecision, Change, Draft, EntryPoint, PipelineOutcome, Preview, Withdrawal,
};
use awp_types::{
    ContentId, ContentItem, ContentKind, ContentState, LanguageTag, Markdown, Role, Schedule,
};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::charter::SignerLine;
use super::onboarding::{CheckView, PageView};
use super::words;
use super::{
    FieldKind, FieldOption, FieldSpec, FormValues, Pill, Tone, WebsiteDesk, forbidden, invalid,
    kernel, not_found, optional, site,
};

/// The roles that author content in the web editor (A-I24).
const EDITING: [Role; 3] = [Role::Owner, Role::Editor, Role::Publisher];

/// The kinds the editor offers, in order.
const KINDS: [ContentKind; 9] = [
    ContentKind::Page,
    ContentKind::Post,
    ContentKind::Article,
    ContentKind::Product,
    ContentKind::Event,
    ContentKind::Notice,
    ContentKind::Service,
    ContentKind::Person,
    ContentKind::Collection,
];

/// A release waiting for its approval chain (spec 3.5 §7.8.2 step 4, A-I23).
#[derive(Clone)]
pub(crate) struct PendingRelease {
    pub(crate) draft: Draft,
    pub(crate) preview: Preview,
    pub(crate) approvals: Vec<RoleApproval>,
}

/// How a release step ended.
pub(crate) enum ReleaseStep {
    Live { seq: u64 },
    Scheduled { at: String },
    Pending(Box<PendingRelease>),
    Held(String),
    Rejected { by: String, reason: String },
}

/// Who has approved a release waiting for its chain, and who still must (A-I23).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalView {
    /// One line.
    pub text: String,
    /// The chain's roles, in order, with who approved in each.
    pub lines: Vec<SignerLine>,
    /// The roles the viewer may approve in now.
    pub can_approve: Vec<String>,
    /// Who proposed it; they cannot approve it.
    pub proposer: String,
}

/// A content kind's form (spec 3.5 §7.8.2, Q13): structured fields, then the Markdown body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct KindForm {
    /// The kind, as sent back.
    pub kind: String,
    /// Its name.
    pub title: String,
    /// What it is for.
    pub description: String,
    /// Its fields, before the body.
    pub fields: Vec<FieldSpec>,
}

/// One item of the site's content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ItemLine {
    /// The item.
    pub id: String,
    /// Its kind's name.
    pub kind: String,
    /// Its title.
    pub title: String,
    /// Its address part.
    pub slug: String,
    /// Its language.
    pub language: String,
    /// Where it stands.
    pub state: Pill,
    /// When it publishes, for a scheduled item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled: Option<DateTime<Utc>>,
    /// Whether it can be corrected or withdrawn: it is live.
    pub live: bool,
}

/// A draft in progress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DraftLine {
    /// The draft.
    pub id: String,
    /// Its title.
    pub title: String,
    /// Its kind's name.
    pub kind: String,
    /// Where it stands.
    pub status: Pill,
    /// Who wrote it.
    pub author: String,
}

/// The editor's home: the forms, the content and the drafts (spec 3.5 §7.8.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EditorHome {
    /// One form per content kind.
    pub kinds: Vec<KindForm>,
    /// The site's content, newest kinds first.
    pub items: Vec<ItemLine>,
    /// Drafts in progress.
    pub drafts: Vec<DraftLine>,
    /// The site's languages.
    pub languages: Vec<FieldOption>,
    /// Whether the viewer may write.
    pub can_edit: bool,
    /// Who approves a publication, when the charter names a chain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain: Option<String>,
}

/// A media file beside a draft (spec 3.5 §7.8.2 "Media").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MediaView {
    /// Its path.
    pub path: String,
    /// Its file name.
    pub name: String,
    /// Its media type.
    pub content_type: String,
    /// Its alt text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alt: Option<String>,
    /// Whether a person confirmed the alt text; the accessibility check needs it.
    pub alt_confirmed: bool,
    /// Whether it is a generated illustration.
    pub generated: bool,
}

/// A schedule as the form shows it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleInput {
    /// When to publish.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publish_at: Option<DateTime<Utc>>,
    /// Not before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embargo_until: Option<DateTime<Utc>>,
    /// When to withdraw it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    /// When to go back to the previous version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revert_at: Option<DateTime<Utc>>,
}

impl ScheduleInput {
    fn into_schedule(self) -> Option<Schedule> {
        let schedule = Schedule {
            publish_at: self.publish_at,
            embargo_until: self.embargo_until,
            expires_at: self.expires_at,
            revert_at: self.revert_at,
        };
        (schedule != Schedule::default()).then_some(schedule)
    }

    fn of(schedule: Option<&Schedule>) -> Self {
        schedule.map_or_else(Self::default, |s| Self {
            publish_at: s.publish_at,
            embargo_until: s.embargo_until,
            expires_at: s.expires_at,
            revert_at: s.revert_at,
        })
    }
}

/// One draft in the editor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DraftView {
    /// The draft.
    pub id: String,
    /// The kind, as sent back.
    pub kind: String,
    /// The kind's name.
    pub kind_title: String,
    /// The address part.
    pub slug: String,
    /// The language.
    pub language: String,
    /// The kind's fields with their values.
    pub fields: Vec<FieldSpec>,
    /// The body, in Markdown.
    pub body: String,
    /// The schedule.
    pub schedule: ScheduleInput,
    /// Its media.
    pub media: Vec<MediaView>,
    /// The per-change checks, once saved.
    pub checks: Vec<CheckView>,
    /// One line on the checks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check_text: Option<String>,
    /// The preview's pages and how they change.
    pub preview: Vec<PageView>,
    /// The approval chain it waits for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<ApprovalView>,
    /// How publishing ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// Where it stands.
    pub status: Pill,
    /// The live item it changes, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changes: Option<String>,
    /// Who wrote it.
    pub author: String,
}

/// An editor action (spec 3.5 §7.8.2). Uploads have their own call, with the file's bytes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum EditorCommand {
    /// A new draft of `kind`.
    New {
        /// The kind, such as `product`.
        kind: String,
        /// Its language; the site's first unless set.
        #[serde(default)]
        language: Option<String>,
    },
    /// A draft that changes the item `id`.
    Open {
        /// The item.
        id: String,
    },
    /// Saves the draft's form and runs the per-change checks.
    Save {
        /// The draft.
        draft: String,
        /// The address part.
        slug: String,
        /// The language.
        language: String,
        /// The kind's fields, by id.
        fields: FormValues,
        /// The body, in Markdown.
        #[serde(default)]
        body: Option<String>,
        /// The schedule.
        #[serde(default)]
        schedule: ScheduleInput,
    },
    /// Confirms an image's alt text; the accessibility check needs it.
    ConfirmAlt {
        /// The draft.
        draft: String,
        /// The media file.
        path: String,
        /// The text.
        alt: String,
    },
    /// Builds the preview with its page diffs (spec 3.3 §7.5).
    Preview {
        /// The draft.
        draft: String,
    },
    /// Publishes, or schedules when the draft has a future time.
    Publish {
        /// The draft.
        draft: String,
    },
    /// Approves a draft waiting for its chain, in `role` (A-I23).
    Approve {
        /// The draft.
        draft: String,
        /// The role.
        role: String,
    },
    /// Refuses a draft waiting for its chain.
    Reject {
        /// The draft.
        draft: String,
        /// Why.
        reason: String,
    },
    /// Corrects a live item: a new release with a visible correction note.
    Correct {
        /// The item.
        id: String,
        /// The correction note readers see.
        note: String,
        /// Fields to change.
        #[serde(default)]
        fields: FormValues,
        /// A new body.
        #[serde(default)]
        body: Option<String>,
    },
    /// Withdraws a live item: a notice in its place, or 410 Gone without one.
    Withdraw {
        /// The item.
        id: String,
        /// The notice; 410 Gone when absent.
        #[serde(default)]
        notice: Option<String>,
    },
    /// Throws a draft away.
    Discard {
        /// The draft.
        draft: String,
    },
}

/// What an editor action did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EditorOutcome {
    /// The editor after it.
    pub home: EditorHome,
    /// The draft it acted on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft: Option<DraftView>,
    /// One line on what happened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

struct EditorDraft {
    change: Change,
    changes: Option<ContentId>,
    checks: Option<GateReport>,
    preview: Option<(Preview, Vec<PageView>)>,
    pending: Option<PendingRelease>,
    outcome: Option<String>,
    done: bool,
    scheduled: bool,
}

impl EditorDraft {
    fn item(&self) -> &ContentItem {
        &self.change.items[0]
    }
}

#[derive(Default)]
pub(crate) struct State {
    drafts: BTreeMap<String, EditorDraft>,
}

impl WebsiteDesk {
    pub(crate) async fn save_drafts(&self) -> Vec<super::saved::SavedDraft> {
        let state = self.inner.editor.lock().await;
        state
            .drafts
            .iter()
            .map(|(id, draft)| super::saved::SavedDraft {
                id: id.clone(),
                items: draft.change.items.clone(),
                media: super::saved::SavedDraft::encode(&draft.change.media),
                entry: draft.change.entry.clone(),
                proposer: draft.change.proposer.clone(),
                job: draft.change.job.clone(),
                action_class: draft.change.action_class.clone(),
                changes: draft.changes.clone(),
                outcome: draft.outcome.clone(),
                done: draft.done,
                scheduled: draft.scheduled,
            })
            .collect()
    }

    pub(crate) async fn restore_drafts(&self, saved: Vec<super::saved::SavedDraft>) -> Result<()> {
        let mut drafts = BTreeMap::new();
        for draft in saved {
            let media = draft.decode()?;
            drafts.insert(
                draft.id.clone(),
                EditorDraft {
                    change: Change {
                        items: draft.items,
                        media,
                        entry: draft.entry,
                        proposer: draft.proposer,
                        job: draft.job,
                        action_class: draft.action_class,
                    },
                    changes: draft.changes,
                    checks: None,
                    preview: None,
                    pending: None,
                    outcome: draft.outcome,
                    done: draft.done,
                    scheduled: draft.scheduled,
                },
            );
        }
        let mut state = self.inner.editor.lock().await;
        // Ids the desk hands out next must not collide with the restored ones.
        for id in drafts.keys() {
            if let Some(n) = id.rsplit('-').next().and_then(|n| n.parse::<u64>().ok()) {
                self.bump_next(n + 1);
            }
        }
        state.drafts = drafts;
        Ok(())
    }
}

fn kind_id(kind: ContentKind) -> String {
    serde_json::to_value(kind).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
}

fn kind_from(id: &str) -> Result<ContentKind> {
    KINDS
        .into_iter()
        .find(|kind| kind_id(*kind) == id)
        .ok_or_else(|| invalid("website.editor.kind", format!("no content kind '{id}'")))
}

fn description(kind: ContentKind) -> &'static str {
    match kind {
        ContentKind::Page => "A standing page, such as About or Opening hours",
        ContentKind::Post => "A blog post",
        ContentKind::Article => "A news article, with its byline",
        ContentKind::Product => "Something you sell, with its price",
        ContentKind::Event => "Something that happens at a time and place",
        ContentKind::Notice => "A notice, with the date it ends",
        ContentKind::Service => "A service you offer",
        ContentKind::Person => "A person on your team",
        ContentKind::Collection => "A list of other items",
        _ => "Content",
    }
}

/// The structured fields of `kind` (spec 3.5 §7.8.2): those the built-in renderer shows and
/// marks up for search engines and agents.
fn kind_fields(kind: ContentKind, currency: &str) -> Vec<FieldSpec> {
    let mut fields = vec![
        FieldSpec::new("title", "Title", FieldKind::Text).required(),
        FieldSpec::new("summary", "Summary", FieldKind::LongText)
            .hint("One or two sentences for lists, search results and agents"),
    ];
    match kind {
        ContentKind::Post | ContentKind::Article => {
            fields.push(FieldSpec::new("byline", "Byline", FieldKind::Text));
        }
        ContentKind::Product => {
            fields.push(FieldSpec::new("price", "Price", FieldKind::Number).required());
            fields.push(
                FieldSpec::new("currency", "Currency", FieldKind::Text).required().value(currency),
            );
            fields.push(FieldSpec::new("sku", "Stock code (SKU)", FieldKind::Text));
            fields.push(
                FieldSpec::new("validUntil", "Price valid until", FieldKind::DateTime)
                    .hint("Quotes keep their price until this time"),
            );
        }
        ContentKind::Event => {
            fields.push(FieldSpec::new("startsAt", "Starts", FieldKind::DateTime).required());
            fields.push(FieldSpec::new("endsAt", "Ends", FieldKind::DateTime));
            fields.push(FieldSpec::new("location", "Where", FieldKind::Text));
        }
        ContentKind::Person => fields.push(FieldSpec::new("role", "Role", FieldKind::Text)),
        _ => {}
    }
    fields
}

fn field_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 80
        && slug.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !slug.starts_with('-')
        && !slug.ends_with('-')
}

fn outcome_words(step: &ReleaseStep) -> String {
    match step {
        ReleaseStep::Live { seq } => format!("Live in release {seq}, signed."),
        ReleaseStep::Scheduled { at } => format!("Scheduled for {at}."),
        ReleaseStep::Pending(_) => "Waiting for the approval chain.".into(),
        ReleaseStep::Held(summary) => format!("Held by the checks: {summary}"),
        ReleaseStep::Rejected { by, reason } => format!("{by} refused it: {reason}"),
    }
}

pub(crate) fn check_views(report: &GateReport) -> Vec<CheckView> {
    report
        .checks
        .iter()
        .filter(|check| check.status != CheckStatus::Skipped)
        .map(|check| CheckView {
            name: words::check(check.check).into(),
            status: words::check_status(check.status),
            findings: check
                .findings
                .iter()
                .filter(|f| f.blocking)
                .take(5)
                .map(|f| f.message.clone())
                .collect(),
            note: check.note.clone(),
        })
        .collect()
}

impl WebsiteDesk {
    /// Releases `draft` (spec 3.5 §7.8.2 steps 3–8): its preview, its approval, then the
    /// signed release, or the chain it waits for.
    pub(crate) async fn release(
        &self,
        draft: Draft,
        preview: Option<Preview>,
    ) -> Result<ReleaseStep> {
        let runtime = self.runtime().await;
        let preview = match preview {
            Some(preview) => preview,
            None => runtime.pipeline().preview(&draft).await.map_err(site)?,
        };
        if preview.gate.refuses_release() {
            let _ = tokio::fs::remove_dir_all(&preview.staged.root).await;
            return Err(site(adk_site::SiteError::GateFailed(preview.gate.summary())));
        }
        if !preview.gate.passed() {
            let _ = tokio::fs::remove_dir_all(&preview.staged.root).await;
            return Ok(ReleaseStep::Held(preview.gate.summary()));
        }
        match runtime.pipeline().approve(&draft, &preview).await.map_err(site)? {
            ApprovalDecision::Approved { decision, .. } => {
                self.finish(&draft, &preview, decision).await
            }
            ApprovalDecision::Pending { .. } => {
                Ok(ReleaseStep::Pending(Box::new(PendingRelease {
                    draft,
                    preview,
                    approvals: Vec::new(),
                })))
            }
            ApprovalDecision::Rejected { by, reason } => {
                let _ = tokio::fs::remove_dir_all(&preview.staged.root).await;
                Ok(ReleaseStep::Rejected { by, reason })
            }
        }
    }

    async fn finish(
        &self,
        draft: &Draft,
        preview: &Preview,
        decision: Option<String>,
    ) -> Result<ReleaseStep> {
        let runtime = self.runtime().await;
        let outcome = runtime.pipeline().complete(draft, decision).await.map_err(site);
        let _ = tokio::fs::remove_dir_all(&preview.staged.root).await;
        Ok(match outcome? {
            PipelineOutcome::Published(published) => {
                ReleaseStep::Live { seq: published.staged.manifest.seq }
            }
            PipelineOutcome::Scheduled { at } => ReleaseStep::Scheduled {
                at: at.map_or_else(
                    || "its time".to_string(),
                    |at| at.format("%-d %b %Y %H:%M UTC").to_string(),
                ),
            },
            PipelineOutcome::Held(report) => ReleaseStep::Held(report.summary()),
            PipelineOutcome::Rejected { by, reason } => ReleaseStep::Rejected { by, reason },
            PipelineOutcome::Pending { .. } => ReleaseStep::Held("it waits for approval".into()),
        })
    }

    /// The chain the publication `action_class` needs, from the charter in force.
    async fn chain(
        &self,
        action_class: &str,
    ) -> Result<Option<(awp_types::Governance, Vec<Role>)>> {
        let Some(charters) = &self.inner.charters else { return Ok(None) };
        let Some(current) = charters.current(self.inner.site.id).await.map_err(kernel)? else {
            return Ok(None);
        };
        let roles = current
            .charter
            .governance
            .approval_chains
            .iter()
            .find(|chain| chain.action.as_str() == action_class)
            .map(|chain| chain.roles.clone());
        Ok(roles.map(|roles| (current.charter.governance, roles)))
    }

    /// Records `person`'s approval in `role` of a release waiting for its chain (A-I23);
    /// `Some` once the release completes or is refused.
    pub(crate) async fn approve_release(
        &self,
        pending: &mut PendingRelease,
        person: &str,
        role: &str,
    ) -> Result<Option<ReleaseStep>> {
        let role = words::role_from(role)
            .ok_or_else(|| invalid("website.editor.role", format!("no role '{role}'")))?;
        let (governance, _) =
            self.chain(&pending.draft.change.action_class).await?.ok_or_else(|| {
                invalid("website.editor.no_chain", "this release needs no approval chain")
            })?;
        if adk_kernel::charter::is_agent_principal(person) || !governance.holds(person, role) {
            return Err(forbidden(
                "website.editor.not_an_approver",
                format!("{person} does not hold the {} role", words::role(role).to_lowercase()),
            ));
        }
        if pending.draft.change.proposer == person {
            return Err(forbidden(
                "website.editor.own_change",
                "the person who proposed a change cannot approve it (spec 3.5 A-I23)",
            ));
        }
        if pending.approvals.iter().any(|a| a.principal.id == person) {
            return Err(invalid("website.editor.approved", "you have approved this already"));
        }
        let approval = RoleApproval::new(role, SubjectRef::human(person), self.now());
        let runtime = self.runtime().await;
        runtime.approvals().approve(&pending.preview.staged.digest, approval.clone()).await;
        pending.approvals.push(approval);
        match runtime.pipeline().approve(&pending.draft, &pending.preview).await.map_err(site)? {
            ApprovalDecision::Approved { decision, .. } => {
                Ok(Some(self.finish(&pending.draft, &pending.preview, decision).await?))
            }
            ApprovalDecision::Pending { .. } => Ok(None),
            ApprovalDecision::Rejected { by, reason } => {
                Ok(Some(ReleaseStep::Rejected { by, reason }))
            }
        }
    }

    /// Who approved `pending` and who still must, as `person` sees it.
    pub(crate) async fn approval_view(
        &self,
        pending: &PendingRelease,
        person: &str,
    ) -> Result<ApprovalView> {
        let (governance, roles) =
            self.chain(&pending.draft.change.action_class).await?.unwrap_or_else(|| {
                (
                    awp_types::Governance { roles: Vec::new(), approval_chains: Vec::new() },
                    Vec::new(),
                )
            });
        let mut used: Vec<&str> = Vec::new();
        let lines: Vec<SignerLine> = roles
            .iter()
            .map(|role| {
                let approval = pending
                    .approvals
                    .iter()
                    .find(|a| a.role == *role && !used.contains(&a.principal.id.as_str()));
                if let Some(approval) = approval {
                    used.push(approval.principal.id.as_str());
                }
                SignerLine {
                    role: words::role_id(*role).into(),
                    label: words::role(*role).into(),
                    holders: governance.holders(*role).map(str::to_string).collect(),
                    signed_by: approval.map(|a| a.principal.id.clone()),
                    signed_at: approval.map(|a| a.at),
                }
            })
            .collect();
        let proposer = pending.draft.change.proposer.clone();
        let mayapprove =
            person != proposer && !pending.approvals.iter().any(|a| a.principal.id == person);
        let can_approve: Vec<String> = if mayapprove {
            let mut roles: Vec<String> = lines
                .iter()
                .filter(|line| line.signed_by.is_none() && line.holders.iter().any(|h| h == person))
                .map(|line| line.role.clone())
                .collect();
            roles.dedup();
            roles
        } else {
            Vec::new()
        };
        let waiting: Vec<String> = lines
            .iter()
            .filter(|l| l.signed_by.is_none())
            .map(|l| l.label.to_lowercase())
            .collect();
        Ok(ApprovalView {
            text: if waiting.is_empty() {
                "Every role has approved.".into()
            } else {
                format!("Waiting for the {}", waiting.join(" and the "))
            },
            lines,
            can_approve,
            proposer,
        })
    }

    /// The editor's home for `person` (spec 3.5 §7.8.2).
    ///
    /// # Errors
    ///
    /// The stores' errors.
    pub async fn editor_home(&self, person: &str) -> Result<EditorHome> {
        let roles = self.roles(person).await?;
        let can_edit = EDITING.iter().any(|role| roles.contains(role));
        let runtime = self.runtime().await;
        let mut items: Vec<ContentItem> = runtime.store().list().await.map_err(site)?;
        // What people write comes first; standing pages, the legal ones among them, after.
        items.sort_by(|a, b| {
            (a.kind == ContentKind::Page, a.kind, &a.slug, a.language.as_str()).cmp(&(
                b.kind == ContentKind::Page,
                b.kind,
                &b.slug,
                b.language.as_str(),
            ))
        });
        let items = items
            .into_iter()
            .map(|item| ItemLine {
                id: item.id.0.clone(),
                kind: words::content_kind(item.kind).into(),
                title: item
                    .fields
                    .get("title")
                    .map(field_text)
                    .unwrap_or_else(|| item.slug.clone()),
                slug: item.slug.clone(),
                language: item.language.to_string(),
                state: words::content_state(item.state),
                scheduled: item
                    .schedule
                    .as_ref()
                    .and_then(|s| s.publish_at)
                    .filter(|_| item.state == ContentState::Scheduled),
                live: matches!(item.state, ContentState::Published | ContentState::Corrected),
            })
            .collect();
        let state = self.inner.editor.lock().await;
        let drafts = state
            .drafts
            .iter()
            .filter(|(_, draft)| !draft.done)
            .map(|(id, draft)| DraftLine {
                id: id.clone(),
                title: draft
                    .item()
                    .fields
                    .get("title")
                    .map(field_text)
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| "Untitled".into()),
                kind: words::content_kind(draft.item().kind).into(),
                status: draft_status(draft),
                author: draft.change.proposer.clone(),
            })
            .collect();
        drop(state);
        let chain = self.chain("publish").await?.map(|(_, roles)| {
            let names: Vec<String> = roles.iter().map(|r| words::role(*r).to_lowercase()).collect();
            format!(
                "Publishing needs the approval of the {}, each a different person.",
                names.join(", then the ")
            )
        });
        Ok(EditorHome {
            kinds: KINDS
                .into_iter()
                .map(|kind| KindForm {
                    kind: kind_id(kind),
                    title: words::content_kind(kind).into(),
                    description: description(kind).into(),
                    fields: kind_fields(kind, &self.inner.currency),
                })
                .collect(),
            items,
            drafts,
            languages: self
                .inner
                .languages
                .iter()
                .map(|l| FieldOption { value: l.to_string(), label: l.to_string() })
                .collect(),
            can_edit,
            chain,
        })
    }

    /// One draft as `person` sees it.
    ///
    /// # Errors
    ///
    /// `NotFound` for an unknown draft; the stores' errors.
    pub async fn draft_view(&self, person: &str, id: &str) -> Result<DraftView> {
        let state = self.inner.editor.lock().await;
        let draft = state
            .drafts
            .get(id)
            .ok_or_else(|| not_found("website.editor.draft", format!("no draft '{id}'")))?;
        let item = draft.item().clone();
        let fields = kind_fields(item.kind, &self.inner.currency)
            .into_iter()
            .map(|field| {
                let value = item.fields.get(&field.id).map(field_text).unwrap_or_default();
                if value.is_empty() { field } else { field.value(value) }
            })
            .collect();
        let checks = draft.checks.as_ref().map(check_views).unwrap_or_default();
        let check_text = draft.checks.as_ref().map(|report| {
            if report.passed() {
                "Every check passed.".to_string()
            } else {
                format!("Fix before publishing: {}", report.summary())
            }
        });
        let preview = draft.preview.as_ref().map(|(_, pages)| pages.clone()).unwrap_or_default();
        let pending = draft.pending.clone();
        let view = DraftView {
            id: id.to_string(),
            kind: kind_id(item.kind),
            kind_title: words::content_kind(item.kind).into(),
            slug: item.slug.clone(),
            language: item.language.to_string(),
            fields,
            body: item.body.as_ref().map(|b| b.0.clone()).unwrap_or_default(),
            schedule: ScheduleInput::of(item.schedule.as_ref()),
            media: item
                .media
                .iter()
                .map(|m| MediaView {
                    name: m.path.rsplit('/').next().unwrap_or(&m.path).to_string(),
                    path: m.path.clone(),
                    content_type: m.content_type.clone(),
                    alt: m.alt.clone(),
                    alt_confirmed: m.alt_confirmed,
                    generated: m.generated,
                })
                .collect(),
            checks,
            check_text,
            preview,
            approval: None,
            outcome: draft.outcome.clone(),
            status: draft_status(draft),
            changes: draft.changes.as_ref().map(|id| id.0.clone()),
            author: draft.change.proposer.clone(),
        };
        drop(state);
        Ok(match pending {
            Some(pending) => {
                DraftView { approval: Some(self.approval_view(&pending, person).await?), ..view }
            }
            None => view,
        })
    }

    /// Runs one media upload through the media pipeline into the draft `id` (spec 3.5 §7.8.2
    /// "Media"): sniffed, executables and polyglots refused, location stripped, resized; the
    /// alt text is a draft until a person confirms it.
    ///
    /// # Errors
    ///
    /// `Forbidden` without an editing role; the media pipeline's refusal.
    pub async fn upload(
        &self,
        person: &str,
        id: &str,
        name: &str,
        bytes: &[u8],
        alt: Option<String>,
        generated: bool,
    ) -> Result<DraftView> {
        self.require(person, &EDITING, "Adding media").await?;
        {
            let mut state = self.inner.editor.lock().await;
            let draft = state
                .drafts
                .get_mut(id)
                .ok_or_else(|| not_found("website.editor.draft", format!("no draft '{id}'")))?;
            let processed = self
                .inner
                .media
                .process(&MediaUpload {
                    name,
                    bytes,
                    alt: alt.filter(|a| !a.trim().is_empty()),
                    generated,
                })
                .map_err(site)?;
            let item = &mut draft.change.items[0];
            let dir = format!("content/{}/{}", kind_directory(item.kind), item.slug);
            let media = processed.media_ref(&dir);
            item.media.retain(|held| held.path != media.path);
            item.media.push(media.clone());
            draft.change.media.retain(|(path, _)| path != &media.path);
            draft.change.media.push((media.path.clone(), processed.bytes.clone()));
            for variant in &processed.variants {
                draft.change.media.push((format!("{dir}/{}", variant.name), variant.bytes.clone()));
            }
            draft.checks = None;
            draft.preview = None;
        }
        self.persist().await;
        self.draft_view(person, id).await
    }

    /// Carries out an editor `command` for `person` (spec 3.5 §7.8.2, A-I23, A-I24).
    ///
    /// # Errors
    ///
    /// `Forbidden` without an editing role or for an approval the person may not give;
    /// `InvalidInput` for a bad value; the pipeline's errors.
    pub async fn editor_command(
        &self,
        person: &str,
        command: EditorCommand,
    ) -> Result<EditorOutcome> {
        let result = self.editor_command_unsaved(person, command).await;
        if result.is_ok() {
            self.persist().await;
        }
        result
    }

    async fn editor_command_unsaved(
        &self,
        person: &str,
        command: EditorCommand,
    ) -> Result<EditorOutcome> {
        let mut note = None;
        let mut current: Option<String> = None;
        match command {
            EditorCommand::New { kind, language } => {
                self.require(person, &EDITING, "Writing").await?;
                let kind = kind_from(&kind)?;
                let language = match language {
                    Some(tag) => LanguageTag::parse(&tag)
                        .map_err(|e| invalid("website.editor.language", e.to_string()))?,
                    None => self
                        .inner
                        .languages
                        .first()
                        .cloned()
                        .unwrap_or_else(|| LanguageTag::parse("en").expect("a valid tag")),
                };
                let id = self.next_id("draft");
                let mut fields = BTreeMap::new();
                if kind == ContentKind::Product {
                    fields
                        .insert("currency".to_string(), Value::String(self.inner.currency.clone()));
                }
                let item = ContentItem {
                    id: ContentId(String::new()),
                    kind,
                    slug: String::new(),
                    language,
                    fields,
                    volatile: Vec::new(),
                    body: None,
                    media: Vec::new(),
                    sources: Vec::new(),
                    schedule: None,
                    state: ContentState::Draft,
                };
                self.inner
                    .editor
                    .lock()
                    .await
                    .drafts
                    .insert(id.clone(), self.editor_draft(item, person, None));
                current = Some(id);
            }
            EditorCommand::Open { id } => {
                self.require(person, &EDITING, "Changing content").await?;
                let item = self
                    .runtime()
                    .await
                    .store()
                    .get(&ContentId(id.clone()))
                    .await
                    .map_err(site)?
                    .ok_or_else(|| not_found("website.editor.item", format!("no item '{id}'")))?;
                let draft_id = self.next_id("draft");
                let origin = item.id.clone();
                let mut item = item;
                item.state = ContentState::Draft;
                self.inner
                    .editor
                    .lock()
                    .await
                    .drafts
                    .insert(draft_id.clone(), self.editor_draft(item, person, Some(origin)));
                current = Some(draft_id);
            }
            EditorCommand::Save { draft, slug, language, fields, body, schedule } => {
                self.require(person, &EDITING, "Writing").await?;
                note = Some(
                    self.save(person, &draft, &slug, &language, &fields, body, schedule).await?,
                );
                current = Some(draft);
            }
            EditorCommand::ConfirmAlt { draft, path, alt } => {
                self.require(person, &EDITING, "Confirming alt text").await?;
                let alt = alt.trim().to_string();
                let mut state = self.inner.editor.lock().await;
                let held = state.drafts.get_mut(&draft).ok_or_else(|| {
                    not_found("website.editor.draft", format!("no draft '{draft}'"))
                })?;
                let media =
                    held.change.items[0].media.iter_mut().find(|m| m.path == path).ok_or_else(
                        || not_found("website.editor.media", format!("no media '{path}'")),
                    )?;
                media.alt = Some(alt);
                media.alt_confirmed = true;
                held.checks = None;
                held.preview = None;
                current = Some(draft);
            }
            EditorCommand::Preview { draft } => {
                self.require(person, &EDITING, "Previewing").await?;
                self.preview(&draft).await?;
                current = Some(draft);
            }
            EditorCommand::Publish { draft } => {
                self.require(person, &EDITING, "Publishing").await?;
                note = Some(self.publish(person, &draft).await?);
                current = Some(draft);
            }
            EditorCommand::Approve { draft, role } => {
                let mut state = self.inner.editor.lock().await;
                let held = state.drafts.get_mut(&draft).ok_or_else(|| {
                    not_found("website.editor.draft", format!("no draft '{draft}'"))
                })?;
                let mut pending = held.pending.take().ok_or_else(|| {
                    invalid("website.editor.not_waiting", "this draft waits for no approval")
                })?;
                match self.approve_release(&mut pending, person, &role).await {
                    Ok(Some(step)) => {
                        note = Some(outcome_words(&step));
                        held.outcome = note.clone();
                        held.done = !matches!(step, ReleaseStep::Held(_));
                    }
                    Ok(None) => {
                        held.pending = Some(pending);
                        note =
                            Some("Your approval is recorded; it waits for the other roles.".into());
                    }
                    Err(error) => {
                        held.pending = Some(pending);
                        return Err(error);
                    }
                }
                current = Some(draft);
            }
            EditorCommand::Reject { draft, reason } => {
                let reason = reason.trim().to_string();
                if reason.is_empty() {
                    return Err(invalid("website.editor.reason", "say why"));
                }
                let mut state = self.inner.editor.lock().await;
                let held = state.drafts.get_mut(&draft).ok_or_else(|| {
                    not_found("website.editor.draft", format!("no draft '{draft}'"))
                })?;
                let pending = held.pending.as_ref().ok_or_else(|| {
                    invalid("website.editor.not_waiting", "this draft waits for no approval")
                })?;
                let (governance, roles) =
                    self.chain(&pending.draft.change.action_class).await?.ok_or_else(|| {
                        invalid("website.editor.no_chain", "this release needs no approval chain")
                    })?;
                if !roles.iter().any(|role| governance.holds(person, *role)) {
                    return Err(forbidden(
                        "website.editor.not_an_approver",
                        "only an approver in the chain can refuse it",
                    ));
                }
                let runtime = self.runtime().await;
                runtime
                    .approvals()
                    .reject(&pending.preview.staged.digest, &SubjectRef::human(person), &reason)
                    .await;
                let _ = tokio::fs::remove_dir_all(&pending.preview.staged.root).await;
                held.pending = None;
                held.outcome = Some(format!("{person} refused it: {reason}"));
                note = held.outcome.clone();
                current = Some(draft);
            }
            EditorCommand::Correct { id, note: text, fields, body } => {
                self.require(person, &EDITING, "Correcting").await?;
                let text = text.trim().to_string();
                if text.is_empty() {
                    return Err(invalid(
                        "website.editor.note",
                        "a correction needs a note readers see",
                    ));
                }
                let runtime = self.runtime().await;
                let currency = self.inner.currency.clone();
                let item =
                    runtime.store().get(&ContentId(id.clone())).await.map_err(site)?.ok_or_else(
                        || not_found("website.editor.item", format!("no item '{id}'")),
                    )?;
                let mut changed = BTreeMap::new();
                for spec in kind_fields(item.kind, &currency) {
                    if let Some(value) = optional(&fields, &spec.id) {
                        changed.insert(spec.id.clone(), typed(&spec, value)?);
                    }
                }
                let body = body.filter(|b| !b.trim().is_empty());
                let outcome = runtime
                    .pipeline()
                    .correct(
                        &ContentId(id),
                        move |item| {
                            item.fields.extend(changed);
                            if let Some(body) = body {
                                item.body = Some(Markdown(body));
                            }
                        },
                        &text,
                        person,
                        EntryPoint::WebEditor,
                    )
                    .await
                    .map_err(site)?;
                let (line, draft) = self.adopt(outcome, person).await;
                note = Some(line);
                current = draft;
            }
            EditorCommand::Withdraw { id, notice } => {
                self.require(person, &EDITING, "Withdrawing").await?;
                let how = match notice.map(|n| n.trim().to_string()).filter(|n| !n.is_empty()) {
                    Some(text) => Withdrawal::Notice(text),
                    None => Withdrawal::Gone,
                };
                let outcome = self
                    .runtime()
                    .await
                    .pipeline()
                    .withdraw(&ContentId(id), how, person, EntryPoint::WebEditor)
                    .await
                    .map_err(site)?;
                let (line, draft) = self.adopt(outcome, person).await;
                note = Some(line);
                current = draft;
            }
            EditorCommand::Discard { draft } => {
                self.require(person, &EDITING, "Discarding a draft").await?;
                let removed = self.inner.editor.lock().await.drafts.remove(&draft);
                if let Some(removed) = removed {
                    if let Some((preview, _)) = removed.preview {
                        let _ = tokio::fs::remove_dir_all(&preview.staged.root).await;
                    }
                    if let Some(pending) = removed.pending {
                        let _ = tokio::fs::remove_dir_all(&pending.preview.staged.root).await;
                    }
                }
                note = Some("The draft is gone; nothing was published.".into());
            }
        }
        let draft = match current {
            Some(id) => Some(self.draft_view(person, &id).await?),
            None => None,
        };
        Ok(EditorOutcome { home: self.editor_home(person).await?, draft, note })
    }

    fn editor_draft(
        &self,
        item: ContentItem,
        person: &str,
        changes: Option<ContentId>,
    ) -> EditorDraft {
        EditorDraft {
            change: Change {
                items: vec![item],
                media: Vec::new(),
                entry: EntryPoint::WebEditor,
                proposer: person.to_string(),
                job: "web-editor".into(),
                action_class: "publish".into(),
            },
            changes,
            checks: None,
            preview: None,
            pending: None,
            outcome: None,
            done: false,
            scheduled: false,
        }
    }

    /// Keeps a correction or withdrawal that waits for its chain as a draft; returns the line
    /// for the person and the draft's id.
    async fn adopt(&self, outcome: PipelineOutcome, person: &str) -> (String, Option<String>) {
        match outcome {
            PipelineOutcome::Pending { draft, preview, .. } => {
                let id = self.next_id("draft");
                let changes = draft.change.items.first().map(|item| item.id.clone());
                let mut held = self.editor_draft(draft.change.items[0].clone(), person, changes);
                held.change = draft.change.clone();
                held.pending = Some(PendingRelease { draft, preview, approvals: Vec::new() });
                self.inner.editor.lock().await.drafts.insert(id.clone(), held);
                ("It waits for the approval chain.".into(), Some(id))
            }
            PipelineOutcome::Published(published) => {
                (format!("Live in release {}, signed.", published.staged.manifest.seq), None)
            }
            PipelineOutcome::Held(report) => {
                (format!("Held by the checks: {}", report.summary()), None)
            }
            PipelineOutcome::Rejected { by, reason } => {
                (format!("{by} refused it: {reason}"), None)
            }
            PipelineOutcome::Scheduled { .. } => ("Scheduled.".into(), None),
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn save(
        &self,
        person: &str,
        id: &str,
        slug: &str,
        language: &str,
        fields: &FormValues,
        body: Option<String>,
        schedule: ScheduleInput,
    ) -> Result<String> {
        let slug = slug.trim();
        if !valid_slug(slug) {
            return Err(invalid(
                "website.editor.slug",
                "the address part uses lowercase letters, digits and hyphens, such as `spring-menu`",
            ));
        }
        let language = LanguageTag::parse(language.trim())
            .map_err(|e| invalid("website.editor.language", e.to_string()))?;
        if !self.inner.languages.iter().any(|l| l.as_str().eq_ignore_ascii_case(language.as_str()))
        {
            return Err(invalid(
                "website.editor.language",
                format!("the site is not in '{language}'"),
            ));
        }
        let mut state = self.inner.editor.lock().await;
        let draft = state
            .drafts
            .get_mut(id)
            .ok_or_else(|| not_found("website.editor.draft", format!("no draft '{id}'")))?;
        if draft.pending.is_some() || draft.done {
            return Err(invalid(
                "website.editor.locked",
                "this draft is waiting for approval or published",
            ));
        }
        if draft.change.proposer != person {
            return Err(forbidden(
                "website.editor.not_yours",
                "someone else is writing this draft",
            ));
        }
        let item = &mut draft.change.items[0];
        if draft.changes.as_ref().is_some_and(|_| item.slug != slug || item.language != language) {
            return Err(invalid(
                "website.editor.address",
                "a live item keeps its address; withdraw it and write a new one to move it",
            ));
        }
        for spec in kind_fields(item.kind, &self.inner.currency) {
            match optional(fields, &spec.id) {
                Some(value) => {
                    item.fields.insert(spec.id.clone(), typed(&spec, value)?);
                }
                None if spec.required => {
                    return Err(invalid("website.ui.missing", format!("{} is needed", spec.label)));
                }
                None => {
                    item.fields.remove(&spec.id);
                }
            }
        }
        item.slug = slug.to_string();
        item.id = ContentId(format!("{}/{slug}/{language}", kind_directory(item.kind)));
        item.language = language;
        item.body = body.filter(|b| !b.trim().is_empty()).map(Markdown);
        item.schedule = schedule.into_schedule();
        if let Some(schedule) = &item.schedule
            && let (Some(publish), Some(expires)) = (schedule.publish_at, schedule.expires_at)
            && expires <= publish
        {
            return Err(invalid("website.editor.schedule", "it has to end after it starts"));
        }
        item.state = ContentState::Draft;
        let change = draft.change.clone();
        let runtime = self.runtime().await;
        let checked = runtime.pipeline().draft(change).await.map_err(site)?;
        let report = runtime.pipeline().check(&checked).await.map_err(site)?;
        let line = if report.passed() {
            "Saved. Every check passed.".to_string()
        } else {
            format!("Saved. Fix before publishing: {}", report.summary())
        };
        draft.checks = Some(report);
        if let Some((preview, _)) = draft.preview.take() {
            let _ = tokio::fs::remove_dir_all(&preview.staged.root).await;
        }
        draft.outcome = None;
        Ok(line)
    }

    async fn preview(&self, id: &str) -> Result<()> {
        let mut state = self.inner.editor.lock().await;
        let draft = state
            .drafts
            .get_mut(id)
            .ok_or_else(|| not_found("website.editor.draft", format!("no draft '{id}'")))?;
        if draft.item().slug.is_empty() {
            return Err(invalid("website.editor.unsaved", "save the draft first"));
        }
        let runtime = self.runtime().await;
        let checked = runtime.pipeline().draft(draft.change.clone()).await.map_err(site)?;
        let preview = runtime.pipeline().preview(&checked).await.map_err(site)?;
        let mut pages = Vec::new();
        let slug = draft.item().slug.clone();
        let mut diffs = preview.diff.clone();
        diffs.sort_by_key(|d| (!d.path.contains(&slug), d.path.clone()));
        for diff in diffs.iter().take(20) {
            let twin =
                diff.path.trim_start_matches('/').trim_end_matches(".html").to_string() + ".md";
            let markdown = tokio::fs::read_to_string(preview.staged.root.join(&twin))
                .await
                .ok()
                .map(|text| text.chars().take(6_000).collect());
            pages.push(PageView {
                path: diff.path.clone(),
                title: if diff.path.contains(&slug) {
                    draft.item().fields.get("title").map(field_text).unwrap_or_else(|| slug.clone())
                } else {
                    diff.path.clone()
                },
                change: words::page_change(diff.change),
                markdown,
            });
        }
        draft.checks = Some(preview.gate.clone());
        if let Some((old, _)) = draft.preview.replace((preview, pages)) {
            let _ = tokio::fs::remove_dir_all(&old.staged.root).await;
        }
        Ok(())
    }

    async fn publish(&self, person: &str, id: &str) -> Result<String> {
        let mut state = self.inner.editor.lock().await;
        let draft = state
            .drafts
            .get_mut(id)
            .ok_or_else(|| not_found("website.editor.draft", format!("no draft '{id}'")))?;
        if draft.item().slug.is_empty() {
            return Err(invalid("website.editor.unsaved", "save the draft first"));
        }
        if draft.pending.is_some() || draft.done {
            return Err(invalid(
                "website.editor.locked",
                "this draft is waiting for approval or published",
            ));
        }
        if draft.change.proposer != person {
            return Err(forbidden(
                "website.editor.not_yours",
                "someone else is writing this draft",
            ));
        }
        let runtime = self.runtime().await;
        let checked = runtime.pipeline().draft(draft.change.clone()).await.map_err(site)?;
        let preview = draft.preview.take().map(|(preview, _)| preview);
        let step = self.release(checked, preview).await?;
        let line = outcome_words(&step);
        match step {
            ReleaseStep::Pending(pending) => draft.pending = Some(*pending),
            ReleaseStep::Held(_) => {}
            ReleaseStep::Scheduled { .. } => {
                draft.done = true;
                draft.scheduled = true;
            }
            _ => draft.done = true,
        }
        draft.outcome = Some(line.clone());
        Ok(line)
    }
}

fn draft_status(draft: &EditorDraft) -> Pill {
    if draft.done && draft.scheduled {
        Pill::new("Scheduled", Tone::Info)
    } else if draft.done {
        Pill::new("Published", Tone::Good)
    } else if draft.pending.is_some() {
        Pill::new("Waiting for approval", Tone::Waiting)
    } else if draft.checks.as_ref().is_some_and(|r| !r.passed()) {
        Pill::new("Needs a fix", Tone::Bad)
    } else {
        Pill::new("Draft", Tone::Neutral)
    }
}

/// A form value as the field's type (spec 3.5 §7.8.2): numbers as numbers, times as RFC 3339.
fn typed(spec: &FieldSpec, value: &str) -> Result<Value> {
    Ok(match spec.kind {
        FieldKind::Number => {
            let number: f64 = value.replace(',', "").parse().map_err(|_| {
                invalid("website.editor.number", format!("{} is a number", spec.label))
            })?;
            if !number.is_finite() || number < 0.0 {
                return Err(invalid(
                    "website.editor.number",
                    format!("{} is zero or more", spec.label),
                ));
            }
            serde_json::Number::from_f64(number).map_or(Value::Null, Value::Number)
        }
        FieldKind::DateTime => {
            let at = DateTime::parse_from_rfc3339(value).map_err(|_| {
                invalid("website.editor.time", format!("{} is a date and time", spec.label))
            })?;
            Value::String(at.with_timezone(&Utc).to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        }
        _ => Value::String(value.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_has_a_form_with_its_claims() {
        let product = kind_fields(ContentKind::Product, "KES");
        let ids: Vec<&str> = product.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(ids, ["title", "summary", "price", "currency", "sku", "validUntil"]);
        assert!(product.iter().all(|f| f.id != "stock"), "stock is live, never a released claim");
        for kind in KINDS {
            assert_eq!(kind_from(&kind_id(kind)).unwrap(), kind);
        }
    }

    #[test]
    fn values_take_their_fields_types() {
        let price = FieldSpec::new("price", "Price", FieldKind::Number);
        assert_eq!(typed(&price, "1,250.50").unwrap(), serde_json::json!(1250.5));
        assert!(typed(&price, "-1").is_err());
        let starts = FieldSpec::new("startsAt", "Starts", FieldKind::DateTime);
        assert_eq!(typed(&starts, "2026-11-02T09:00:00+03:00").unwrap(), "2026-11-02T06:00:00Z");
        assert!(typed(&starts, "tomorrow").is_err());
    }

    #[test]
    fn slugs_are_address_parts() {
        assert!(valid_slug("spring-menu-2"));
        assert!(!valid_slug("Spring menu"));
        assert!(!valid_slug("../etc"));
        assert!(!valid_slug("-x"));
    }
}
