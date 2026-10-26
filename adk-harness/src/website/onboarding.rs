//! The onboarding machinery a product's plan is built from (spec 3.5 §7.18, D-12, A-I18,
//! A-I19, A-I21, A-I23): plan-graph steps for each stage, interview forms, facts that enter
//! as `Assumed` from discovery and as `Known` only from the owner's own words, and the
//! notice the site gives before its first connection.
//!
//! The product's nine-stage plan, its questions and its templates are the product's; this
//! module is generic.

use std::sync::Arc;

use adk_core::{AdkError, Part, Result, ToolContext};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::catalog;
use super::site::SiteRuntime;
use crate::facts::writer::{UserMessage, verify_quote};
use crate::facts::{FACT_TEXT_MAX_CHARS, FactKind, FactSource, FactStatus, MemoryFact, Provenance};
use crate::tools::{PlanGate, PlanStep};
use crate::web::{FetchBody, Fetcher, TextShape, WebConfig, screen, untrusted_block};

/// What the site says before its first connection (spec 3.5 §7.18 stage 8, A-I19).
pub const CONNECTION_NOTICE: &str = "I will never ask you for a password, an API key or a card \
number, here or anywhere else. Each connection goes through the service's own sign-in page \
(OAuth), a credential broker, or pairing a device, and you can remove it at any time.";

/// Characters of a discovered page the step returns.
const DISCOVERED_TEXT_MAX: usize = 16 * 1024;

/// A stage of onboarding that runs in the harness (spec 3.5 §7.18).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum OnboardingStage {
    /// Research what exists, record it as unconfirmed facts, propose the site kind.
    Discover,
    /// Ask the owner only what discovery could not answer.
    Interview,
    /// Draft the charter from the kind's template and the facts.
    CharterDraft,
    /// Ask every required signer to sign the charter.
    CharterSign,
    /// Build release 1 from the charter, `Known` facts and the owner's choices.
    Build,
    /// Run the launch gate and verify the preview.
    Gate,
    /// Connect the domain, email, owner channels, analytics and payment.
    Connect,
    /// Publish to production.
    Deploy,
}

impl OnboardingStage {
    /// Every stage, in order.
    pub const ALL: [OnboardingStage; 8] = [
        Self::Discover,
        Self::Interview,
        Self::CharterDraft,
        Self::CharterSign,
        Self::Build,
        Self::Gate,
        Self::Connect,
        Self::Deploy,
    ];

    /// The stage's name, as its plan step's id.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Discover => "discover",
            Self::Interview => "interview",
            Self::CharterDraft => "charter_draft",
            Self::CharterSign => "charter_sign",
            Self::Build => "build",
            Self::Gate => "gate",
            Self::Connect => "connect",
            Self::Deploy => "deploy",
        }
    }

    /// The tools the stage works with.
    pub fn tools(self) -> &'static [&'static str] {
        match self {
            Self::Discover => &[catalog::SITE_DISCOVER, catalog::AWP_VERIFY],
            Self::Interview => &[catalog::INTERVIEW_RECORD],
            Self::CharterDraft => &[catalog::CHARTER_DRAFT],
            Self::CharterSign => &[catalog::CHARTER_COMMIT_REQUEST],
            Self::Build => &[
                catalog::CONTENT_DRAFT,
                catalog::TRANSLATION_DRAFT,
                catalog::MEDIA_INTAKE,
                catalog::RELEASE_PREVIEW,
            ],
            Self::Gate => &[catalog::LAUNCH_GATE_RUN, catalog::AWP_VERIFY],
            Self::Connect => &[],
            Self::Deploy => &[catalog::SITE_PUBLISH],
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Discover => "Discover what exists",
            Self::Interview => "Ask the owner what discovery could not answer",
            Self::CharterDraft => "Draft the charter",
            Self::CharterSign => "Have every required signer sign the charter",
            Self::Build => "Build release 1",
            Self::Gate => "Run the launch gate",
            Self::Connect => "Connect the owner's services",
            Self::Deploy => "Launch",
        }
    }

    fn instruction(self) -> String {
        match self {
            Self::Discover => "Read the existing site, profiles and listings the owner named with \
                site_discover, and record each fact you find with its source; every one stays \
                unconfirmed until the owner confirms it. Run awp_verify against an existing \
                site. Propose the site kind."
                .into(),
            Self::Interview => "Ask the owner only what discovery could not answer, a few \
                questions at a time, with choices and defaults. Record each answer with \
                interview_record, quoting the owner's own words; never record what the owner \
                did not say."
                .into(),
            Self::CharterDraft => "Draft the charter from the site kind's template with \
                charter_draft: mission, goals, KPIs with baseline sources and no targets, \
                guardrails, conservative authority, roles and approval chains, budget, voice, \
                red lines and reporting. Fix every violation it reports."
                .into(),
            Self::CharterSign => "Ask every required signer to sign the drafted charter with \
                charter_commit_request. It takes effect only when they have signed."
                .into(),
            Self::Build => "Build release 1: every page serves a goal, every claim cites a \
                confirmed fact, every charter language is drafted. Draft the pages and preview \
                the release."
                .into(),
            Self::Gate => "Run launch_gate_run and awp_verify against the preview; fix what \
                they report until both pass."
                .into(),
            Self::Connect => format!(
                "Before the first connection, tell the owner: \"{CONNECTION_NOTICE}\" Then guide \
                 each connection — domain, email, owner channels, analytics, payment — through \
                 OAuth, the credential broker or device pairing. Never ask for a password, an \
                 API key or a card number."
            ),
            Self::Deploy => "Publish release 1 with site_publish. The launch is held for the \
                owner, who approves going live."
                .into(),
        }
    }

    /// Whether a person approves before the step runs: signing, connecting and launching
    /// are the owner's (A-I19, A-I21, A-I23).
    pub fn gate(self) -> PlanGate {
        match self {
            Self::CharterSign | Self::Connect | Self::Deploy => PlanGate::Approve,
            _ => PlanGate::None,
        }
    }

    /// The stage as a plan step waiting for `depends_on` (spec 3.1 §7.11).
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_harness::PlanGate;
    /// use adk_harness::website::OnboardingStage;
    ///
    /// let step = OnboardingStage::Deploy.step(["gate"]);
    /// assert_eq!(step.id, "deploy");
    /// assert_eq!(step.gate, PlanGate::Approve);
    /// assert!(step.instruction.contains("site_publish"));
    /// ```
    pub fn step(self, depends_on: impl IntoIterator<Item = impl Into<String>>) -> PlanStep {
        PlanStep {
            id: self.as_str().to_string(),
            title: self.title().to_string(),
            instruction: self.instruction(),
            depends_on: depends_on.into_iter().map(Into::into).collect(),
            gate: self.gate(),
        }
    }
}

