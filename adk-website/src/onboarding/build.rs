//! Building release 1 (spec 3.5 §7.18 stage 6, D-12, A-I18): a structure where every page
//! serves a goal of the charter and names its KPIs, content from the owner's `Known` facts
//! only, the legal pages the launch gate requires, every charter language; and the live
//! preview that rebuilds after each interview answer (stage 4).
//!
//! | Page | Goal | Content |
//! |---|---|---|
//! | Home (`/`) | the charter's first goal | the mission in the owner's words and links to the goal pages |
//! | About | `trust` | the owner's `Known` facts, each cited as the page's sources |
//! | One page per goal | that goal | what the page is for; the capability that serves it |
//! | Privacy, terms, consent, and the kind's policies | `trust` | the charter's public terms |
//!
//! Nothing here states a fact the owner has not confirmed: the only claims are the `Known`
//! facts, cited so the launch gate checks them (A-I18).

use std::collections::BTreeSet;
use std::sync::Arc;

use adk_core::{AdkError, Result};
use adk_harness::website::SiteRuntime;
use adk_harness::{FactFilter, FactScope, FactStatus, FactStore};
use adk_site::content::kind_directory;
use adk_site::gate::GateReport;
use adk_site::pipeline::{Change, EntryPoint};
use awp_types::{
    Charter, ContentId, ContentItem, ContentKind, ContentState, FactRef, LanguageTag, Markdown,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

fn site_error(error: adk_site::SiteError) -> AdkError {
    AdkError::config(error.to_string())
}

/// The trust goal every kind's template carries (spec 3.5 §7.16.1).
pub const TRUST_GOAL: &str = "trust";

/// A fact the owner confirmed: the only kind a page may claim (A-I18).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnownFact {
    /// The fact's id, cited in a page's sources.
    pub id: String,
    /// The fact.
    pub text: String,
}

/// The owner's `Known` facts in `scope` (spec 3.5 §7.18, A-I18).
///
/// # Errors
///
/// Returns the fact store's error.
pub async fn known_facts(store: &dyn FactStore, scope: &FactScope) -> Result<Vec<KnownFact>> {
    Ok(store
        .list(FactFilter::recall(scope))
        .await?
        .into_iter()
        .filter(|fact| fact.status == FactStatus::Known)
        .map(|fact| KnownFact { id: fact.id, text: fact.text })
        .collect())
}

/// What a page is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageRole {
    /// The home page.
    Home,
    /// About the site and its owner, from `Known` facts.
    About,
    /// The page serving one goal.
    Goal,
    /// A legal page.
    Legal,
}

/// One planned page (spec 3.5 §7.18 stage 6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PagePlan {
    /// Its slug.
    pub slug: String,
    /// Its title.
    pub title: String,
    /// One line on what it is for.
    pub summary: String,
    /// The charter goal it serves.
    pub goal: String,
    /// The KPIs that measure it.
    pub kpis: Vec<String>,
    /// What it is.
    pub role: PageRole,
    /// The legal page's kind: `privacy`, `terms`, `consent` or a policy's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legal: Option<String>,
}

/// The page a goal is served by, by goal id: slug, title, summary. No slug is a content
/// kind's listing path, such as `/notices/` or `/events/`.
const GOAL_PAGES: &[(&str, &str, &str, &str)] = &[
    ("audience", "newsletter", "Newsletter", "New writing, by email, when you ask for it."),
    ("enquiries", "contact", "Contact", "How to reach us."),
    ("bookings", "book", "Book", "Book a time with us."),
    ("reputation", "reviews", "Reviews", "What people say, with their permission."),
    ("sales", "shop", "Shop", "Everything we sell, at its published price."),
    ("service", "help", "Help", "Orders, delivery and returns."),
    ("services", "apply", "Apply online", "Apply for services online."),
    ("accuracy", "announcements", "Announcements", "Official notices in force."),
    ("donations", "donate", "Donate", "Support our work."),
    ("volunteers", "volunteer", "Volunteer", "Give your time."),
    ("events", "whats-on", "What is on", "Events coming up."),
    ("attendance", "whats-on", "What is on", "Events coming up."),
    ("admissions", "admissions", "Admissions", "Enquire or book an open day."),
    ("information", "prospectus", "Prospectus", "Ask for a prospectus."),
    ("readers", "subscribe", "Subscribe", "Read more of our journalism."),
    ("corrections", "corrections", "Corrections", "What we corrected, and when."),
    ("appointments", "appointments", "Appointments", "Book an appointment."),
    ("registrations", "register", "Register", "Register for the event."),
    ("attendees", "questions", "Questions", "Answers for attendees."),
];

