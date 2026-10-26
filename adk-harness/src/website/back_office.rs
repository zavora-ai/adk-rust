//! The back-office toolset (spec 3.5 §7.8.2, §7.10, §7.11, §7.13, §7.18): content drafts and
//! edits, schedules, corrections, withdrawals and reverts, translations, media, previews,
//! publication and rollback, host reads, the launch gate, `awp verify`, charter drafts and
//! the decision cards that commit charters and promote jobs.
//!
//! Every tool calls `adk-site`'s and the kernel's APIs; none decides its own authority. The
//! governed path decides each call by the catalogue ([`super::catalog`]), the job's level
//! ([`super::ladder`]) and the website policy ([`super::policy`]).

use std::collections::BTreeMap;
use std::sync::Arc;

use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result, Tool, ToolContext, ToolEffect};
use adk_site::content::kind_directory;
use adk_site::media::MediaUpload;
use adk_site::pipeline::{
    ApprovalDecision, Change, Draft, EntryPoint, PipelineOutcome, Published, Withdrawal,
};
use async_trait::async_trait;
use awp_types::{
    Charter, ContentId, ContentItem, ContentKind, ContentState, FactRef, LanguageTag, Markdown,
    Role, Schedule,
};
use base64::Engine as _;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};

use super::catalog::{self, BACK_OFFICE_ORIGIN, ToolSpec};
use super::site::{
    CharterSignRequest, DraftRecord, PromotionRequest, SiteRuntime, kernel_error, site_error,
};

/// The session state key holding the [`EntryPoint`] of the turn: the authoring router sets it
/// for a paired owner's message (spec 3.5 §7.8.2 "Entry points", A-I24); without it, a change
/// is the site's own agent's.
pub const ENTRY_STATE_KEY: &str = "website:entry";

/// The default action class of a release, whose approval chain applies (spec 3.5 §7.6).
const PUBLISH_ACTION: &str = "publish";

fn invalid(tool: &str, error: impl std::fmt::Display) -> AdkError {
    AdkError::new(
        ErrorComponent::Tool,
        ErrorCategory::InvalidInput,
        "website.tool.invalid_args",
        format!("invalid arguments for {tool}: {error}"),
    )
}

fn parse<T: for<'de> Deserialize<'de>>(tool: &str, args: Value) -> Result<T> {
    serde_json::from_value(args).map_err(|e| invalid(tool, e))
}

fn kind_of(name: &str) -> Result<ContentKind> {
    serde_json::from_value(Value::String(name.to_ascii_lowercase()))
        .map_err(|_| invalid("content", format!("'{name}' is not a content kind")))
}

fn language_of(tag: &str) -> Result<LanguageTag> {
    tag.parse().map_err(|_| invalid("content", format!("'{tag}' is not a language tag")))
}

/// The proposer of a change: the acting agent's principal.
fn proposer(ctx: &Arc<dyn ToolContext>) -> String {
    ctx.acting_context().agent.id.as_str().to_string()
}