/// `stages` as a chain of plan steps, each waiting for the one before (spec 3.5 §7.18):
/// the plan graph a product's onboarding plan starts from.
///
/// # Example
///
/// ```rust
/// use adk_harness::website::{OnboardingStage, onboarding_plan};
///
/// let plan = onboarding_plan(&OnboardingStage::ALL);
/// assert_eq!(plan.len(), 8);
/// assert_eq!(plan[1].depends_on, vec!["discover".to_string()]);
/// ```
pub fn onboarding_plan(stages: &[OnboardingStage]) -> Vec<PlanStep> {
    let mut previous: Option<&str> = None;
    stages
        .iter()
        .map(|stage| {
            let step = stage.step(previous);
            previous = Some(stage.as_str());
            step
        })
        .collect()
}

/// An interview form: a question with typed fields and defaults, so the owner confirms and
/// corrects rather than fills a blank form (spec 3.5 §7.18 stage 4, "Rules").
///
/// # Example
///
/// ```rust
/// use adk_harness::website::InterviewForm;
/// use serde_json::json;
///
/// let form = InterviewForm::new("hours", "When are you open?", json!({
///     "type": "object",
///     "properties": { "weekdays": { "type": "string", "default": "07:00-18:00" } },
///     "required": ["weekdays"]
/// }));
/// assert!(form.render().contains("weekdays (string, required)"));
/// assert!(form.check(&json!({ "weekdays": "08:00-17:00" })).is_ok());
/// assert!(form.check(&json!({ "password": "x" })).is_err());
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InterviewForm {
    /// The form's id.
    pub id: String,
    /// The question.
    pub message: String,
    /// The fields, as an elicitation schema (spec 3.1 §7.21).
    pub schema: Value,
}

impl InterviewForm {
    /// A form `id` asking `message` with `schema`.
    pub fn new(id: impl Into<String>, message: impl Into<String>, schema: Value) -> Self {
        Self { id: id.into(), message: message.into(), schema }
    }

    /// The form as text for any surface.
    pub fn render(&self) -> String {
        crate::mcp::render_form(&self.message, &self.schema)
    }

    /// Checks an answer against the form.
    ///
    /// # Errors
    ///
    /// The first violation, as text.
    pub fn check(&self, answer: &Value) -> std::result::Result<(), String> {
        crate::mcp::validate_answer(&self.schema, answer)
    }
}