/// Release 1's structure (spec 3.5 §7.18 stage 6): every page serves a goal of the charter.
///
/// # Example
///
/// ```rust
/// use adk_harness::website::SiteRef;
/// use adk_website::kinds::{CharterInputs, template};
/// use adk_website::onboarding::build::SitePlan;
/// use awp_types::SiteKind;
///
/// let id = uuid::Uuid::new_v4();
/// let business = template(SiteKind::Business);
/// let team = adk_website::team::SiteTeam::new(SiteRef::new(id, "bakery", "https://bakery.example"), &business);
/// let inputs = CharterInputs::new(id, "Juniper Bakery", "amina", "Fresh bread every morning.", vec!["en".parse().unwrap()], "KES");
/// let charter = business.draft_charter(&team, &inputs);
/// let plan = SitePlan::new(&charter, "Juniper Bakery");
/// let goals: Vec<&str> = charter.goals.iter().map(|g| g.id.as_str()).collect();
/// assert!(plan.pages.iter().all(|page| goals.contains(&page.goal.as_str())));
/// assert!(plan.pages.iter().any(|page| page.legal.as_deref() == Some("privacy")));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SitePlan {
    /// The pages.
    pub pages: Vec<PagePlan>,
}

impl SitePlan {
    /// The structure for `charter`.
    pub fn new(charter: &Charter, site_name: &str) -> Self {
        let kpis_of = |goal: &str| -> Vec<String> {
            charter.goals.iter().find(|g| g.id == goal).map(|g| g.kpis.clone()).unwrap_or_default()
        };
        let trust = charter
            .goals
            .iter()
            .find(|goal| goal.id == TRUST_GOAL)
            .or_else(|| charter.goals.last())
            .map(|goal| goal.id.clone())
            .unwrap_or_default();
        let mut pages = Vec::new();
        if let Some(first) = charter.goals.first() {
            pages.push(PagePlan {
                slug: "home".into(),
                title: site_name.to_string(),
                summary: charter.mission.clone(),
                goal: first.id.clone(),
                kpis: first.kpis.clone(),
                role: PageRole::Home,
                legal: None,
            });
        }
        pages.push(PagePlan {
            slug: "about".into(),
            title: format!("About {site_name}"),
            summary: format!("Who is behind {site_name}."),
            goal: trust.clone(),
            kpis: kpis_of(&trust),
            role: PageRole::About,
            legal: None,
        });
        let mut slugs: BTreeSet<String> = ["home".to_string(), "about".to_string()].into();
        for goal in charter.goals.iter().filter(|goal| goal.id != trust) {
            let (slug, title, summary) = GOAL_PAGES
                .iter()
                .find(|(id, ..)| *id == goal.id)
                .map(|(_, slug, title, summary)| {
                    ((*slug).to_string(), (*title).to_string(), (*summary).to_string())
                })
                .unwrap_or_else(|| (goal.id.clone(), goal.name.clone(), goal.name.clone()));
            if slugs.insert(slug.clone()) {
                pages.push(PagePlan {
                    slug,
                    title,
                    summary,
                    goal: goal.id.clone(),
                    kpis: goal.kpis.clone(),
                    role: PageRole::Goal,
                    legal: None,
                });
            }
        }
        let mut legal: Vec<(String, String)> = vec![
            ("privacy".into(), "Privacy".into()),
            ("terms".into(), "Terms".into()),
            ("consent".into(), "Consent".into()),
        ];
        for policy in &charter.public.policies {
            if !legal.iter().any(|(name, _)| *name == policy.name) {
                legal.push((policy.name.clone(), policy.name.replace('-', " ")));
            }
        }
        for (name, title) in legal {
            if slugs.insert(name.clone()) {
                pages.push(PagePlan {
                    summary: format!("{title} at {site_name}."),
                    slug: name.clone(),
                    title,
                    goal: trust.clone(),
                    kpis: kpis_of(&trust),
                    role: PageRole::Legal,
                    legal: Some(name),
                });
            }
        }
        Self { pages }
    }

    /// The pages whose goal the charter does not have (spec 3.5 S1: every page maps to a
    /// charter goal).
    pub fn unmapped(&self, charter: &Charter) -> Vec<&PagePlan> {
        self.pages.iter().filter(|page| !charter.goals.iter().any(|g| g.id == page.goal)).collect()
    }