/// The entry point of the turn (A-I24): the router's paired device, else the site's agent.
fn entry_point(ctx: &Arc<dyn ToolContext>) -> EntryPoint {
    ctx.session()
        .and_then(|session| session.state().get(ENTRY_STATE_KEY))
        .and_then(|value| serde_json::from_value::<EntryPoint>(value).ok())
        .unwrap_or_else(|| EntryPoint::SiteAgent { agent: proposer(ctx) })
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ScheduleArgs {
    #[serde(default)]
    publish_at: Option<DateTime<Utc>>,
    #[serde(default)]
    embargo_until: Option<DateTime<Utc>>,
    #[serde(default)]
    expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    revert_at: Option<DateTime<Utc>>,
}

impl ScheduleArgs {
    fn into_schedule(self) -> Option<Schedule> {
        let schedule = Schedule {
            publish_at: self.publish_at,
            embargo_until: self.embargo_until,
            expires_at: self.expires_at,
            revert_at: self.revert_at,
        };
        (schedule != Schedule::default()).then_some(schedule)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DraftArgs {
    kind: String,
    slug: String,
    language: String,
    #[serde(default)]
    fields: BTreeMap<String, Value>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    sources: Vec<String>,
    #[serde(default)]
    volatile: Vec<String>,
    #[serde(default)]
    schedule: Option<ScheduleArgs>,
    #[serde(default)]
    job: Option<String>,
    #[serde(default)]
    draft: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EditArgs {
    id: String,
    #[serde(default)]
    fields: BTreeMap<String, Value>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    sources: Option<Vec<String>>,
    #[serde(default)]
    job: Option<String>,
    #[serde(default)]
    draft: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TranslationArgs {
    id: String,
    language: String,
    #[serde(default)]
    fields: BTreeMap<String, Value>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    job: Option<String>,
    #[serde(default)]
    draft: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MediaArgs {
    draft: String,
    name: String,
    data_base64: String,
    #[serde(default)]
    alt: Option<String>,
    #[serde(default)]
    generated: bool,
    #[serde(default)]
    item: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DraftRef {
    draft: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ScheduleDraftArgs {
    draft: String,
    #[serde(default)]
    publish_at: Option<DateTime<Utc>>,
    #[serde(default)]
    embargo_until: Option<DateTime<Utc>>,
    #[serde(default)]
    expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    revert_at: Option<DateTime<Utc>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CorrectArgs {
    id: String,
    note: String,
    #[serde(default)]
    fields: BTreeMap<String, Value>,
    #[serde(default)]
    body: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WithdrawArgs {
    id: String,
    #[serde(default)]
    notice: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RevertArgs {
    id: String,
    #[serde(default)]
    to_release: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RollbackArgs {
    to_release: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HostFetchArgs {
    path: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VerifyArgs {
    #[serde(default)]
    origin: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CharterDraftArgs {
    charter: Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PromotionArgs {
    job: String,
    reason: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DiscoverArgs {
    url: String,
    #[serde(default)]
    facts: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InterviewArgs {
    text: String,
    quote: String,
    #[serde(default)]
    kind: Option<String>,
}

/// One back-office tool: its catalogue entry, description and schema, over the site runtime.
pub struct BackOfficeTool {
    spec: &'static ToolSpec,
    description: &'static str,
    schema: Value,
    site: Arc<SiteRuntime>,
}

impl std::fmt::Debug for BackOfficeTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackOfficeTool").field("name", &self.spec.name).finish_non_exhaustive()
    }
}

fn content_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "kind": { "type": "string", "enum": ["page", "post", "article", "product", "event", "notice", "service", "person", "collection"] },
            "slug": { "type": "string" },
            "language": { "type": "string", "description": "BCP 47 tag, one of the site's languages" },
            "fields": { "type": "object", "description": "Typed fields: title, summary, price, sku, byline, startsAt…" },
            "body": { "type": "string", "description": "Markdown body" },
            "sources": { "type": "array", "items": { "type": "string" }, "description": "Ids of the Known facts the claims rest on" },
            "volatile": { "type": "array", "items": { "type": "string" } },
            "schedule": { "$ref": "#/$defs/schedule" },
            "job": { "type": "string" },
            "draft": { "type": "string", "description": JOIN_DRAFT }
        },
        "required": ["kind", "slug", "language"],
        "additionalProperties": false,
        "$defs": { "schedule": schedule_schema() }
    })
}

fn schedule_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "publishAt": { "type": "string", "format": "date-time" },
            "embargoUntil": { "type": "string", "format": "date-time" },
            "expiresAt": { "type": "string", "format": "date-time" },
            "revertAt": { "type": "string", "format": "date-time" }
        },
        "additionalProperties": false
    })
}

/// What the `draft` argument of the drafting tools does (spec 3.5 §7.8.2 step 1).
const JOIN_DRAFT: &str = "The id of an earlier draft this item joins, so one release carries \
    several items, such as the pages of release 1; without it the item starts a new draft";

fn object(properties: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": properties, "required": required, "additionalProperties": false })
}

/// The back-office tools over `site`, in catalogue order (spec 3.5 §7.10).
///
/// # Example
///
/// ```rust,ignore
/// let tools = adk_harness::website::back_office_tools(&site);
/// assert!(tools.iter().any(|tool| tool.name() == "site_publish"));
/// ```
pub fn back_office_tools(site: &Arc<SiteRuntime>) -> Vec<Arc<dyn Tool>> {
    let entries: Vec<(&str, &'static str, Value)> = vec![
        (
            catalog::CONTENT_DRAFT,
            "Drafts a content item on a draft branch and checks it; pass `draft` to add it to an earlier draft, so one release carries several pages. Nothing goes live until it is published.",
            content_schema(),
        ),
        (
            catalog::CONTENT_EDIT,
            "Turns an existing item into a draft with changed fields or body.",
            object(
                json!({
                    "id": { "type": "string" }, "fields": { "type": "object" }, "body": { "type": "string" },
                    "sources": { "type": "array", "items": { "type": "string" } }, "job": { "type": "string" },
                    "draft": { "type": "string", "description": JOIN_DRAFT }
                }),
                &["id"],
            ),
        ),
        (
            catalog::TRANSLATION_DRAFT,
            "Drafts an item in another of the site's languages.",
            object(
                json!({
                    "id": { "type": "string" }, "language": { "type": "string" }, "fields": { "type": "object" },
                    "body": { "type": "string" }, "job": { "type": "string" },
                    "draft": { "type": "string", "description": JOIN_DRAFT }
                }),
                &["id", "language"],
            ),
        ),
        (
            catalog::MEDIA_INTAKE,
            "Takes an upload through the media pipeline and attaches it to a draft. Alt text stays a draft until a person confirms it.",
            object(
                json!({
                    "draft": { "type": "string" }, "name": { "type": "string" }, "dataBase64": { "type": "string" },
                    "alt": { "type": "string" }, "generated": { "type": "boolean" },
                    "item": { "type": "string", "description": "The id of the draft's item the upload belongs to; its first item by default" }
                }),
                &["draft", "name", "dataBase64"],
            ),
        ),
        (
            catalog::CHARTER_DRAFT,
            "Drafts the next charter version and reports what keeps it from being committed.",
            object(
                json!({
                    "charter": { "type": "object", "description": "The charter in the AWP 2.0 format" }
                }),
                &["charter"],
            ),
        ),
        (
            catalog::RELEASE_PREVIEW,
            "Builds a draft's release and publishes it to the host's preview, with page diffs.",
            object(json!({ "draft": { "type": "string" } }), &["draft"]),
        ),
        (
            catalog::SITE_DISCOVER,
            "Reads a page of an existing site, profile or listing, and records what it states as unconfirmed facts.",
            object(
                json!({
                    "url": { "type": "string" }, "facts": { "type": "array", "items": { "type": "string" } }
                }),
                &["url"],
            ),
        ),
        (
            catalog::INTERVIEW_RECORD,
            "Records the owner's answer as a fact, quoting the owner's own words; it is confirmed only when the quote is theirs.",
            object(
                json!({
                    "text": { "type": "string" }, "quote": { "type": "string", "description": "The owner's words, verbatim" },
                    "kind": { "type": "string", "enum": ["project_fact", "preference", "procedure", "outcome", "decision", "correction"] }
                }),
                &["text", "quote"],
            ),
        ),
        (
            catalog::CONTENT_SCHEDULE,
            "Schedules a draft: publication, embargo, expiry or reversal.",
            {
                let mut schema = schedule_schema();
                schema["properties"]["draft"] = json!({ "type": "string" });
                schema["required"] = json!(["draft"]);
                schema
            },
        ),
        (
            catalog::CONTENT_CORRECT,
            "Publishes a correction of an item with a visible correction note.",
            object(
                json!({
                    "id": { "type": "string" }, "note": { "type": "string" }, "fields": { "type": "object" }, "body": { "type": "string" }
                }),
                &["id", "note"],
            ),
        ),
        (
            catalog::CONTENT_WITHDRAW,
            "Withdraws an item: a notice replaces it, or it answers 410 Gone.",
            object(
                json!({
                    "id": { "type": "string" }, "notice": { "type": "string" }
                }),
                &["id"],
            ),
        ),
        (
            catalog::CONTENT_REVERT,
            "Reverts one item to the version an earlier release served, keeping every other change.",
            object(
                json!({
                    "id": { "type": "string" }, "toRelease": { "type": "integer", "minimum": 1 }
                }),
                &["id"],
            ),
        ),
        (
            catalog::SITE_PUBLISH,
            "Publishes a draft as a signed release: preview, approval, release, publication.",
            object(json!({ "draft": { "type": "string" } }), &["draft"]),
        ),
        (
            catalog::SITE_ROLLBACK,
            "Rolls the whole site back to an earlier signed release.",
            object(json!({ "toRelease": { "type": "integer", "minimum": 1 } }), &["toRelease"]),
        ),
        (
            catalog::HOST_FETCH,
            "Reads what the host serves at a path.",
            object(json!({ "path": { "type": "string" } }), &["path"]),
        ),
        (
            catalog::LAUNCH_GATE_RUN,
            "Runs the launch gate's full checks over the site as it would publish now.",
            object(json!({}), &[]),
        ),
        (
            catalog::AWP_VERIFY,
            "Verifies a served site from its published documents: log checkpoint, release inclusion, sampled pages.",
            object(json!({ "origin": { "type": "string" } }), &[]),
        ),
        (
            catalog::CHARTER_COMMIT_REQUEST,
            "Asks every required signer to sign a drafted charter. It commits nothing: the charter takes effect only when they sign.",
            object(json!({ "draft": { "type": "string" } }), &["draft"]),
        ),
        (
            catalog::JOB_PROMOTION_REQUEST,
            "Asks the owner to promote a job to the next autonomy level. Only the owner's decision promotes it.",
            object(
                json!({
                    "job": { "type": "string" }, "reason": { "type": "string" }
                }),
                &["job", "reason"],
            ),
        ),
    ];
    entries
        .into_iter()
        .filter_map(|(name, description, schema)| {
            let spec = catalog::spec(name)?;
            Some(Arc::new(BackOfficeTool { spec, description, schema, site: site.clone() })
                as Arc<dyn Tool>)
        })
        .collect()
}

#[async_trait]
impl Tool for BackOfficeTool {
    fn name(&self) -> &str {
        self.spec.name
    }

    fn description(&self) -> &str {
        self.description
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(self.schema.clone())
    }

    fn origin(&self) -> &str {
        BACK_OFFICE_ORIGIN
    }

    fn effect(&self) -> ToolEffect {
        self.spec.effect
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let site = &self.site;
        let name = self.spec.name;
        match name {
            catalog::CONTENT_DRAFT => {
                let args: DraftArgs = parse(name, args)?;
                let kind = kind_of(&args.kind)?;
                let language = language_of(&args.language)?;
                let item = ContentItem {
                    id: ContentId(format!("{}/{}/{}", kind_directory(kind), args.slug, language)),
                    kind,
                    slug: args.slug,
                    language,
                    fields: args.fields,
                    volatile: args.volatile,
                    body: args.body.map(Markdown),
                    media: Vec::new(),
                    sources: args.sources.into_iter().map(FactRef).collect(),
                    schedule: args.schedule.and_then(ScheduleArgs::into_schedule),
                    state: ContentState::Draft,
                };
                draft(site, &ctx, item, args.job, args.draft).await
            }
            catalog::CONTENT_EDIT => {
                let args: EditArgs = parse(name, args)?;
                let id = ContentId(args.id);
                let mut item = site
                    .store
                    .get(&id)
                    .await
                    .map_err(site_error)?
                    .ok_or_else(|| invalid(name, format!("no item '{id}'")))?;
                item.fields.extend(args.fields);
                if let Some(body) = args.body {
                    item.body = Some(Markdown(body));
                }
                if let Some(sources) = args.sources {
                    item.sources = sources.into_iter().map(FactRef).collect();
                }
                item.state = ContentState::Draft;
                draft(site, &ctx, item, args.job, args.draft).await
            }
            catalog::TRANSLATION_DRAFT => {
                let args: TranslationArgs = parse(name, args)?;
                let source_id = ContentId(args.id);
                let source = site
                    .store
                    .get(&source_id)
                    .await
                    .map_err(site_error)?
                    .ok_or_else(|| invalid(name, format!("no item '{source_id}'")))?;
                let language = language_of(&args.language)?;
                let mut item = source.clone();
                item.id = ContentId(format!(
                    "{}/{}/{}",
                    kind_directory(source.kind),
                    source.slug,
                    language
                ));
                item.language = language;
                item.fields.extend(args.fields);
                item.body = args.body.map(Markdown).or(source.body);
                item.state = ContentState::Draft;
                draft(site, &ctx, item, args.job, args.draft).await
            }
            catalog::MEDIA_INTAKE => {
                let args: MediaArgs = parse(name, args)?;
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(args.data_base64.as_bytes())
                    .map_err(|e| invalid(name, format!("dataBase64: {e}")))?;
                let processed = site
                    .media
                    .process(&MediaUpload {
                        name: &args.name,
                        bytes: &bytes,
                        alt: args.alt,
                        generated: args.generated,
                    })
                    .map_err(site_error)?;
                let mut drafts = site.drafts.lock().await;
                let record = drafts
                    .get_mut(&args.draft)
                    .ok_or_else(|| invalid(name, format!("no draft '{}'", args.draft)))?;
                let item = match &args.item {
                    Some(id) => record.draft.change.items.iter_mut().find(|item| item.id.0 == *id),
                    None => record.draft.change.items.first_mut(),
                }
                .ok_or_else(|| invalid(name, "the draft holds no such item"))?;
                let dir = format!("content/{}/{}", kind_directory(item.kind), item.slug);
                let media = processed.media_ref(&dir);
                item.media.retain(|held| held.path != media.path);
                item.media.push(media.clone());
                record.draft.change.media.push((media.path.clone(), processed.bytes.clone()));
                for variant in &processed.variants {
                    record
                        .draft
                        .change
                        .media
                        .push((format!("{dir}/{}", variant.name), variant.bytes.clone()));
                }
                record.preview = None;
                Ok(json!({
                    "draft": args.draft,
                    "media": media,
                    "altConfirmed": media.alt_confirmed,
                    "note": "alt text is a draft until a person confirms it; the accessibility check fails until then",
                }))
            }
            catalog::CHARTER_DRAFT => {
                let args: CharterDraftArgs = parse(name, args)?;
                let mut charter: Charter =
                    serde_json::from_value(args.charter).map_err(|e| invalid(name, e))?;
                charter.site_id = site.site.id;
                charter.signed = None;
                let violations = match charter.validate() {
                    Ok(()) => Vec::new(),
                    Err(error) => error.violations.iter().map(|v| format!("{v:?}")).collect(),
                };
                let id = site.next_id("charter");
                site.charter_drafts.lock().await.insert(id.clone(), charter);
                Ok(json!({ "draft": id, "valid": violations.is_empty(), "violations": violations }))
            }
            catalog::RELEASE_PREVIEW => {
                let args: DraftRef = parse(name, args)?;
                let mut drafts = site.drafts.lock().await;
                let record = drafts
                    .get_mut(&args.draft)
                    .ok_or_else(|| invalid(name, format!("no draft '{}'", args.draft)))?;
                let preview = site.pipeline.preview(&record.draft).await.map_err(site_error)?;
                let out = json!({
                    "draft": args.draft,
                    "preview": preview.staged.digest,
                    "release": preview.staged.manifest.seq,
                    "gate": { "passed": preview.gate.passed(), "summary": preview.gate.summary() },
                    "diff": preview.diff,
                    "onPreviewHost": preview.report.is_some(),
                });
                record.preview = Some(preview);
                Ok(out)
            }
            catalog::SITE_DISCOVER => {
                let args: DiscoverArgs = parse(name, args)?;
                super::onboarding::discover(site, &ctx, &args.url, &args.facts).await
            }
            catalog::INTERVIEW_RECORD => {
                let args: InterviewArgs = parse(name, args)?;
                super::onboarding::record_answer(
                    site,
                    &ctx,
                    &args.text,
                    &args.quote,
                    args.kind.as_deref(),
                )
                .await
            }
            catalog::CONTENT_SCHEDULE => {
                let args: ScheduleDraftArgs = parse(name, args)?;
                let schedule = ScheduleArgs {
                    publish_at: args.publish_at,
                    embargo_until: args.embargo_until,
                    expires_at: args.expires_at,
                    revert_at: args.revert_at,
                }
                .into_schedule()
                .ok_or_else(|| invalid(name, "a schedule needs at least one time"))?;
                {
                    let mut drafts = site.drafts.lock().await;
                    let record = drafts
                        .get_mut(&args.draft)
                        .ok_or_else(|| invalid(name, format!("no draft '{}'", args.draft)))?;
                    for item in &mut record.draft.change.items {
                        item.schedule = Some(schedule.clone());
                    }
                    record.preview = None;
                }
                publish(site, &args.draft).await
            }
            catalog::SITE_PUBLISH => {
                let args: DraftRef = parse(name, args)?;
                publish(site, &args.draft).await
            }
            catalog::CONTENT_CORRECT => {
                let args: CorrectArgs = parse(name, args)?;
                let (fields, body) = (args.fields, args.body);
                let outcome = site
                    .pipeline
                    .correct(
                        &ContentId(args.id),
                        move |item| {
                            item.fields.extend(fields);
                            if let Some(body) = body {
                                item.body = Some(Markdown(body));
                            }
                        },
                        &args.note,
                        &proposer(&ctx),
                        entry_point(&ctx),
                    )
                    .await
                    .map_err(site_error)?;
                Ok(outcome_json(&outcome))
            }
            catalog::CONTENT_WITHDRAW => {
                let args: WithdrawArgs = parse(name, args)?;
                let how = match args.notice {
                    Some(text) => Withdrawal::Notice(text),
                    None => Withdrawal::Gone,
                };
                let outcome = site
                    .pipeline
                    .withdraw(&ContentId(args.id), how, &proposer(&ctx), entry_point(&ctx))
                    .await
                    .map_err(site_error)?;
                Ok(outcome_json(&outcome))
            }
            catalog::CONTENT_REVERT => {
                let args: RevertArgs = parse(name, args)?;
                let outcome = site
                    .pipeline
                    .revert_item(
                        &ContentId(args.id),
                        args.to_release,
                        &proposer(&ctx),
                        entry_point(&ctx),
                    )
                    .await
                    .map_err(site_error)?;
                Ok(outcome_json(&outcome))
            }
            catalog::SITE_ROLLBACK => {
                let args: RollbackArgs = parse(name, args)?;
                let published =
                    site.pipeline.rollback(args.to_release).await.map_err(site_error)?;
                Ok(published_json(&published))
            }
            catalog::HOST_FETCH => {
                let args: HostFetchArgs = parse(name, args)?;
                let bytes = site.host.fetch(&args.path).await.map_err(site_error)?;
                Ok(match bytes {
                    None => json!({ "path": args.path, "found": false }),
                    Some(bytes) => json!({
                        "path": args.path,
                        "found": true,
                        "bytes": bytes.len(),
                        "sha256": adk_site::release::file_digest(&bytes),
                    }),
                })
            }
            catalog::LAUNCH_GATE_RUN => {
                let report = site.run_launch_gate().await?;
                Ok(
                    json!({ "passed": report.passed(), "summary": report.summary(), "report": report }),
                )
            }
            catalog::AWP_VERIFY => {
                let args: VerifyArgs = parse(name, args)?;
                let verifier = site.verifier.as_ref().ok_or_else(|| {
                    AdkError::config(
                        "awp_verify needs a verifier on the site runtime (spec 3.5 §7.7)",
                    )
                })?;
                let origin = args.origin.unwrap_or_else(|| site.site.origin.clone());
                let report = verifier.verify(&origin).await?;
                Ok(json!({ "passed": report.passed(), "report": report }))
            }
            catalog::CHARTER_COMMIT_REQUEST => {
                let args: DraftRef = parse(name, args)?;
                let draft =
                    site.charter_drafts.lock().await.get(&args.draft).cloned().ok_or_else(
                        || invalid(name, format!("no charter draft '{}'", args.draft)),
                    )?;
                if let Err(error) = draft.validate() {
                    return Err(invalid(name, format!("the draft cannot be committed: {error}")));
                }
                let governing = match &site.charters {
                    Some(charters) => charters
                        .current(site.site.id)
                        .await
                        .map_err(kernel_error)?
                        .map(|current| current.charter.governance),
                    None => None,
                }
                .unwrap_or_else(|| draft.governance.clone());
                let roles = governing
                    .approval_chains
                    .iter()
                    .find(|chain| chain.action == adk_kernel::charter::CHARTER_ACTION)
                    .map(|chain| chain.roles.clone())
                    .unwrap_or_else(|| vec![Role::Owner]);
                let request = CharterSignRequest {
                    id: site.next_id("charter-sign"),
                    draft,
                    roles: roles.clone(),
                    signatures: Vec::new(),
                    requested_by: proposer(&ctx),
                    requested_at: site.clock.now(),
                    committed: None,
                };
                let id = request.id.clone();
                site.requests.charters.lock().await.insert(id.clone(), request);
                Ok(json!({
                    "request": id,
                    "signers": roles,
                    "committed": false,
                    "note": "the charter takes effect only when every required signer has signed it",
                }))
            }
            catalog::JOB_PROMOTION_REQUEST => {
                let args: PromotionArgs = parse(name, args)?;
                let request = PromotionRequest {
                    id: site.next_id("promotion"),
                    job: args.job,
                    reason: args.reason,
                    requested_by: proposer(&ctx),
                    requested_at: site.clock.now(),
                };
                let id = request.id.clone();
                site.requests.promotions.lock().await.insert(id.clone(), request);
                Ok(json!({
                    "request": id,
                    "promoted": false,
                    "note": "only the owner's decision promotes a job",
                }))
            }
            other => Err(invalid(other, "not a back-office tool")),
        }
    }
}

/// Step 1 and 2: holds the change apart and runs the fast tier on it. With `join`, the item
/// joins that draft — replacing an earlier version of the same item — so one release
/// carries several items (spec 3.5 §7.8.2 step 1, §7.18 stage 6).
async fn draft(
    site: &Arc<SiteRuntime>,
    ctx: &Arc<dyn ToolContext>,
    item: ContentItem,
    job: Option<String>,
    join: Option<String>,
) -> Result<Value> {
    let change = Change {
        items: vec![item],
        media: Vec::new(),
        entry: entry_point(ctx),
        proposer: proposer(ctx),
        job: job.unwrap_or_else(|| "content".to_string()),
        action_class: PUBLISH_ACTION.to_string(),
    };
    // The pipeline checks and records the new item alone, so a joined draft's earlier items
    // are not recorded again.
    let single: Draft = site.pipeline.draft(change).await.map_err(site_error)?;
    let mut drafts = site.drafts.lock().await;
    let (joined, candidate) = match join {
        Some(id) => {
            let mut held = drafts
                .get(&id)
                .map(|record| record.draft.clone())
                .ok_or_else(|| invalid("content_draft", format!("no draft '{id}'")))?;
            for item in single.change.items {
                match held.change.items.iter_mut().find(|known| known.id == item.id) {
                    Some(known) => *known = item,
                    None => {
                        held.ids.insert(item.id.clone());
                        held.change.items.push(item);
                    }
                }
            }
            (Some(id), held)
        }
        None => (None, single),
    };
    let ids: Vec<String> = candidate.change.items.iter().map(|item| item.id.0.clone()).collect();
    let gate = site.pipeline.check(&candidate).await.map_err(site_error)?;
    let id = joined.unwrap_or_else(|| site.next_id("draft"));
    drafts.insert(id.clone(), DraftRecord { draft: candidate, preview: None });
    Ok(json!({
        "draft": id,
        "items": ids,
        "gate": { "passed": gate.passed(), "summary": gate.summary() },
    }))
}

/// Steps 3 to 8 for a held draft: preview, approval, release, publication.
async fn publish(site: &Arc<SiteRuntime>, id: &str) -> Result<Value> {
    let mut drafts = site.drafts.lock().await;
    let record =
        drafts.get_mut(id).ok_or_else(|| invalid("site_publish", format!("no draft '{id}'")))?;
    if record.preview.is_none() {
        record.preview = Some(site.pipeline.preview(&record.draft).await.map_err(site_error)?);
    }
    let preview = record.preview.as_ref().ok_or_else(|| AdkError::tool("no preview"))?;
    if preview.gate.refuses_release() {
        return Err(site_error(adk_site::SiteError::GateFailed(preview.gate.summary())));
    }
    if !preview.gate.passed() {
        return Ok(
            json!({ "published": false, "held": "launch gate", "summary": preview.gate.summary() }),
        );
    }
    match site.pipeline.approve(&record.draft, preview).await.map_err(site_error)? {
        ApprovalDecision::Approved { decision, approvers } => {
            let outcome =
                site.pipeline.complete(&record.draft, decision).await.map_err(site_error)?;
            let _ = tokio::fs::remove_dir_all(&preview.staged.root).await;
            drafts.remove(id);
            let mut out = outcome_json(&outcome);
            out["approvers"] = json!(approvers);
            Ok(out)
        }
        ApprovalDecision::Pending { ticket } => Ok(json!({
            "published": false,
            "pending": ticket,
            "note": "the release waits for every role of its approval chain",
        })),
        ApprovalDecision::Rejected { by, reason } => {
            drafts.remove(id);
            Ok(json!({ "published": false, "rejected": { "by": by, "reason": reason } }))
        }
    }
}

fn published_json(published: &Published) -> Value {
    json!({
        "published": true,
        "release": published.staged.manifest.seq,
        "manifest": published.staged.digest,
        "uploaded": published.report.uploaded.len(),
        "pages": published.announcement.pages,
    })
}

fn outcome_json(outcome: &PipelineOutcome) -> Value {
    match outcome {
        PipelineOutcome::Published(published) => published_json(published),
        PipelineOutcome::Held(report) => {
            json!({ "published": false, "held": "launch gate", "summary": report.summary() })
        }
        PipelineOutcome::Pending { ticket, .. } => json!({ "published": false, "pending": ticket }),
        PipelineOutcome::Rejected { by, reason } => {
            json!({ "published": false, "rejected": { "by": by, "reason": reason } })
        }
        PipelineOutcome::Scheduled { at } => json!({ "published": false, "scheduled": at }),
    }
}