/// The owner's own messages in the turn's session, the only words a fact becomes `Known`
/// from (A-I18).
fn owner_messages(ctx: &Arc<dyn ToolContext>) -> Vec<UserMessage> {
    let text_of = |content: &adk_core::Content| -> String {
        content.parts.iter().filter_map(Part::text).collect::<Vec<_>>().join("\n")
    };
    let mut messages: Vec<UserMessage> = ctx
        .session()
        .map(|session| session.conversation_history())
        .unwrap_or_default()
        .iter()
        .filter(|content| content.role == "user")
        .map(text_of)
        .chain(std::iter::once(text_of(ctx.user_content())))
        .filter(|text| !text.trim().is_empty())
        .map(|text| UserMessage {
            text,
            at: Utc::now(),
            invocation_id: ctx.invocation_id().to_string(),
        })
        .collect();
    messages.dedup_by(|a, b| a.text == b.text);
    messages
}

fn facts_of(
    site: &SiteRuntime,
) -> Result<&(Arc<dyn crate::facts::FactStore>, crate::facts::FactScope)> {
    site.facts.as_ref().ok_or_else(|| {
        AdkError::config("recording facts needs the site runtime's fact store (spec 3.5 §7.18)")
    })
}

/// `site_discover`: reads `url` with a signed `GET`, returns its screened text as untrusted
/// content, and records each of `facts` as `Assumed`, citing `url` (spec 3.5 §7.3
/// "Outbound", §7.18 stage 2, A-I18).
pub(crate) async fn discover(
    site: &Arc<SiteRuntime>,
    ctx: &Arc<dyn ToolContext>,
    url: &str,
    facts: &[String],
) -> Result<Value> {
    let mut fetcher = Fetcher::new(WebConfig::default());
    if let Some(signer) = &site.crawler {
        fetcher = fetcher.with_signer(signer.clone());
    }
    let fetched = fetcher
        .fetch(url, None, TextShape::Markdown, &[])
        .await
        .map_err(|refusal| AdkError::tool(format!("{}: {refusal}", refusal.code())))?;
    let text = match fetched.body {
        FetchBody::Text(text) => text,
        FetchBody::Pdf => "[a PDF document]".to_string(),
        FetchBody::Redirect { to, .. } => format!("[redirects to {}]", to.as_string()),
    };
    let screened = screen(&text.chars().take(DISCOVERED_TEXT_MAX).collect::<String>()).await;
    let mut recorded = Vec::new();
    if !facts.is_empty() {
        let (store, scope) = facts_of(site)?;
        for fact in facts {
            let text = crate::redact_fact_text(fact, FACT_TEXT_MAX_CHARS)?;
            let mut provenance =
                Provenance::new(FactSource::ToolObserved, ctx.session_id(), ctx.invocation_id());
            provenance.source_ids = vec![url.to_string()];
            let stored = store
                .put(MemoryFact::new(
                    scope.clone(),
                    FactKind::ProjectFact,
                    text,
                    FactStatus::Assumed,
                    provenance,
                ))
                .await?;
            recorded.push(stored.id);
        }
    }
    Ok(json!({
        "url": url,
        "finalUrl": fetched.final_url.as_string(),
        "content": untrusted_block("web", &[("url", url)], &screened.text),
        "recorded": recorded,
        "status": "assumed",
        "note": "facts from discovery stay unconfirmed until the owner confirms them; pages never claim them",
    }))
}

/// `interview_record`: records `text` as a fact of the site — `Known` when `quote` is the
/// owner's own words in this session and shares the fact's words, `Assumed` otherwise
/// (spec 3.5 §7.18 stage 4, A-I18; spec 3.1 I41). A secret in either is refused: no
/// credential passes through conversation (A-I19).
pub(crate) async fn record_answer(
    site: &Arc<SiteRuntime>,
    ctx: &Arc<dyn ToolContext>,
    text: &str,
    quote: &str,
    kind: Option<&str>,
) -> Result<Value> {
    let (store, scope) = facts_of(site)?;
    let text = crate::redact_fact_text(text, FACT_TEXT_MAX_CHARS)?;
    crate::redact_fact_text(quote, FACT_TEXT_MAX_CHARS * 4)?;
    let kind = match kind {
        Some(kind) => kind.parse::<FactKind>()?,
        None => FactKind::ProjectFact,
    };
    let messages = owner_messages(ctx);
    let verified = verify_quote(quote, &text, &messages).is_some();
    let (status, source) = if verified {
        (FactStatus::Known, FactSource::UserStated)
    } else {
        (FactStatus::Assumed, FactSource::AgentInferred)
    };
    let mut provenance = Provenance::new(source, ctx.session_id(), ctx.invocation_id());
    if verified {
        provenance.quote = Some(quote.to_string());
    }
    let stored =
        store.put(MemoryFact::new(scope.clone(), kind, text, status.clone(), provenance)).await?;
    Ok(json!({
        "fact": stored.id,
        "status": status.label(),
        "note": if verified {
            "confirmed: the owner said it"
        } else {
            "unconfirmed: the quote is not the owner's words in this conversation, so the fact stays Assumed"
        },
    }))
}