    /// The content items of the plan in every charter language (spec 3.5 §7.18 stage 6): the
    /// About page claims only `facts` and cites them; the legal pages carry the charter's
    /// public terms and cover `jurisdictions`. Pages in a language after the first wait for
    /// their translation (`translation: pending`).
    pub fn items(
        &self,
        charter: &Charter,
        facts: &[KnownFact],
        jurisdictions: &[String],
    ) -> Vec<ContentItem> {
        let languages: Vec<LanguageTag> = if charter.voice.languages.is_empty() {
            vec![LanguageTag::parse("en").expect("a valid tag")]
        } else {
            charter.voice.languages.clone()
        };
        let contact =
            charter.public.contact.email.clone().unwrap_or_else(|| "the contact page".into());
        let links: Vec<String> = self
            .pages
            .iter()
            .filter(|page| page.role == PageRole::Goal)
            .map(|page| format!("- [{}](/{}/)", page.title, page.slug))
            .collect();
        let mut items = Vec::new();
        for (index, language) in languages.iter().enumerate() {
            for page in &self.pages {
                let (body, sources) = match page.role {
                    PageRole::Home => {
                        (format!("{}\n\n{}\n", charter.mission, links.join("\n")), Vec::new())
                    }
                    PageRole::About if facts.is_empty() => {
                        ("More about us soon.\n".to_string(), Vec::new())
                    }
                    PageRole::About => (
                        facts.iter().map(|fact| format!("- {}\n", fact.text)).collect(),
                        facts.iter().map(|fact| FactRef(fact.id.clone())).collect(),
                    ),
                    PageRole::Goal => (
                        format!(
                            "{}\n\nAsk the site's assistant, or use the form on this page.\n",
                            page.summary
                        ),
                        Vec::new(),
                    ),
                    PageRole::Legal => (legal_text(page, charter, &contact), Vec::new()),
                };
                let mut fields = serde_json::Map::new();
                fields.insert("title".into(), json!(page.title));
                fields.insert("summary".into(), json!(page.summary));
                fields.insert("goal".into(), json!(page.goal));
                fields.insert("kpis".into(), json!(page.kpis));
                if let Some(legal) = &page.legal {
                    fields.insert("legal".into(), json!(legal));
                    if !jurisdictions.is_empty() {
                        fields.insert("jurisdictions".into(), json!(jurisdictions));
                    }
                }
                if index > 0 {
                    fields.insert("translation".into(), json!("pending"));
                }
                items.push(ContentItem {
                    id: ContentId(format!(
                        "{}/{}/{}",
                        kind_directory(ContentKind::Page),
                        page.slug,
                        language
                    )),
                    kind: ContentKind::Page,
                    slug: page.slug.clone(),
                    language: language.clone(),
                    fields: fields.into_iter().collect(),
                    volatile: Vec::new(),
                    body: Some(Markdown(body)),
                    media: Vec::new(),
                    sources,
                    schedule: None,
                    state: ContentState::Draft,
                });
            }
        }
        items
    }
}

fn legal_text(page: &PagePlan, charter: &Charter, contact: &str) -> String {
    let scopes: Vec<&str> = charter.public.consent_scopes.iter().map(|s| s.as_str()).collect();
    match page.legal.as_deref() {
        Some("privacy") => format!(
            "We collect only what you give us, for the purpose you give it for. We use no \
             advertising trackers and set no cookies to measure visits. To see, correct or delete \
             what we hold about you, write to {contact}.\n"
        ),
        Some("consent") => format!(
            "We ask for your consent before we contact you or keep your details, one purpose at a \
             time{}. Each consent comes with a receipt, and you can withdraw it at any time; \
             withdrawing stops every pending message.\n",
            if scopes.is_empty() { String::new() } else { format!(" ({})", scopes.join(", ")) }
        ),
        Some("terms") => {
            "Use this site and its services as described on each page. Messages written by the \
             site's assistant say so.\n"
                .to_string()
        }
        Some("corrections") => {
            "When we get something wrong we correct it in the open: the page shows a correction \
             note, and the site's signed log keeps both versions.\n"
                .to_string()
        }
        Some("records-retention") => {
            "Superseded notices and records are kept for the period the law requires.\n".to_string()
        }
        _ => format!("{}\n", page.summary),
    }
}

/// What staging release 1 did (spec 3.5 §7.18 stage 6).
#[derive(Debug, Clone)]
pub struct StagedBuild {
    /// The items written to the content repository's working copy.
    pub written: Vec<ContentId>,
    /// The items the per-change checks held, with why.
    pub held: Vec<ContentId>,
    /// The per-change checks' verdict.
    pub gate: GateReport,
}

/// Stages release 1 in the content repository (spec 3.5 §7.18 stage 6, §7.8.2 steps 1–2):
/// the items enter as one draft by the Steward, pass the per-change checks — claims from
/// `Known` facts only, links, accessibility, languages, red lines, structured data — and
/// those that pass join the working copy the launch gate checks and release 1 publishes. An
/// item the checks hold stays out.
///
/// # Errors
///
/// Returns the pipeline's or the store's error.
#[tracing::instrument(skip_all, name = "website.onboarding.build", fields(site.id = %runtime.site().id, items = items.len()))]
pub async fn stage_release_one(
    runtime: &SiteRuntime,
    items: Vec<ContentItem>,
    proposer: &str,
) -> Result<StagedBuild> {
    let change = Change {
        items,
        media: Vec::new(),
        entry: EntryPoint::SiteAgent { agent: proposer.to_string() },
        proposer: proposer.to_string(),
        job: crate::jobs::ONBOARDING.to_string(),
        action_class: "publish".to_string(),
    };
    let draft = runtime.pipeline().draft(change).await.map_err(site_error)?;
    let gate = runtime.pipeline().check(&draft).await.map_err(site_error)?;
    let held = gate.held_items();
    let mut written = Vec::new();
    for item in &draft.change.items {
        if held.contains(&item.id) {
            continue;
        }
        let mut item = item.clone();
        item.state = ContentState::Published;
        runtime.store().put(&item).await.map_err(site_error)?;
        written.push(item.id);
    }
    Ok(StagedBuild { written, held: held.into_iter().collect(), gate })
}

/// One rebuild of the live preview (spec 3.5 §7.18 stage 4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewSnapshot {
    /// The `Known` facts the preview shows.
    pub facts: usize,
    /// The preview release's digest.
    pub digest: String,
    /// Whether the per-change checks passed, so the host's preview shows it.
    pub published: bool,
    /// The About page's Markdown twin as the preview built it.
    pub about: Option<String>,
}

/// The live preview: after each interview answer the About page is rebuilt from the
/// owner's `Known` facts and published to the host's preview (spec 3.5 §7.18 stage 4). The
/// rest of release 1 waits for the charter its pages serve.
pub struct LivePreview {
    runtime: Arc<SiteRuntime>,
    facts: Arc<dyn FactStore>,
    scope: FactScope,
    site_name: String,
    proposer: String,
}

impl LivePreview {
    /// The preview of `runtime`'s site, reading `scope`'s facts in `facts`, built as
    /// `proposer` (the Steward).
    pub fn new(
        runtime: Arc<SiteRuntime>,
        facts: Arc<dyn FactStore>,
        scope: FactScope,
        site_name: impl Into<String>,
        proposer: impl Into<String>,
    ) -> Self {
        Self { runtime, facts, scope, site_name: site_name.into(), proposer: proposer.into() }
    }

    /// Rebuilds the preview from the facts as they stand now.
    ///
    /// # Errors
    ///
    /// Returns the fact store's or the pipeline's error.
    #[tracing::instrument(skip_all, name = "website.onboarding.interview")]
    pub async fn refresh(&self, charter: &Charter) -> Result<PreviewSnapshot> {
        let facts = known_facts(self.facts.as_ref(), &self.scope).await?;
        let plan = SitePlan::new(charter, &self.site_name);
        let language = charter.voice.languages.first().map(LanguageTag::as_str).unwrap_or("en");
        let items: Vec<ContentItem> = plan
            .items(charter, &facts, &[])
            .into_iter()
            .filter(|item| item.language.as_str() == language && item.slug == "about")
            .collect();
        let change = Change {
            items,
            media: Vec::new(),
            entry: EntryPoint::SiteAgent { agent: self.proposer.clone() },
            proposer: self.proposer.clone(),
            job: crate::jobs::ONBOARDING.to_string(),
            action_class: "publish".to_string(),
        };
        let pipeline = self.runtime.pipeline();
        let draft = pipeline.draft(change).await.map_err(site_error)?;
        let preview = pipeline.preview(&draft).await.map_err(site_error)?;
        let about =
            tokio::fs::read_to_string(preview.staged.root.join("about/index.md")).await.ok();
        let _ = tokio::fs::remove_dir_all(&preview.staged.root).await;
        Ok(PreviewSnapshot {
            facts: facts.len(),
            digest: preview.staged.digest.to_string(),
            published: preview.report.is_some(),
            about,
        })
    }
}

/// The value of a page's `goal` field.
pub fn page_goal(item: &ContentItem) -> Option<&str> {
    item.fields.get("goal").and_then(Value::as_str)
}
